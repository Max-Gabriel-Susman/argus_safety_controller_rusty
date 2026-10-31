//! The replay transport, the one-shot fetch and the stream:
//! `argus_replay.c`'s socket and streaming, and `main.c`'s network check
//! before it.
//!
//! A UDP socket bound to port 5011 sends the client's requests to the relay
//! at 192.168.1.20:5010 and hands every datagram that comes back to the
//! client. Once the link is up and the host has been reached, one half
//! (offset 0, 147 samples) is fetched into a DDR buffer and the `replay ok:`
//! (or `replay FAILED`) and `replay: req=` lines are printed, as `main.c`
//! does before it starts streaming. Then the task streams for good: the
//! state machine in [`argus_fw_core::stream`] primes both BRAM halves,
//! switches the chain to the external source and refills whichever half
//! the PL has consumed, the chunks landing straight in the BRAM.
//!
//! What changed from the C, and why:
//!
//! - The C runs the fetch inline in `main()`, spinning on the stack until
//!   the fetch is done. Here it is a task, so telemetry keeps running.
//! - The C's receive callback copies a datagram out of its pbuf as lwIP
//!   delivers it. Here the task waits on the socket with a 1 ms timeout and
//!   calls the client's `poll` after every datagram and every timeout, so
//!   the deadline is checked at least once a millisecond.
//! - The C sends its first request whatever ARP knows; lwIP queues it, and
//!   the host has long been up. Here the first request waits until
//!   telemetry has had a frame leave for the same host (see
//!   [`telemetry::HOST_REACHED`]). Otherwise the request can sit in the
//!   socket for the second smoltcp waits between ARP requests, the 200 ms
//!   deadline passes, and the fetch shows a timeout and a retransmit that
//!   say nothing about the relay.
//! - The C's stream is serviced from the superloop, and its report is
//!   printed from there too. Here the stream belongs to this task, which
//!   hands the telemetry task the report when asked (see
//!   [`stream_report`]), so the lines still come between `tx` and `feat:`.

use core::task::{Context, Poll, Waker};

use argus_fw_core::acq::ReplayStatus;
use argus_fw_core::acq::bram::HALF_BYTES;
use argus_fw_core::lines::{INITIALIZED, REPLAY_FAILED, REPLAY_NEVER_SENT, STREAM_PRIMING};
use argus_fw_core::replay::{self, LOCAL_PORT, RELAY_ADDRESS, SAMPLES_PER_HALF};
use argus_fw_core::stream::{Clock, Fetch, Half, Pl, ReportInputs, RxStats, Stream, StreamReport};
use argus_replay::{Client, FetchError, State, Transport};
use argus_wire::generated::{
    ARGUS_REPLAY_MAX_PAYLOAD, ARGUS_REPLAY_PORT, argus_replay_chunk_hdr_t,
};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use static_cell::ConstStaticCell;

use crate::acq::{Acq, SharedAcq};
use crate::bram::ReplayBram;
use crate::console::{self, console};
use crate::telemetry;

/// The longest datagram the client can accept: a chunk header and a full
/// payload, 1472 bytes, the size of the C's `g_rx`. Anything longer is
/// dropped by the socket, as the C drops a pbuf that does not fit `g_rx`.
const MAX_DATAGRAM: usize =
    size_of::<argus_replay_chunk_hdr_t>() + ARGUS_REPLAY_MAX_PAYLOAD as usize;

/// Datagrams the receive buffer holds. A burst is 21 chunks; a late burst
/// for a superseded request can be queued ahead of the answer to its
/// retransmit, so there is room for two, and a few more.
const RX_DATAGRAMS: usize = 48;

/// Payload bytes the receive buffer holds: two bursts of 21 full chunks
/// (21 x 1400 bytes each). smoltcp keeps each datagram contiguous, so the
/// ring can waste up to one datagram where it wraps; 64 KB covers that.
const RX_BYTES: usize = 64 * 1024;

/// Requests the transmit buffer holds. One is outstanding at a time; the
/// spare slots cover a retransmit queued while ARP is still pending.
const TX_REQUESTS: usize = 4;

