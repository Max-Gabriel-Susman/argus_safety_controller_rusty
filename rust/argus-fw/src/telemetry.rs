//! `main.c`'s main loop, the telemetry half: every new feature bin is read
//! under the feature hold and sent to the host as one v3 frame on UDP
//! port 5005, with the `tx` line every 20 bins and, every 100, the `feat:`
//! line and the frame check.
//!
//! Paced by the fabric, not a timer: the task polls FEATURE_INDEX every
//! millisecond and sends when it has moved. A bin is 50 ms, so a new one is
//! seen within 2% of a bin. The C polls on every pass of its superloop.
//!
//! What changed from the C, and why:
//!
//! - The C sends whatever the link is doing. Here a frame built while the
//!   link is down is dropped and counted, not handed to smoltcp, which
//!   would hold it and send stale bins in a burst when the link returned.
//! - The C ignores `udp_sendto()`'s result. Here a frame the socket cannot
//!   take within [`SEND_WAIT`] is dropped and counted too, so a stuck
//!   transmitter never stalls the bin loop.
//! - The frame lives on this task's stack only until smoltcp has copied it
//!   into the socket's buffer, and from there into a GEM transmit buffer,
//!   which the HAL cleans to DDR before the GEM is given it and does not
//!   reuse until the GEM has handed its descriptor back. Nothing the GEM
//!   reads is written while it reads it.

use argus_fw_core::acq::BinCounter;
use argus_fw_core::lines::{Feat, Tx};
use argus_fw_core::telemetry::{self, HOST_ADDRESS, Unsent, UnsentCounter};
use argus_wire::frame::{FramePacket, frame_bytes};
use argus_wire::generated::ARGUS_UDP_PORT;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer, with_timeout};
use static_cell::ConstStaticCell;

use crate::acq::Acq;
use crate::acq_check::frame_check;
use crate::console::console;

/// How often FEATURE_INDEX is looked at. One GP0 read a millisecond costs
/// the executor about 0.1% of its time.
const BIN_POLL: Duration = Duration::from_millis(1);

/// How long a frame may wait for room in the socket's buffer before it is
/// dropped. The buffer holds [`TX_FRAMES`] frames and the stack sends each
/// within microseconds of its arrival, so it is only ever full when the
/// transmitter is stuck; a fifth of a bin is then long enough to tell.
const SEND_WAIT: Duration = Duration::from_millis(10);

/// Frames the socket's transmit buffer holds. One is sent per 50 ms bin;
/// the second covers the bin after a frame that waits for ARP.
const TX_FRAMES: usize = 2;

/// The socket never receives; smoltcp still wants a receive buffer, and
/// one metadata slot with no payload room drops anything sent to it.
const RX_SLOTS: usize = 1;

static TX_META: ConstStaticCell<[PacketMetadata; TX_FRAMES]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; TX_FRAMES]);
static TX_BUFFER: ConstStaticCell<[u8; TX_FRAMES * size_of::<FramePacket>()]> =
    ConstStaticCell::new([0; TX_FRAMES * size_of::<FramePacket>()]);
static RX_META: ConstStaticCell<[PacketMetadata; RX_SLOTS]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; RX_SLOTS]);
static RX_BUFFER: ConstStaticCell<[u8; 0]> = ConstStaticCell::new([]);

/// The telemetry socket: a UDP socket on an ephemeral port, as lwIP binds
/// the C's unbound `g_pcb` on its first `udp_sendto()`. `None` when called
/// a second time or when the stack has no port to give.
pub fn socket(stack: Stack<'static>) -> Option<UdpSocket<'static>> {
    let mut socket = UdpSocket::new(
        stack,
        RX_META.try_take()?,
        RX_BUFFER.try_take()?,
        TX_META.try_take()?,
        TX_BUFFER.try_take()?,
    );
    socket.bind(0).ok()?;
    Some(socket)
}

/// Signalled once, when the first frame has left the board for the host,
/// so the host's MAC address is known. The replay relay is the same host:
/// the replay task waits for this before its first request, so that the
/// request's 200 ms deadline is not spent waiting on ARP.
pub static HOST_REACHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Where the frames go: the host on [`ARGUS_UDP_PORT`].
fn host() -> IpEndpoint {
    let [a, b, c, d] = HOST_ADDRESS;
    // The port is 5005; the fallback only keeps the code free of a panic.
    let port = u16::try_from(ARGUS_UDP_PORT).unwrap_or(5005);
    IpEndpoint::new(IpAddress::v4(a, b, c, d), port)
}

/// The network side of the task: the stack and its socket, or neither when
/// the network did not come up (the bins are still read and reported).
pub struct Link {
    stack: Stack<'static>,
    socket: UdpSocket<'static>,
    /// Set once [`HOST_REACHED`] has been signalled.
    host_reached: bool,
}

impl Link {
    /// The stack and the socket opened on it.
    pub const fn new(stack: Stack<'static>, socket: UdpSocket<'static>) -> Self {
        Self {
            stack,
            socket,
            host_reached: false,
        }
    }

    /// Sends one frame, or says why it was not sent.
    async fn send(&mut self, frame: &FramePacket) -> Result<(), Unsent> {
        if !self.stack.is_link_up() {
            return Err(Unsent::LinkDown);
        }
        // A timeout drops the send future before smoltcp took the frame, so
        // a frame is either queued whole or not at all.
        match with_timeout(SEND_WAIT, self.socket.send_to(frame_bytes(frame), host())).await {
            Ok(Ok(())) => {
                self.note_host_reached().await;
                Ok(())
            }
            Ok(Err(_)) | Err(_) => Err(Unsent::Refused),
        }
    }

    /// Until the host has been reached once, waits up to [`SEND_WAIT`] for
    /// the socket to empty and signals [`HOST_REACHED`] when it does.
    /// smoltcp keeps a datagram in the socket until it knows the host's
    /// MAC address, so an empty socket after a send means ARP has resolved.
    /// Afterwards this costs nothing.
    async fn note_host_reached(&mut self) {
        if self.host_reached {
            return;
        }
        if with_timeout(SEND_WAIT, self.socket.flush()).await.is_ok() {
            self.host_reached = true;
            HOST_REACHED.signal(());
        }
    }
}

/// The main loop's telemetry half (see the module notes). `link` is `None`
/// when there is no network; every frame then counts as unsent.
#[embassy_executor::task]
pub async fn telemetry(mut acq: Acq, mut link: Option<Link>) -> ! {
    let mut bins = BinCounter::new();
    let mut unsent = UnsentCounter::new();
    loop {
        Timer::after(BIN_POLL).await;
        if !bins.is_new(acq.feature_index()) {
            continue;
        }
        // A hold that never took effect is a fabric problem, not a reason
        // to publish a torn bin: skipped, counted, and shown on the tx line.
        let Ok(features) = acq.read_features() else {
            bins.skipped(acq.feature_index());
            continue;
        };

        let due = bins.read(features.bin);

        let frame = telemetry::frame(&features);
        let sent = match &mut link {
            Some(link) => link.send(&frame).await,
            None => Err(Unsent::LinkDown),
        };
        match sent {
            Ok(()) => {
                if let Some(line) = unsent.sent() {
                    console!("{line}");
                }
            }
            Err(why) => unsent.unsent(why),
        }

        if due.tx {
            console!(
                "{}",
                Tx {
                    count: bins.read_count(),
                    bin: features.bin,
                    skipped: bins.skipped_count(),
                }
            );
        }
        if due.status {
            console!("{}", Feat::new(&features, acq.feat_dropped()));
            let ext = acq.ext_mode();
            frame_check(&mut acq, ext);
        }
    }
}
