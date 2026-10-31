//! The client state machine: one fetch at a time, from request to a filled
//! destination.
//!
//! A line-for-line port of `argus_replay_client.h`. The checks run in the
//! same order and every rejection is counted the same way, so the five
//! counters mean what the C status lines say they mean. What differs is the
//! shape: the C init/fetch/on_packet/poll functions are methods, the send
//! callback and the `dst` pointer are traits handed to each call (see
//! `io.rs`), and `on_packet` says which check a chunk failed.

use core::mem::offset_of;

use argus_wire::crc::Crc16;
use argus_wire::generated::{
    ARGUS_FRAME_MAGIC, ARGUS_MSG_REPLAY_CHUNK, ARGUS_MSG_REPLAY_REQUEST, ARGUS_REPLAY_F_RETRANSMIT,
    ARGUS_REPLAY_MAX_PAYLOAD, ARGUS_REPLAY_VERSION, argus_replay_chunk_hdr_t,
    argus_replay_request_t,
};
use zerocopy::{FromBytes, IntoBytes};

use crate::MAX_CHUNKS;
use crate::io::{Sink, Transport};

/// The protocol constants the 16-bit header fields carry. bindgen makes
/// every `#define` a `u32`; these say once, and check at build time, that
/// each fits its field.
const _: () = assert!(ARGUS_REPLAY_VERSION <= 0xFFFF);
const _: () = assert!(ARGUS_MSG_REPLAY_REQUEST <= 0xFFFF);
const _: () = assert!(ARGUS_REPLAY_F_RETRANSMIT <= 0xFFFF);
#[expect(
    clippy::cast_possible_truncation,
    reason = "the asserts above prove each value fits in 16 bits"
)]
mod wire16 {
    use super::{ARGUS_MSG_REPLAY_REQUEST, ARGUS_REPLAY_F_RETRANSMIT, ARGUS_REPLAY_VERSION};
    pub(super) const VERSION: u16 = ARGUS_REPLAY_VERSION as u16;
    pub(super) const MSG_REQUEST: u16 = ARGUS_MSG_REPLAY_REQUEST as u16;
    pub(super) const F_RETRANSMIT: u16 = ARGUS_REPLAY_F_RETRANSMIT as u16;
}

/// Bytes the CRC covers at the front of each header: everything before the
/// `crc` field.
const REQUEST_CRC_SPAN: usize = offset_of!(argus_replay_request_t, crc);
const CHUNK_CRC_SPAN: usize = offset_of!(argus_replay_chunk_hdr_t, crc);

/// Where a fetch stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// No fetch yet.
    Idle,
    /// A request is outstanding.
    Waiting,
    /// Every chunk landed; the destination is filled.
    Complete,
    /// Retries exhausted; the destination is partial.
    Failed,
}

/// Why [`Client::new`] refused a configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Not one sample of this many channels fits in a chunk's payload
    /// (zero channels, or more than 722).
    ChannelCount,
}

/// Why [`Client::fetch`] did not start a fetch, or could not send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError<E> {
    /// Asked for no samples. Nothing changed.
    NoSamples,
    /// The burst would need more than [`MAX_CHUNKS`] chunks. Nothing changed.
    TooManyChunks,
    /// The request was built and the fetch started, but the transport could
    /// not send it. As in the C, the client is now waiting and will
    /// retransmit when the deadline passes.
    Send(E),
}

/// What [`Client::on_packet`] did with a datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet {
    /// No fetch is outstanding, so the datagram was dropped uncounted.
    Ignored,
    /// The chunk was written to the sink.
    Accepted,
    /// The chunk failed a check and was counted in `chunks_rejected`.
    Rejected(Reject),
}

/// The check a rejected chunk failed, in the order they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Shorter than a chunk header.
    Short,
    /// Wrong magic, version or message type.
    NotAChunk,
    /// Answers a superseded request. Expected after a retransmit.
    StaleSeq,
    /// Carries a different channel count from ours.
    ChannelCount,
    /// Its burst length is not the length we asked for.
    ChunkTotal,
    /// Its index is not below its burst length.
    ChunkIndex,
    /// The datagram is not exactly the header plus `payload_len` bytes.
    Length,
    /// `payload_len` is not `sample_count` rows of samples.
    PayloadLen,
    /// The CRC over the header and payload does not match.
    Crc,
    /// The index, mapped into the original burst, is past its end.
    ChunkRange,
    /// That chunk has already landed.
    Duplicate,
    /// Not the number of samples a chunk at this index carries.
    SampleCount,
    /// The payload would reach past the end of the destination.
    Overflow,
}

