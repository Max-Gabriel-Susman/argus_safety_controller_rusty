//! The acquisition block as the PS sees it: the register map of
//! `argus_acq.h` and what the firmware makes of the words it reads.
//!
//! Nothing here reads a register. The firmware does the volatile reads
//! (under the hold handshake, see [`HOLD_SPIN_MAX`]) and passes the words
//! in; this module decodes them exactly as `main.c` does.
//!
//! `argus_acq.h` mirrors the register map comment in `argus_acq_axi.vhd`;
//! if the two disagree, the hardware wins.

use argus_wire::frame::CHANNELS;

/// Base address of the register block, pinned in the block design.
pub const BASE: usize = 0x43C0_0000;

/// Register offsets from [`BASE`], in bytes. Every register is 32 bits wide
/// and must be accessed as one volatile 32-bit word.
pub mod reg {
    /// Control: see [`ctrl`](super::ctrl).
    pub const CTRL: usize = 0x000;
    /// Status: see [`status`](super::status).
    pub const STATUS: usize = 0x004;
    /// Sweep number of the frame in the read bank; frozen under hold.
    pub const FRAME_INDEX: usize = 0x008;
    /// Fabric revision: see [`is_accepted_id`](super::is_accepted_id).
    pub const ID: usize = 0x00C;
    /// Replay playback state: see [`ReplayStatus`](super::ReplayStatus).
    pub const REPLAY_STATUS: usize = 0x010;
    /// Write 1 to clear: see [`replay_ack`](super::replay_ack).
    pub const REPLAY_ACK: usize = 0x014;
    /// Number of the bin in the feature read bank; frozen under feat_hold.
    pub const FEATURE_INDEX: usize = 0x018;
    /// Bins lost because a feat_hold outlasted a whole bin.
    pub const FEAT_DROPPED: usize = 0x01C;
    /// ACQ4 only: which source feeds each lane. Its reset value, 0, is the
    /// in-fabric chip models, which is what this firmware runs on, so it is
    /// never written. On ACQ3 it reads 0xDEADBEEF.
    pub const LANE_SRC: usize = 0x020;
    /// ACQ4 only: per-lane MISO capture delay, for external chips. Never
    /// written, for the same reason as [`LANE_SRC`].
    pub const MISO_DELAY: usize = 0x024;

    /// FRAME\[n\]: channel `n`'s sample in the held frame, in the low 16
    /// bits.
    pub const fn frame(n: usize) -> usize {
        0x100 + 4 * n
    }

    /// FEATURE\[n\], low word: bits 31:0 of channel `n`'s sum of squares.
    pub const fn feature_lo(n: usize) -> usize {
        0x400 + 8 * n
    }

    /// FEATURE\[n\], high word: the crossing count in bits 31:16 over bits
    /// 47:32 of the sum.
    pub const fn feature_hi(n: usize) -> usize {
        0x400 + 8 * n + 4
    }
}

/// CTRL bits.
pub mod ctrl {
    /// Run the acquisition chain.
    pub const ENABLE: u32 = 1 << 0;
    /// Hold the chain in reset.
    pub const SOFT_RESET: u32 = 1 << 1;
    /// Feed the chip models from the replay BRAM instead of their built-in
    /// pattern.
    pub const EXT_MODE: u32 = 1 << 2;
    /// Freeze the frame read bank. The assembler keeps sweeping and
    /// discards what it completes, so hold only as long as a read needs.
    pub const HOLD: u32 = 1 << 3;
    /// Freeze the feature read bank. A bin that completes under the hold is
    /// deferred, not lost, unless the hold outlasts a whole bin.
    pub const FEAT_HOLD: u32 = 1 << 4;
}

/// STATUS bits.
pub mod status {
    /// The chain is running.
    pub const READY: u32 = 1 << 0;
    /// A frame was overwritten before it was published.
    pub const OVERRUN: u32 = 1 << 1;
    /// The frame bank is frozen and settled. Poll this after setting
    /// [`HOLD`](super::ctrl::HOLD); the write does not take effect at once.
    pub const HELD: u32 = 1 << 2;
    /// The same for the feature bank and
    /// [`FEAT_HOLD`](super::ctrl::FEAT_HOLD).
    pub const FEAT_HELD: u32 = 1 << 3;
}