/// One request, 22 bytes.
const REQUEST_BYTES: usize = size_of::<argus_wire::generated::argus_replay_request_t>();

/// How long the task waits for a datagram before it lets the client check
/// its deadline.
const SERVICE_PERIOD: Duration = Duration::from_millis(1);

/// `REPLAY_SEND_ATTEMPTS` and `REPLAY_SEND_GAP_US`: how often, and how far
/// apart, the one-shot fetch is tried while its request cannot be handed
/// to the stack.
const SEND_ATTEMPTS: u32 = 200;
const SEND_GAP: Duration = Duration::from_millis(10);

static RX_META: ConstStaticCell<[PacketMetadata; RX_DATAGRAMS]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; RX_DATAGRAMS]);
static RX_BUFFER: ConstStaticCell<[u8; RX_BYTES]> = ConstStaticCell::new([0; RX_BYTES]);
static TX_META: ConstStaticCell<[PacketMetadata; TX_REQUESTS]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; TX_REQUESTS]);
static TX_BUFFER: ConstStaticCell<[u8; TX_REQUESTS * REQUEST_BYTES]> =
    ConstStaticCell::new([0; TX_REQUESTS * REQUEST_BYTES]);

/// The C's `g_fetch_buf`: one half in DDR, for the one-shot fetch only.
/// Streaming fetches land in the BRAM directly.
static FETCH_BUFFER: ConstStaticCell<[u8; HALF_BYTES]> = ConstStaticCell::new([0; HALF_BYTES]);

/// The replay socket, bound to port 5011 as `argus_replay_init()` binds
/// `g_pcb`. `None` when called a second time or when the port is taken.
pub fn socket(stack: Stack<'static>) -> Option<UdpSocket<'static>> {
    let mut socket = UdpSocket::new(
        stack,
        RX_META.try_take()?,
        RX_BUFFER.try_take()?,
        TX_META.try_take()?,
        TX_BUFFER.try_take()?,
    );
    socket.bind(LOCAL_PORT).ok()?;
    Some(socket)
}

/// The relay, 192.168.1.20:5010.
fn relay() -> IpEndpoint {
    let [a, b, c, d] = RELAY_ADDRESS;
    // The port is 5010; the fallback only keeps the code free of a panic.
    let port = u16::try_from(ARGUS_REPLAY_PORT).unwrap_or(5010);
    IpEndpoint::new(IpAddress::v4(a, b, c, d), port)
}

/// The C's `argus_now_ms()`: milliseconds since boot, wrapping at 2^32 as
/// its `uint32_t` does (the client handles the wrap).
fn now_ms() -> u32 {
    let ms = Instant::now().as_millis();
    u32::try_from(ms & u64::from(u32::MAX)).unwrap_or(0)
}

/// A request the stack would not take: its buffer was full, or the
/// destination is unreachable.
#[derive(Debug)]
struct NotSent;

/// The client's way out: `replay_send()`, a datagram to the relay.
struct Relay<'a> {
    socket: &'a UdpSocket<'static>,
}

impl Transport for Relay<'_> {
    type Error = NotSent;

    /// Hands the request to smoltcp without waiting, as `udp_sendto()`
    /// returns at once. The client's calls are synchronous, so the socket
    /// is polled once with a waker that does nothing: a full buffer is a
    /// failed send, which the client answers by retransmitting at its
    /// deadline. Nothing in the firmware awaits this socket's send side, so
    /// the waker it replaces is never one a task depends on.
    fn send(&mut self, datagram: &[u8]) -> Result<(), NotSent> {
        let mut cx = Context::from_waker(Waker::noop());
        match self.socket.poll_send_to(datagram, relay(), &mut cx) {
            Poll::Ready(Ok(())) => Ok(()),
            Poll::Ready(Err(_)) | Poll::Pending => Err(NotSent),
        }
    }
}

/// The C's `argus_now_us()`: microseconds since boot, wrapping at 2^32.
fn now_us() -> u32 {
    let us = Instant::now().as_micros();
    u32::try_from(us & u64::from(u32::MAX)).unwrap_or(0)
}