/// The status counters. They count from [`Client::new`] and wrap, as the C's
/// `uint32_t`s do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// First requests, one per fetch.
    pub requests_sent: u32,
    /// Requests for a gap after a timeout.
    pub retransmits_sent: u32,
    /// Chunks written to the destination.
    pub chunks_accepted: u32,
    /// Datagrams that failed a check while a fetch was outstanding.
    pub chunks_rejected: u32,
    /// Deadlines that passed, including the one that ends in `Failed`.
    pub timeouts: u32,
}

/// The replay client: configuration, the fetch in progress, and the
/// counters. Owns no memory but its own fields.
#[derive(Clone, Debug)]
pub struct Client {
    // configuration
    channel_count: u16,
    /// Derived from `channel_count`: as many rows as fit the payload.
    samples_per_chunk: u16,
    timeout_ms: u32,
    max_retries: u8,

    // current fetch
    state: State,
    seq: u32,
    base_offset: u32,
    total_samples: u16,
    total_chunks: u16,
    /// Bitmap over the original burst.
    arrived: u32,
    arrived_count: u16,
    deadline_ms: u32,
    retries: u8,

    // Retransmits restart chunk_index at zero, so responses carry an offset
    // into the original burst rather than an absolute position.
    req_first_chunk: u16,
    req_chunks: u16,

    counters: Counters,
}

/// The bit for chunk `index` in the arrival set. Every caller has
/// `index < total_chunks <= 32`; past that the bit is simply absent.
fn bit(index: u16) -> u32 {
    1u32.checked_shl(u32::from(index)).unwrap_or(0)
}

impl Client {
    /// A client for `channel_count` channels that waits `timeout_ms` for a
    /// burst and retransmits at most `max_retries` times per fetch.
    ///
    /// # Errors
    ///
    /// [`ConfigError::ChannelCount`] if no sample fits in a chunk.
    pub fn new(channel_count: u16, timeout_ms: u32, max_retries: u8) -> Result<Self, ConfigError> {
        let samples_per_chunk = chunk_capacity(channel_count);
        if samples_per_chunk == 0 {
            return Err(ConfigError::ChannelCount);
        }
        Ok(Self {
            channel_count,
            samples_per_chunk,
            timeout_ms,
            max_retries,
            state: State::Idle,
            seq: 0,
            base_offset: 0,
            total_samples: 0,
            total_chunks: 0,
            arrived: 0,
            arrived_count: 0,
            deadline_ms: 0,
            retries: 0,
            req_first_chunk: 0,
            req_chunks: 0,
            counters: Counters::default(),
        })
    }

    /// Where the current fetch stands.
    pub fn state(&self) -> State {
        self.state
    }

    /// True once the fetch has ended, either way.
    pub fn is_done(&self) -> bool {
        matches!(self.state, State::Complete | State::Failed)
    }

    /// The status counters.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Chunks of the current fetch that have landed.
    pub fn arrived_count(&self) -> u16 {
        self.arrived_count
    }

    /// Chunks in the current fetch's burst.
    pub fn total_chunks(&self) -> u16 {
        self.total_chunks
    }

    /// The sequence number of the request outstanding: every chunk of its
    /// answer must echo it.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Samples per chunk at this channel count: 7 at 96 channels.
    pub fn samples_per_chunk(&self) -> u16 {
        self.samples_per_chunk
    }

    /// Begins a fetch of `sample_count` samples from `sample_offset` and
    /// sends the request. Any fetch in progress is abandoned: the new seq
    /// makes its late chunks stale.
    ///
    /// The sink handed to [`on_packet`](Self::on_packet) for this fetch must
    /// hold `sample_count * channel_count` samples; on the PS that is one
    /// half of the ping-pong BRAM, written through directly.
    ///
    /// # Errors
    ///
    /// [`FetchError::NoSamples`] or [`FetchError::TooManyChunks`] if the
    /// fetch cannot start; [`FetchError::Send`] if it started but the
    /// request did not go out.
    pub fn fetch<T: Transport + ?Sized>(
        &mut self,
        transport: &mut T,
        sample_offset: u32,
        sample_count: u16,
        now_ms: u32,
    ) -> Result<(), FetchError<T::Error>> {
        if sample_count == 0 {
            return Err(FetchError::NoSamples);
        }
        let chunks = sample_count.div_ceil(self.samples_per_chunk);
        if chunks > MAX_CHUNKS {
            return Err(FetchError::TooManyChunks);
        }

        self.base_offset = sample_offset;
        self.total_samples = sample_count;
        self.total_chunks = chunks;
        self.arrived = 0;
        self.arrived_count = 0;
        self.retries = 0;
        self.req_first_chunk = 0;
        self.req_chunks = chunks;
        self.seq = self.seq.wrapping_add(1);

        self.send_request(transport, sample_offset, sample_count, 0, now_ms)
            .map_err(FetchError::Send)
    }

