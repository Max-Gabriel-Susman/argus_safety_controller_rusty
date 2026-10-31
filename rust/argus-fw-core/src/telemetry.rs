//! The telemetry half of `main.c`'s main loop: the frame each feature bin
//! becomes, and the count of frames that never reached the network.
//!
//! The C hands every frame to lwIP and ignores what `udp_sendto()` returns.
//! The Rust firmware does not send while the link is down (smoltcp would
//! hold the datagram until the link came back, and the host would then get
//! a burst of stale bins), and gives up on a frame the socket cannot take
//! at once. Both are counted here, and a line says so when sending resumes.

use argus_wire::frame::{FramePacket, encode_frame};

use crate::acq::{Features, bin_time};
use crate::lines::TelemetryUnsent;

/// The host's telemetry receiver, 192.168.1.20 (the C's `g_dest`).
pub const HOST_ADDRESS: [u8; 4] = [192, 168, 1, 20];

/// The v3 frame for one bin, as `argus_send_frame()` is called from the
/// main loop: the sample is the bin number, `t` the bin's time at the sweep
/// rate.
pub fn frame(features: &Features) -> FramePacket {
    encode_frame(
        features.bin,
        bin_time(features.bin),
        &features.counts,
        &features.power,
    )
}

/// Why a frame was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsent {
    /// The link was down. The frame was dropped, not queued.
    LinkDown,
    /// The socket did not take the frame: its buffer stayed full, or the
    /// stack refused the destination.
    Refused,
}

/// Frames not sent since boot, by reason, and whether any have not been
/// reported yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnsentCounter {
    link_down: u32,
    refused: u32,
    unreported: bool,
}

impl UnsentCounter {
    /// None yet.
    pub const fn new() -> Self {
        Self {
            link_down: 0,
            refused: 0,
            unreported: false,
        }
    }

    /// A frame was not sent, for `why`.
    pub fn unsent(&mut self, why: Unsent) {
        let count = match why {
            Unsent::LinkDown => &mut self.link_down,
            Unsent::Refused => &mut self.refused,
        };
        *count = count.wrapping_add(1);
        self.unreported = true;
    }

    /// A frame was sent. Returns the line to print when frames went unsent
    /// since the last frame that was: once per run of unsent frames, rather
    /// than one line per 50 ms bin while the link is down.
    pub fn sent(&mut self) -> Option<TelemetryUnsent> {
        if !self.unreported {
            return None;
        }
        self.unreported = false;
        Some(TelemetryUnsent {
            link_down: self.link_down,
            refused: self.refused,
        })
    }
}
