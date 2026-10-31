//! The C side, as Rust sees it: the shim's functions and a safe wrapper.
//!
//! The shim keeps all its state, the C client and its destination buffer
//! included, in one block of memory that Rust allocates and hands to every
//! call. The functions take references to fixed-size arrays rather than
//! pointer and length pairs, which is what lets them be declared `safe`
//! below: no call can pass a length that disagrees with its buffer.

use core::ffi::c_int;

use argus_replay::MAX_CHUNKS;
use argus_wire::frame::CHANNELS;
use argus_wire::generated::ARGUS_REPLAY_MAX_PAYLOAD;

/// Bytes in the destination of a fetch: every chunk of the largest burst
/// full. Both sides write into a buffer of this size, and the tests compare
/// all of it, so a write outside the fetch shows too.
pub const DST_BYTES: usize = MAX_CHUNKS as usize * ARGUS_REPLAY_MAX_PAYLOAD as usize;

/// Bytes of the last request the shim keeps. A request is 22.
pub const SENT_BYTES: usize = 64;

/// The largest datagram [`CClient::on_packet`] passes on whole. A chunk is
/// at most 28 + 1444 bytes.
pub const PACKET_BYTES: usize = 2048;

/// Bytes in a telemetry frame.
pub const FRAME_BYTES: usize = 594;

/// Room for a `shim_t`: the client, the destination and a little more.
/// [`CClient::new`] checks it against the C's `sizeof`.
const BLOCK_BYTES: usize = 48 * 1024;

/// The memory the shim works in. Aligned for the pointers inside the C
/// client struct.
#[repr(C, align(16))]
struct Block([u8; BLOCK_BYTES]);

/// The C client's state after a step, and the last request it sent. The
/// layout is `shim_snapshot_t`'s; the tests build the same struct from the
/// Rust client and compare the two whole.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// 0 idle, 1 waiting, 2 complete, 3 failed.
    pub state: u32,
    /// The sequence number of the request outstanding.
    pub seq: u32,
    /// Chunks of the current fetch that have landed.
    pub arrived_count: u32,
    /// Chunks in the current fetch's burst.
    pub total_chunks: u32,
    /// Samples per chunk at this channel count.
    pub samples_per_chunk: u32,
    /// The five status counters.
    pub requests_sent: u32,
    /// See [`requests_sent`](Self::requests_sent).
    pub retransmits_sent: u32,
    /// See [`requests_sent`](Self::requests_sent).
    pub chunks_accepted: u32,
    /// See [`requests_sent`](Self::requests_sent).
    pub chunks_rejected: u32,
    /// See [`requests_sent`](Self::requests_sent).
    pub timeouts: u32,
    /// Send attempts, failed ones included.
    pub sends: u32,
    /// The length of the last attempt.
    pub sent_len: u32,
    /// Its first [`SENT_BYTES`] bytes, zero past its end.
    pub sent: [u8; SENT_BYTES],
}

/// Written out because `Default` is not derived for arrays longer than 32.
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            state: 0,
            seq: 0,
            arrived_count: 0,
            total_chunks: 0,
            samples_per_chunk: 0,
            requests_sent: 0,
            retransmits_sent: 0,
            chunks_accepted: 0,
            chunks_rejected: 0,
            timeouts: 0,
            sends: 0,
            sent_len: 0,
            sent: [0; SENT_BYTES],
        }
    }
}

// SAFETY: each declaration matches the definition of the same name in
// csrc/shim.c, which build.rs compiles and links into this crate. They are
// `safe` because none can be misused from Rust: every buffer is passed as a
// reference to an array of exactly the size the C reads or writes, the
// block is only ever a `Block`, which `CClient::new` has checked against
// the C's `sizeof(shim_t)` before any other call, and the pointers the
// client keeps into the block are re-pointed at the block on every call.
unsafe extern "C" {
    safe fn shim_bytes() -> usize;
    safe fn shim_init(
        mem: &mut Block,
        size: usize,
        channel_count: u16,
        timeout_ms: u32,
        max_retries: u8,
    ) -> c_int;
    safe fn shim_set_send_fails(mem: &mut Block, fails: c_int);
    safe fn shim_fetch(
        mem: &mut Block,
        sample_offset: u32,
        sample_count: u16,
        now_ms: u32,
    ) -> c_int;
    safe fn shim_on_packet(mem: &mut Block, packet: &[u8; PACKET_BYTES], len: u16);
    safe fn shim_poll(mem: &mut Block, now_ms: u32);
    safe fn shim_snapshot(mem: &mut Block, out: &mut Snapshot);
    safe fn shim_dst_diff(mem: &mut Block, other: &[u8; DST_BYTES]) -> i64;
    safe fn shim_pack_frame(
        sample: u32,
        t: f32,
        ch: &[u16; CHANNELS],
        power: &[u32; CHANNELS],
        out: &mut [u8; FRAME_BYTES],
    );
}