/// REPLAY_ACK bits. Write 1 to clear the matching REPLAY_STATUS flag.
pub mod replay_ack {
    /// The PS has refilled half 0.
    pub const CONSUMED0: u32 = 1 << 0;
    /// The PS has refilled half 1.
    pub const CONSUMED1: u32 = 1 << 1;
    /// Clear the underrun flag.
    pub const UNDERRUN: u32 = 1 << 2;
}

/// How many times to poll STATUS for the held bit before giving up and
/// releasing the hold. A poll is one GP0 read, about 1 us, so this is about
/// 100 ms: far longer than a hold takes, short enough not to hang the
/// firmware on a fabric that never answers.
pub const HOLD_SPIN_MAX: u32 = 100_000;

/// ID of the ACQ3 fabric ("ACQ3"), the first with the feature bank.
pub const ID_ACQ3: u32 = 0x4143_5133;

/// ID of the ACQ4 fabric ("ACQ4"). It adds LANE_SRC and MISO_DELAY, whose
/// reset values make it behave as ACQ3; the firmware never writes them.
pub const ID_ACQ4: u32 = 0x4143_5134;

/// Whether `id` names a fabric this firmware can drive. Anything else almost
/// always means a bitstream that predates the firmware, and every register
/// after ID would then mean something else.
pub const fn is_accepted_id(id: u32) -> bool {
    id == ID_ACQ3 || id == ID_ACQ4
}

/// Channels from one chip. A frame is three chips' worth in chip order.
pub const CH_PER_CHIP: u16 = 32;

/// The fabric's sweep rate in sweeps per second, 125 MHz / (35 slots x 119
/// clocks). As in the C, an `f32`, so bin times round the same way.
pub const SWEEP_HZ: f32 = 30012.0;

/// Sweeps per feature bin: 1500, 50 ms.
pub const BIN_LEN: u32 = 1500;

/// The crossing count in a FEATURE high word, bits 31:16.
pub const fn feature_count(hi: u32) -> u16 {
    (hi >> 16) as u16
}

/// The 48-bit sum of squared high-passed samples: bits 47:32 from the high
/// word's low half, bits 31:0 from the low word.
pub const fn feature_sum(hi: u32, lo: u32) -> u64 {
    (((hi & 0xFFFF) as u64) << 32) | lo as u64
}

/// A channel's power: the sum of squares divided by [`BIN_LEN`], integer
/// division, so the mean square in ADC code squared. The telemetry frame
/// carries this, and the host's bit-exact check recomputes it, so it must
/// be exactly the C's `(uint32_t)(sum / 1500)`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the C's (uint32_t) cast truncates the same way; a bin of 16-bit samples \
              has a mean square under 2^30, so it never does in practice"
)]
pub const fn feature_power(hi: u32, lo: u32) -> u32 {
    (feature_sum(hi, lo) / BIN_LEN as u64) as u32
}

/// One feature bin as `acq_read_features()` hands it back: the bin number
/// and each channel's crossing count and power, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    /// FEATURE_INDEX under the feature hold: the bin in the read bank.
    pub bin: u32,
    /// Each channel's threshold crossings in the bin.
    pub counts: [u16; CHANNELS],
    /// Each channel's power, [`feature_power`].
    pub power: [u32; CHANNELS],
}

impl Features {
    /// Bin `bin` with every channel zero, to be filled by [`Self::set`].
    pub const fn new(bin: u32) -> Self {
        Self {
            bin,
            counts: [0; CHANNELS],
            power: [0; CHANNELS],
        }
    }

