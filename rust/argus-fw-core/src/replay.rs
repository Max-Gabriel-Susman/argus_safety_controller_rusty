//! The replay side of `argus_replay.c` that the firmware decides rather
//! than touches: where the relay is, how long to wait for it, and what the
//! one-shot fetch's two console lines say about the half it brought back.
//!
//! The socket, the clock and the buffer are the firmware's; the protocol is
//! `argus-replay`'s [`Client`].

use argus_replay::Client;
use argus_wire::generated::{ARGUS_MAX_CHANNELS, ARGUS_REPLAY_SAMPLES_PER_HALF};

use crate::lines::{ReplayCounters, ReplayOk};
use crate::telemetry::HOST_ADDRESS;

/// The relay, 192.168.1.20. The C's comment: same machine as the telemetry
/// destination, and if that address moves, both must move together. So it
/// is the same constant.
pub const RELAY_ADDRESS: [u8; 4] = HOST_ADDRESS;

/// The PS's end of the replay conversation, `ARGUS_REPLAY_LOCAL_PORT`.
pub const LOCAL_PORT: u16 = 5011;

/// How long a burst may take before the first gap is asked for again,
/// `ARGUS_REPLAY_TIMEOUT_MS`.
pub const TIMEOUT_MS: u32 = 200;

/// Retransmits per fetch before it fails, `ARGUS_REPLAY_MAX_RETRIES`.
pub const MAX_RETRIES: u8 = 5;

// bindgen makes every `#define` a `u32`; the client takes both as `u16`.
const _: () = assert!(ARGUS_MAX_CHANNELS == 96 && ARGUS_REPLAY_SAMPLES_PER_HALF == 147);

/// Channels per sample, as the client is configured with.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the assert above pins it at 96"
)]
pub const CHANNELS: u16 = ARGUS_MAX_CHANNELS as u16;

/// Samples in one half, the length of every fetch.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the assert above pins it at 147"
)]
pub const SAMPLES_PER_HALF: u16 = ARGUS_REPLAY_SAMPLES_PER_HALF as u16;

/// The client as `argus_replay_init()` configures it: 96 channels, 200 ms,
/// 5 retries. `None` cannot happen at 96 channels; the client refuses only
/// a channel count no chunk can carry.
pub fn client() -> Option<Client> {
    Client::new(CHANNELS, TIMEOUT_MS, MAX_RETRIES).ok()
}

/// The `[sample][channel]` sample of a fetched half: the relay's
/// little-endian `u16` at `(sample * 96 + channel) * 2`, as the C indexes
/// its `uint16_t` buffer. Zero past the end of `half`.
fn sample(half: &[u8], sample: usize, channel: usize) -> u16 {
    let at = (sample * usize::from(CHANNELS) + channel) * 2;
    match half.get(at..at + 2) {
        Some(&[lo, hi]) => u16::from_le_bytes([lo, hi]),
        _ => 0,
    }
}

/// The `replay ok:` line for a completed one-shot fetch of `half`: the
/// four samples the C prints.
pub fn ok_line(half: &[u8]) -> ReplayOk {
    ReplayOk {
        s0_c0: sample(half, 0, 0),
        s0_c5: sample(half, 0, 5),
        s1_c0: sample(half, 1, 0),
        s146_c95: sample(half, 146, 95),
    }
}

/// The `replay: req=` line, `argus_replay_report()`: the client's counters
/// and how much of its last fetch arrived.
pub fn counters_line(client: &Client) -> ReplayCounters {
    let c = client.counters();
    ReplayCounters {
        requests: c.requests_sent,
        retransmits: c.retransmits_sent,
        accepted: c.chunks_accepted,
        rejected: c.chunks_rejected,
        timeouts: c.timeouts,
        arrived: u32::from(client.arrived_count()),
        total: u32::from(client.total_chunks()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_little_endian_sample_major() {
        let mut half = [0u8; 147 * 96 * 2];
        let mut put = |s: usize, c: usize, v: u16| {
            let at = (s * 96 + c) * 2;
            if let Some(dst) = half.get_mut(at..at + 2) {
                dst.copy_from_slice(&v.to_le_bytes());
            }
        };
        put(0, 0, 0x0001);
        put(0, 5, 0x00a5);
        put(1, 0, 0x1000);
        put(146, 95, 0xfffe);
        assert_eq!(
            ok_line(&half),
            ReplayOk {
                s0_c0: 0x0001,
                s0_c5: 0x00a5,
                s1_c0: 0x1000,
                s146_c95: 0xfffe,
            }
        );
    }

    #[test]
    fn a_short_buffer_reads_as_zero() {
        let line = ok_line(&[0xff; 4]);
        assert_eq!(line.s0_c0, 0xffff);
        assert_eq!(line.s146_c95, 0);
    }

    #[test]
    fn a_fresh_client_reports_nothing_sent() {
        let client = client();
        assert_eq!(client.as_ref().map(Client::samples_per_chunk), Some(7));
        assert_eq!(
            client.as_ref().map(counters_line),
            Some(ReplayCounters {
                requests: 0,
                retransmits: 0,
                accepted: 0,
                rejected: 0,
                timeouts: 0,
                arrived: 0,
                total: 0,
            })
        );
    }
}