/// What `shim_init` returns when the block does not fit a `shim_t`.
const BLOCK_TOO_SMALL: c_int = -2;

/// The C replay client, `argus_replay_client.h` compiled as the firmware
/// compiles it, with a send callback that records each request.
pub struct CClient {
    /// Boxed so it never moves, and because 48 KB is a lot of stack.
    block: Box<Block>,
}

impl CClient {
    /// `argus_replay_client_init(&cl, send, ctx, channel_count, timeout_ms,
    /// max_retries)`.
    ///
    /// # Errors
    ///
    /// The C's return code when it is not 0: -1 when no sample fits a
    /// chunk, -2 when the shim's block is too small for the C struct (a
    /// fault in this crate, not in either client).
    pub fn new(channel_count: u16, timeout_ms: u32, max_retries: u8) -> Result<Self, c_int> {
        if shim_bytes() > BLOCK_BYTES {
            return Err(BLOCK_TOO_SMALL);
        }
        let mut block = Box::new(Block([0; BLOCK_BYTES]));
        match shim_init(
            &mut block,
            BLOCK_BYTES,
            channel_count,
            timeout_ms,
            max_retries,
        ) {
            0 => Ok(Self { block }),
            rc => Err(rc),
        }
    }

    /// Makes every send from now on report failure, or succeed again. The
    /// request is recorded either way.
    pub fn set_send_fails(&mut self, fails: bool) {
        shim_set_send_fails(&mut self.block, c_int::from(fails));
    }

    /// `argus_replay_client_fetch()` into the shim's destination: 0 if the
    /// request went out, -1 otherwise.
    pub fn fetch(&mut self, sample_offset: u32, sample_count: u16, now_ms: u32) -> c_int {
        shim_fetch(&mut self.block, sample_offset, sample_count, now_ms)
    }

    /// `argus_replay_client_on_packet()`. A datagram longer than
    /// [`PACKET_BYTES`] is cut to that length; the tests never make one.
    pub fn on_packet(&mut self, datagram: &[u8]) {
        let mut packet = [0u8; PACKET_BYTES];
        let len = datagram.len().min(PACKET_BYTES);
        if let (Some(dst), Some(src)) = (packet.get_mut(..len), datagram.get(..len)) {
            dst.copy_from_slice(src);
        }
        shim_on_packet(
            &mut self.block,
            &packet,
            u16::try_from(len).unwrap_or(u16::MAX),
        );
    }

    /// `argus_replay_client_poll()`.
    pub fn poll(&mut self, now_ms: u32) {
        shim_poll(&mut self.block, now_ms);
    }

    /// The client's state and the last request sent.
    pub fn snapshot(&mut self) -> Snapshot {
        let mut out = Snapshot::default();
        shim_snapshot(&mut self.block, &mut out);
        out
    }

    /// The first byte at which the C destination differs from `other`, or
    /// `None` if the two are the same.
    pub fn dst_diff(&mut self, other: &[u8; DST_BYTES]) -> Option<usize> {
        usize::try_from(shim_dst_diff(&mut self.block, other)).ok()
    }
}

/// The frame `argus_send_frame()` in `argus_net.c` would send for these
/// values, packed by a copy of its code.
pub fn pack_frame(
    sample: u32,
    t: f32,
    channels: &[u16; CHANNELS],
    power: &[u32; CHANNELS],
) -> [u8; FRAME_BYTES] {
    let mut out = [0u8; FRAME_BYTES];
    shim_pack_frame(sample, t, channels, power, &mut out);
    out
}
