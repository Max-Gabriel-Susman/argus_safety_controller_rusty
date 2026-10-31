//! A simulated relay: answers a replay request with a burst of chunks, the
//! way argus_sim's relay does.
//!
//! Chunk `i` of a burst carries samples from `sample_offset + i * per_chunk`,
//! indexed from zero, the last one short, each with its CRC. The data is a
//! fixed function of (sample, channel), so a retransmitted chunk carries the
//! same bytes as the original and a misplaced chunk shows in the
//! destination.
//!
//! It can also be set up wrong on purpose, stamping another channel count
//! or cutting chunks smaller than the client expects, to exercise the
//! checks that catch a relay that disagrees with the board.

use argus_wire::crc::Crc16;
use argus_wire::generated::{
    ARGUS_FRAME_MAGIC, ARGUS_MSG_REPLAY_CHUNK, ARGUS_MSG_REPLAY_REQUEST, ARGUS_REPLAY_MAX_PAYLOAD,
    ARGUS_REPLAY_VERSION, argus_replay_chunk_hdr_t, argus_replay_request_t,
};
use zerocopy::{FromBytes, IntoBytes};

/// A relay with a channel count and a chunk size.
#[derive(Clone, Copy, Debug)]
pub struct Relay {
    /// The channel count it stamps on every chunk and sizes rows by.
    pub channel_count: u16,
    /// Samples in every chunk but the last.
    pub samples_per_chunk: u16,
}

impl Relay {
    /// A relay for `channel_count` channels with chunks as large as fit,
    /// which is what the client expects.
    pub fn for_channels(channel_count: u16) -> Self {
        let rows = match channel_count {
            0 => 0,
            n => ARGUS_REPLAY_MAX_PAYLOAD / (u32::from(n) * 2),
        };
        Self {
            channel_count,
            samples_per_chunk: u16::try_from(rows).unwrap_or(0),
        }
    }

    /// The burst that answers `request`, in order; nothing if the request
    /// is not one, or if this relay cannot fit a sample in a chunk.
    pub fn answer(&self, request: &[u8]) -> Vec<Vec<u8>> {
        let Ok(req) = argus_replay_request_t::read_from_bytes(request) else {
            return Vec::new();
        };
        if req.magic != ARGUS_FRAME_MAGIC
            || u32::from(req.type_) != ARGUS_MSG_REPLAY_REQUEST
            || self.samples_per_chunk == 0
        {
            return Vec::new();
        }
        let per_chunk = self.samples_per_chunk;
        let total = req.sample_count.div_ceil(per_chunk);
        (0..total)
            .map(|index| {
                let done = u32::from(index) * u32::from(per_chunk);
                let left = u32::from(req.sample_count) - done;
                let count = u16::try_from(left.min(u32::from(per_chunk))).unwrap_or(per_chunk);
                self.chunk(
                    req.seq,
                    req.sample_offset.wrapping_add(done),
                    index,
                    total,
                    count,
                )
            })
            .collect()
    }

    /// One chunk: `count` samples from `first`, as chunk `index` of `total`
    /// in the answer to request `seq`.
    fn chunk(&self, seq: u32, first: u32, index: u16, total: u16, count: u16) -> Vec<u8> {
        let payload: Vec<u8> = (0..u32::from(count))
            .flat_map(|s| {
                let sample = first.wrapping_add(s);
                (0..self.channel_count).map(move |c| value(sample, c))
            })
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut hdr = argus_replay_chunk_hdr_t {
            magic: ARGUS_FRAME_MAGIC,
            version: u16::try_from(ARGUS_REPLAY_VERSION).unwrap_or(0),
            type_: u16::try_from(ARGUS_MSG_REPLAY_CHUNK).unwrap_or(0),
            seq,
            sample_offset: first,
            sample_count: count,
            chunk_index: index,
            chunk_total: total,
            channel_count: self.channel_count,
            payload_len: u16::try_from(payload.len()).unwrap_or(u16::MAX),
            crc: 0,
        };
        let covered = hdr.as_bytes().get(..26).unwrap_or(&[]);
        hdr.crc = Crc16::new().update(covered).update(&payload).finish();
        let mut datagram = hdr.as_bytes().to_vec();
        datagram.extend_from_slice(&payload);
        datagram
    }
}

/// The dataset: a value for every (absolute sample, channel), distinct
/// enough that a sample in the wrong place shows.
pub fn value(sample: u32, channel: u16) -> u16 {
    let mixed = sample.wrapping_mul(0x9E37_79B1) ^ u32::from(channel).wrapping_mul(0x85EB_CA6B);
    let [a, b, c, d] = mixed.to_le_bytes();
    u16::from_le_bytes([a ^ c, b ^ d])
}
