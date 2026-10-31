//! A duration figure for the status lines: how many, the sum and the
//! longest, reset at each report.
//!
//! `argus_replay.c` keeps these as three loose `uint32_t`s per figure
//! (`g_fetch_us_sum`, `g_fetch_us_max`, `g_fetch_n` and so on); here each
//! figure is one value with the same arithmetic.

/// Durations in microseconds since the last report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    sum: u32,
    max: u32,
    n: u32,
}

impl Timing {
    /// No durations yet.
    pub const fn new() -> Self {
        Self {
            sum: 0,
            max: 0,
            n: 0,
        }
    }

    /// Adds one duration. The sum wraps at 2^32 as the C's does; the
    /// stream's largest, the fetch times, come to about 2.4 s per 5 s
    /// report, far short of the 71 minutes that takes.
    pub fn record(&mut self, us: u32) {
        self.sum = self.sum.wrapping_add(us);
        self.n = self.n.wrapping_add(1);
        self.max = self.max.max(us);
    }

    /// The mean, rounded down, or 0 with nothing recorded (the C's
    /// `n ? sum / n : 0`).
    pub const fn avg(&self) -> u32 {
        match self.sum.checked_div(self.n) {
            Some(avg) => avg,
            None => 0,
        }
    }

    /// The longest, or 0 with nothing recorded.
    pub const fn max(&self) -> u32 {
        self.max
    }

    /// How many were recorded.
    pub const fn n(&self) -> u32 {
        self.n
    }

    /// Returns the figures so far and starts again from none, as the stream
    /// report does after printing them.
    pub fn take(&mut self) -> Self {
        core::mem::take(self)
    }
}