    /// Decodes channel `n`'s FEATURE words, high and low, as the C does.
    /// A channel past the last is ignored.
    pub fn set(&mut self, n: usize, hi: u32, lo: u32) {
        if let Some(count) = self.counts.get_mut(n) {
            *count = feature_count(hi);
        }
        if let Some(power) = self.power.get_mut(n) {
            *power = feature_power(hi, lo);
        }
    }

    /// Channel `n`'s count and power, or zeros past the last channel.
    pub fn channel(&self, n: usize) -> (u16, u32) {
        (
            self.counts.get(n).copied().unwrap_or(0),
            self.power.get(n).copied().unwrap_or(0),
        )
    }
}

/// Bins between the status lines of the feature loop: 100 x 50 ms = 5 s,
/// the C's `STATUS_EVERY_BINS`.
pub const STATUS_EVERY_BINS: u32 = 100;

/// Bins between the `tx` lines: 20 x 50 ms = 1 s, the C's `tx_count % 20`.
pub const TX_EVERY_BINS: u32 = 20;

/// Which lines are due after a bin was read, from [`BinCounter::read`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Due {
    /// The `tx` line: every [`TX_EVERY_BINS`]th bin read.
    pub tx: bool,
    /// The status block (the stream lines, `feat:` and the frame check):
    /// every [`STATUS_EVERY_BINS`]th bin read.
    pub status: bool,
}

/// The bookkeeping of `main.c`'s loop around `acq_read_features()`: which
/// bin was read last, how many have been read and how many skipped.
///
/// The loop polls FEATURE_INDEX and reads only when it has moved. A read
/// whose hold never took effect is skipped and counted rather than
/// published torn, and the bin it missed is taken as seen, so the next
/// attempt waits for the bin after it, as in the C.
///
/// The count of bins read is the C's `tx_count`, the first figure of the
/// `tx` line. The C counts every frame it builds, whatever `udp_sendto()`
/// then returns, so this counts the same; frames that never reached the
/// stack are counted apart (see `telemetry::Unsent`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BinCounter {
    last: u32,
    read: u32,
    skipped: u32,
}

impl BinCounter {
    /// Nothing read yet. The C starts `last_bin` at 0, so bin 0 is never
    /// read by the loop (the smoke test reads the first bin itself).
    pub const fn new() -> Self {
        Self {
            last: 0,
            read: 0,
            skipped: 0,
        }
    }

    /// Whether FEATURE_INDEX reading `index` means a bin not read yet.
    pub const fn is_new(&self, index: u32) -> bool {
        index != self.last
    }

    /// Bin `bin` was read. Returns which lines are now due.
    pub fn read(&mut self, bin: u32) -> Due {
        self.last = bin;
        self.read = self.read.wrapping_add(1);
        Due {
            tx: self.read.is_multiple_of(TX_EVERY_BINS),
            status: self.read.is_multiple_of(STATUS_EVERY_BINS),
        }
    }

    /// The hold never took effect; FEATURE_INDEX now reads `index`.
    pub fn skipped(&mut self, index: u32) {
        self.last = index;
        self.skipped = self.skipped.wrapping_add(1);
    }

    /// Bins read since boot.
    pub const fn read_count(&self) -> u32 {
        self.read
    }

    /// Bins skipped since boot.
    pub const fn skipped_count(&self) -> u32 {
        self.skipped
    }
}

/// A bin's time in seconds since the chain was reset: the `t` of its
/// telemetry frame. The C computes `(float)bin * ((float)BIN_LEN /
/// SWEEP_HZ)` in single precision; so does this, in the same order, so the
/// two round alike. (Above 2^24 the bin itself rounds to the nearest float,
/// in both.)
pub fn bin_time(bin: u32) -> f32 {
    bin as f32 * (BIN_LEN as f32 / SWEEP_HZ)
}

/// The word channel `n` must carry in a frame whose sample index is `idx`:
/// the chip number in bits 15:14, the channel within the chip in bits 13:8,
/// the index in bits 7:0. Both sources, the chips' built-in IDENT pattern
/// and the relay's synthetic one, use this layout.
pub const fn frame_word(n: u16, idx: u8) -> u16 {
    ((n / CH_PER_CHIP) << 14) | ((n % CH_PER_CHIP) << 8) | idx as u16
}