    /// Feeds one received datagram, writing its samples to `sink` if it
    /// checks out.
    ///
    /// Malformed, stale and duplicate packets are counted and dropped, never
    /// fatal, because a UDP endpoint sees all three in normal operation.
    pub fn on_packet<S: Sink + ?Sized>(&mut self, data: &[u8], sink: &mut S) -> Packet {
        if self.state != State::Waiting {
            return Packet::Ignored;
        }
        match self.check_and_place(data, sink) {
            Ok(()) => Packet::Accepted,
            Err(reason) => {
                self.counters.chunks_rejected = self.counters.chunks_rejected.wrapping_add(1);
                Packet::Rejected(reason)
            }
        }
    }

    /// Call periodically. When the deadline has passed, reissues the first
    /// contiguous run of missing chunks; gives up after `max_retries`.
    ///
    /// # Errors
    ///
    /// The transport's error if a retransmit could not be sent. The client
    /// counts it as sent and waits out the new deadline regardless, as the
    /// C does, so the next timeout tries again.
    pub fn poll<T: Transport + ?Sized>(
        &mut self,
        transport: &mut T,
        now_ms: u32,
    ) -> Result<(), T::Error> {
        if self.state != State::Waiting {
            return Ok(());
        }
        // Unsigned difference, so a wrapping millisecond counter is handled:
        // the deadline is still ahead while now - deadline, taken mod 2^32,
        // is in the upper half. It has passed from the very millisecond it
        // names.
        if now_ms.wrapping_sub(self.deadline_ms) > 0x7FFF_FFFF {
            return Ok(());
        }

        self.counters.timeouts = self.counters.timeouts.wrapping_add(1);

        if self.retries >= self.max_retries {
            self.state = State::Failed;
            return Ok(());
        }
        self.retries = self.retries.wrapping_add(1);

        let missing = |i: u16| self.arrived & bit(i) == 0;
        let Some(first) = (0..self.total_chunks).find(|&i| missing(i)) else {
            // Nothing missing after all.
            self.state = State::Complete;
            return Ok(());
        };
        let mut last = first;
        while last + 1 < self.total_chunks && missing(last + 1) {
            last += 1;
        }

        let n_chunks = last - first + 1;
        let first_sample = u32::from(first) * u32::from(self.samples_per_chunk);
        let offset = self.base_offset.wrapping_add(first_sample);
        // A run that reaches the end of the burst ends with the short final
        // chunk, so ask for exactly what is left rather than whole chunks.
        let count = if last == self.total_chunks - 1 {
            sample_span(u32::from(self.total_samples) - first_sample)
        } else {
            sample_span(u32::from(n_chunks) * u32::from(self.samples_per_chunk))
        };

        // New seq so late chunks from the previous attempt are dropped
        // rather than landing at the wrong offsets.
        self.seq = self.seq.wrapping_add(1);
        self.req_first_chunk = first;
        self.req_chunks = n_chunks;

        self.send_request(transport, offset, count, wire16::F_RETRANSMIT, now_ms)
    }

    /// Builds the request, arms the deadline, counts it and sends it. The
    /// state changes before the send, so a send fault still leaves the
    /// client waiting for the deadline, as in the C.
    fn send_request<T: Transport + ?Sized>(
        &mut self,
        transport: &mut T,
        offset: u32,
        count: u16,
        flags: u16,
        now_ms: u32,
    ) -> Result<(), T::Error> {
        let mut req = argus_replay_request_t {
            magic: ARGUS_FRAME_MAGIC,
            version: wire16::VERSION,
            type_: wire16::MSG_REQUEST,
            seq: self.seq,
            sample_offset: offset,
            sample_count: count,
            flags,
            crc: 0,
        };
        req.crc = crc_of_prefix(req.as_bytes(), REQUEST_CRC_SPAN).finish();

        self.deadline_ms = now_ms.wrapping_add(self.timeout_ms);
        self.state = State::Waiting;

        if flags & wire16::F_RETRANSMIT != 0 {
            self.counters.retransmits_sent = self.counters.retransmits_sent.wrapping_add(1);
        } else {
            self.counters.requests_sent = self.counters.requests_sent.wrapping_add(1);
        }

        transport.send(req.as_bytes())
    }