/// The replay task: the one-shot fetch once the network is ready, then
/// streaming into the BRAM for good.
#[embassy_executor::task]
pub async fn replay(
    stack: Stack<'static>,
    socket: UdpSocket<'static>,
    mut client: Client,
    acq: &'static SharedAcq,
    mut bram: ReplayBram,
) -> ! {
    stack.wait_link_up().await;
    telemetry::HOST_REACHED.wait().await;

    if let Some(half) = FETCH_BUFFER.try_take() {
        one_shot(&socket, &mut client, half).await;
    }

    stream(&socket, &mut client, acq, &mut bram).await
}

/// `main.c`'s one-shot fetch into DDR: proves the host to PS path before
/// anything depends on it.
async fn one_shot(socket: &UdpSocket<'static>, client: &mut Client, half: &mut [u8; HALF_BYTES]) {
    let mut relay = Relay { socket };

    // The C retries the fetch itself while the request will not leave,
    // pumping the stack between tries; here the runner task does the
    // pumping while this one sleeps. A refused send is the only failure a
    // valid fetch can have.
    let mut sent = false;
    for _ in 0..SEND_ATTEMPTS {
        match client.fetch(&mut relay, 0, SAMPLES_PER_HALF, now_ms()) {
            Ok(()) => {
                sent = true;
                break;
            }
            Err(FetchError::Send(NotSent)) => Timer::after(SEND_GAP).await,
            Err(FetchError::NoSamples | FetchError::TooManyChunks) => break,
        }
    }
    if !sent {
        console!("{REPLAY_NEVER_SENT}");
        return;
    }

    let mut rx = [0u8; MAX_DATAGRAM];
    while !client.is_done() {
        // A datagram too long for `rx` is dropped by the socket and comes
        // back as an error; like a timeout, it only means nothing to hand on.
        if let Ok(Ok((len, _from))) = with_timeout(SERVICE_PERIOD, socket.recv_from(&mut rx)).await
            && let Some(datagram) = rx.get(..len)
        {
            client.on_packet(datagram, half.as_mut_slice());
        }
        // A retransmit the stack refused is counted as sent, and retried at
        // the next deadline, as in the C.
        let _ = client.poll(&mut relay, now_ms());
    }

    if client.state() == State::Complete {
        console!("{}", replay::ok_line(half.as_slice()));
    } else {
        console!("{REPLAY_FAILED}");
    }
    console!("{}", replay::counters_line(client));
}

/// Asked for by [`stream_report`], answered by the replay task.
static REPORT_WANTED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The report the replay task answers [`REPORT_WANTED`] with.
static REPORT: Signal<CriticalSectionRawMutex, StreamReport> = Signal::new();

/// How long [`stream_report`] waits for the answer. The replay task looks
/// for the request at least once a millisecond.
const REPORT_WAIT: Duration = Duration::from_millis(5);

/// `argus_replay_stream_report()`, for the telemetry task's status block:
/// asks the replay task for the three `stream:` lines, which starts every
/// timing figure in them again. `None` if the replay task did not answer,
/// as before it streams or without a network.
pub async fn stream_report() -> Option<StreamReport> {
    // An answer left over from a request that timed out is stale.
    REPORT.reset();
    REPORT_WANTED.signal(());
    with_timeout(REPORT_WAIT, REPORT.wait()).await.ok()
}

/// The stream's PL: the replay registers and the BRAM's cache clean.
struct Fabric<'a> {
    acq: &'a mut Acq,
    bram: &'a ReplayBram,
}

impl Pl for Fabric<'_> {
    fn replay_status(&mut self) -> ReplayStatus {
        self.acq.replay_status()
    }

    fn ack_replay(&mut self, bits: u32) {
        self.acq.ack_replay(bits);
    }

    fn write_ctrl(&mut self, value: u32) {
        self.acq.write_ctrl(value);
    }

    fn clean_half(&mut self, half: Half) {
        // Fails only for a range that is not whole cache lines, and the
        // halves are (checked at compile time in argus-fw-core).
        let _ = self.bram.clean(half);
    }
}