/// What [`check_frame`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCheck {
    /// The sample index, taken from channel 0's low byte.
    pub idx: u8,
    /// Words that are not [`frame_word`]`(n, idx)`.
    pub bad: u32,
}

/// The identity check of `acq_frame_check()`: every word must carry its own
/// chip and channel, and the whole frame one sample index, channel 0's.
pub fn check_frame(frame: &[u16; CHANNELS]) -> FrameCheck {
    let [first, ..] = frame;
    let [idx, _] = first.to_le_bytes();
    let bad = (0u16..)
        .zip(frame)
        .filter(|&(n, &word)| word != frame_word(n, idx))
        .count();
    FrameCheck {
        idx,
        // At most CHANNELS, so it always fits.
        bad: u32::try_from(bad).unwrap_or(u32::MAX),
    }
}

/// The sample index a frame with the built-in source should carry: the
/// chips' sweep counter agrees with the assembler's frame counter modulo
/// 256, one behind it. Frames dropped during a hold do not break that,
/// because FRAME_INDEX names the frame in the bank. With the external source
/// the index is the dataset's sample number and this does not apply.
pub const fn expected_index(frame_index: u32) -> u8 {
    let [low, ..] = frame_index.wrapping_sub(1).to_le_bytes();
    low
}

/// The replay BRAM aperture: two halves, sample-major and channel-minor,
/// matching the relay's chunk payload so that chunks are copied in
/// unchanged. The PL plays one half while the PS refills the other.
pub mod bram {
    use argus_wire::generated::{ARGUS_MAX_CHANNELS, ARGUS_REPLAY_SAMPLES_PER_HALF};

    /// Base address of the aperture, pinned in the block design.
    pub const BASE: usize = 0x4000_0000;

    /// Size of the aperture: 64 KB of BRAM.
    pub const SIZE: usize = 0x1_0000;

    /// The number of halves.
    pub const HALVES: usize = 2;

    /// One half: 147 samples of 96 little-endian `u16` channels, 28224
    /// bytes. Half `h` starts at `h * HALF_BYTES`, as `ARGUS_BRAM_HALF(h)`.
    pub const HALF_BYTES: usize =
        ARGUS_REPLAY_SAMPLES_PER_HALF as usize * ARGUS_MAX_CHANNELS as usize * 2;

    /// The A9's L1 and L2 line length. Each half is cleaned to memory before
    /// it is acked, so both its start and its length must be whole lines, or
    /// the clean would reach into the half the PL is playing.
    pub const CACHE_LINE: usize = 32;

    // The C checks the first with #error; the other two are what make the
    // per-half clean exact.
    const _: () = assert!(
        HALVES * HALF_BYTES <= SIZE,
        "two halves do not fit the aperture"
    );
    const _: () = assert!(
        HALF_BYTES.is_multiple_of(CACHE_LINE),
        "a half is not whole cache lines"
    );
    const _: () = assert!(
        BASE.is_multiple_of(CACHE_LINE),
        "the aperture is not line aligned"
    );
}

/// REPLAY_STATUS, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayStatus(pub u32);

impl ReplayStatus {
    /// The half the PL is playing, 0 or 1.
    pub const fn play_half(self) -> u32 {
        self.0 & 1
    }

    /// The PL has finished with half 0 and it awaits a refill.
    pub const fn consumed0(self) -> bool {
        self.0 & (1 << 1) != 0
    }

    /// The PL has finished with half 1 and it awaits a refill.
    pub const fn consumed1(self) -> bool {
        self.0 & (1 << 2) != 0
    }

    /// The PL flipped back into a half before it was acked and is playing
    /// stale data.
    pub const fn underrun(self) -> bool {
        self.0 & (1 << 3) != 0
    }

    /// The row being played within the half, bits 23:8.
    pub const fn row(self) -> u32 {
        (self.0 >> 8) & 0xFFFF
    }
}