    /// The checks of `argus_replay_client_on_packet()`, in its order, then
    /// the copy. Returns the first check that fails.
    fn check_and_place<S: Sink + ?Sized>(
        &mut self,
        data: &[u8],
        sink: &mut S,
    ) -> Result<(), Reject> {
        // read_from_prefix copies the header out, as the C memcpy does: the
        // receive buffer carries no alignment guarantee, and the packed
        // struct needs none.
        let (hdr, payload) =
            argus_replay_chunk_hdr_t::read_from_prefix(data).map_err(|_| Reject::Short)?;

        if hdr.magic != ARGUS_FRAME_MAGIC
            || u32::from(hdr.version) != ARGUS_REPLAY_VERSION
            || u32::from(hdr.type_) != ARGUS_MSG_REPLAY_CHUNK
        {
            return Err(Reject::NotAChunk);
        }
        if hdr.seq != self.seq {
            return Err(Reject::StaleSeq);
        }
        if hdr.channel_count != self.channel_count {
            return Err(Reject::ChannelCount);
        }
        if hdr.chunk_total != self.req_chunks {
            return Err(Reject::ChunkTotal);
        }
        if hdr.chunk_index >= hdr.chunk_total {
            return Err(Reject::ChunkIndex);
        }
        if payload.len() != usize::from(hdr.payload_len) {
            return Err(Reject::Length);
        }
        // The C truncates this product to 16 bits before comparing. Any
        // sample_count that only passes because of that wrap fails the
        // sample count check below, so comparing at full width rejects the
        // same packets.
        let rows = u32::from(hdr.sample_count) * u32::from(self.channel_count) * 2;
        if u32::from(hdr.payload_len) != rows {
            return Err(Reject::PayloadLen);
        }

        let crc = crc_of_prefix(data, CHUNK_CRC_SPAN).update(payload).finish();
        if crc != hdr.crc {
            return Err(Reject::Crc);
        }

        // Both are below MAX_CHUNKS (chunk_index < chunk_total = req_chunks),
        // so the sum cannot overflow.
        let global_chunk = self.req_first_chunk + hdr.chunk_index;
        if global_chunk >= self.total_chunks {
            return Err(Reject::ChunkRange);
        }
        if self.arrived & bit(global_chunk) != 0 {
            return Err(Reject::Duplicate);
        }

        // Every chunk but the last carries a full load. Checking this catches
        // a server that sizes chunks differently from us, which would
        // otherwise scatter samples to the wrong offsets.
        let first_sample = u32::from(global_chunk) * u32::from(self.samples_per_chunk);
        let expect_samples = if global_chunk == self.total_chunks - 1 {
            u32::from(self.total_samples) - first_sample
        } else {
            u32::from(self.samples_per_chunk)
        };
        if u32::from(hdr.sample_count) != expect_samples {
            return Err(Reject::SampleCount);
        }

        // Destination is derived from chunk_index, not sample_offset: when
        // the dataset wraps mid-burst the offsets are not monotonic, but the
        // index always is.
        let row_bytes = usize::from(self.channel_count) * 2;
        let dst_offset =
            usize::from(global_chunk) * usize::from(self.samples_per_chunk) * row_bytes;
        // The C bounds the copy by the fetch's own size. The sink is handed
        // in separately here, so bound it by the sink as well: a sink
        // smaller than the fetch gets a rejected chunk, not a short write.
        let capacity = (usize::from(self.total_samples) * row_bytes).min(sink.capacity());
        if dst_offset + payload.len() > capacity {
            return Err(Reject::Overflow);
        }

        sink.write(dst_offset, payload);

        self.arrived |= bit(global_chunk);
        self.arrived_count += 1;
        self.counters.chunks_accepted = self.counters.chunks_accepted.wrapping_add(1);

        if self.arrived_count == self.total_chunks {
            self.state = State::Complete;
        }
        Ok(())
    }
}

/// Samples of `channel_count` channels that fit one chunk's payload; 0 when
/// none do.
fn chunk_capacity(channel_count: u16) -> u16 {
    if channel_count == 0 {
        return 0;
    }
    let rows = ARGUS_REPLAY_MAX_PAYLOAD / (u32::from(channel_count) * 2);
    // At least one channel, so at most 1444 / 2 = 722 rows: it fits.
    u16::try_from(rows).unwrap_or(0)
}

/// A sample count inside one fetch. Every caller passes at most the fetch's
/// own `u16` total, so this never saturates.
fn sample_span(samples: u32) -> u16 {
    u16::try_from(samples).unwrap_or(u16::MAX)
}

/// The CRC of the first `span` bytes of `bytes`, the part of a header in
/// front of its `crc` field. Callers have already checked that `bytes`
/// holds a whole header.
fn crc_of_prefix(bytes: &[u8], span: usize) -> Crc16 {
    Crc16::new().update(bytes.get(..span).unwrap_or(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_constants_match_the_header() {
        assert_eq!(REQUEST_CRC_SPAN, 20);
        assert_eq!(CHUNK_CRC_SPAN, 26);
        assert_eq!(size_of::<argus_replay_chunk_hdr_t>(), 28);
    }

    #[test]
    fn chunk_capacity_matches_the_c() {
        assert_eq!(chunk_capacity(0), 0);
        assert_eq!(chunk_capacity(1), 722);
        assert_eq!(chunk_capacity(96), 7);
        assert_eq!(chunk_capacity(124), 5);
        assert_eq!(chunk_capacity(722), 1);
        assert_eq!(chunk_capacity(723), 0);
    }
}