/// The stream's fetch: the client, sending through the socket.
struct Fetcher<'a> {
    client: &'a mut Client,
    socket: &'a UdpSocket<'static>,
}

impl Fetch for Fetcher<'_> {
    fn start(&mut self, offset: u32) -> bool {
        let mut relay = Relay {
            socket: self.socket,
        };
        self.client
            .fetch(&mut relay, offset, SAMPLES_PER_HALF, now_ms())
            .is_ok()
    }

    fn state(&self) -> State {
        self.client.state()
    }
}

/// The stream's clock.
struct Micros;

impl Clock for Micros {
    fn now_us(&self) -> u32 {
        now_us()
    }
}

/// Streaming: `argus_replay_stream_begin(0)`, the two lines `main.c` prints
/// after it, then the main loop's replay half for good.
///
/// Each pass waits up to [`SERVICE_PERIOD`] for a datagram, takes every
/// datagram the socket holds and hands each to the client, landing it in
/// the half in flight; then polls the client for its deadline, moves the
/// stream on by a step, and answers a report request. That is the C's
/// superloop order: `xemacif_input()`, then `argus_replay_stream_service()`
/// (whose first act is `argus_replay_service()`).
///
/// The C copies each datagram out of its pbuf in lwIP's receive callback.
/// Here a pass drains the socket without awaiting, which is at most the
/// datagrams that arrived since the last pass; the runner task has to run
/// to put more in, so a burst is taken a few datagrams at a time.
async fn stream(
    socket: &UdpSocket<'static>,
    client: &mut Client,
    acq: &'static SharedAcq,
    bram: &mut ReplayBram,
) -> ! {
    let mut stream = Stream::new();
    let mut rx_stats = RxStats::new();
    let mut rx = [0u8; MAX_DATAGRAM];

    REPORT_WANTED.reset();
    stream.begin(0, &mut Fetcher { client, socket }, &Micros);
    console!("{STREAM_PRIMING}");
    console!("{INITIALIZED}");

    loop {
        // A timeout only means there is nothing to take this time.
        let _ = with_timeout(SERVICE_PERIOD, socket.wait_recv_ready()).await;

        // The socket is readable or the pass is a timeout, so nothing here
        // waits: a datagram is there or the drain ends. The waker this
        // registers is replaced at the next wait.
        let mut cx = Context::from_waker(Waker::noop());
        let mut packets = 0;
        loop {
            let t0 = now_us();
            let Poll::Ready(received) = socket.poll_recv_from(&mut rx, &mut cx) else {
                break;
            };
            let t1 = now_us();
            // A datagram longer than `rx` was dropped by the socket, as the
            // C drops a pbuf that does not fit `g_rx`.
            let Some(datagram) = received.ok().and_then(|(len, _)| rx.get(..len)) else {
                continue;
            };
            // Between fetches the client ignores what arrives, so the
            // empty slice is never written.
            let sink = match stream.in_flight().and_then(|half| bram.half(half)) {
                Some(half) => half.as_mut_slice(),
                None => &mut [],
            };
            client.on_packet(datagram, sink);
            rx_stats.datagram(t1.wrapping_sub(t0), now_us().wrapping_sub(t1));
            packets += 1;
        }
        rx_stats.wakeup(packets);

        // A retransmit the stack refused is counted as sent, and retried at
        // the next deadline, as in the C.
        let _ = client.poll(&mut Relay { socket }, now_ms());

        // Never held across an await: see SharedAcq.
        let mut acq = acq.lock().await;
        let mut fabric = Fabric {
            acq: &mut acq,
            bram,
        };
        stream.service(&mut fabric, &mut Fetcher { client, socket }, &Micros);

        if REPORT_WANTED.try_take().is_some() {
            let inputs = ReportInputs {
                status: acq.replay_status(),
                client: client.counters(),
                con_drops: console::drops(),
                con_max: console::take_max_msg(),
            };
            REPORT.signal(StreamReport::take(&mut stream, &mut rx_stats, &inputs));
        }
    }
}
