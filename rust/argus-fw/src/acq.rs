//! The acquisition block's registers, argus_acq_top at 0x43C0_0000, as a
//! typed view.
//!
//! The C reaches every register with `Xil_In32`/`Xil_Out32` on an address
//! from `argus_acq.h`. Here the register map is a `#[repr(C)]` struct, and
//! derive-mmio generates a handle whose methods do one volatile 32-bit read
//! or write per call, never a reference to the registers themselves (the
//! compiler could then read them whenever it liked, and a GP0 read costs a
//! microsecond and some of these registers change under it). The offsets
//! come from [`argus_fw_core::acq::reg`] and are checked against the struct
//! at compile time.
//!
//! The struct says what each register allows: STATUS and the banks are
//! read-only, REPLAY_ACK is write-only, CTRL is read-write. LANE_SRC and
//! MISO_DELAY are declared read-only on purpose: the firmware runs the
//! in-fabric chip models, their reset value, and must never write them.
//!
//! The page stays as the HAL's MMU table maps it (strongly ordered, which
//! is stricter than Device): each access reaches the fabric, in program
//! order, unbuffered.
//!
//! Every read here is free of side effects in the fabric, so reads take
//! `&self`; writes take `&mut self`.

use core::sync::atomic::{AtomicBool, Ordering};

use argus_fw_core::acq::{self, BASE, ReplayStatus, reg};
use argus_wire::frame::CHANNELS;

/// The register map, in address order. Fields whose name starts with `_`
/// are gaps (they read 0xDEADBEEF) and get no accessors.
#[derive(derive_mmio::Mmio)]
#[mmio(no_owned)]
#[repr(C)]
struct Registers {
    ctrl: u32,
    #[mmio(PureRead)]
    status: u32,
    #[mmio(PureRead)]
    frame_index: u32,
    #[mmio(PureRead)]
    id: u32,
    #[mmio(PureRead)]
    replay_status: u32,
    #[mmio(Write)]
    replay_ack: u32,
    #[mmio(PureRead)]
    feature_index: u32,
    #[mmio(PureRead)]
    feat_dropped: u32,
    #[mmio(PureRead)]
    lane_src: u32,
    #[mmio(PureRead)]
    miso_delay: u32,
    _gap_after_miso_delay: [u32; (reg::frame(0) - reg::MISO_DELAY - 4) / 4],
    /// FRAME\[n\], one sample in the low half of each word.
    #[mmio(PureRead)]
    frame: [u32; CHANNELS],
    _gap_after_frame: [u32; (reg::feature_lo(0) - reg::frame(CHANNELS)) / 4],
    /// FEATURE\[n\] is words `2n` (sum bits 31:0) and `2n + 1` (count over
    /// sum bits 47:32).
    #[mmio(PureRead)]
    feature: [u32; 2 * CHANNELS],
}

// The struct's layout is the register map of argus_acq.h.
const _: () = {
    use core::mem::offset_of;
    assert!(offset_of!(Registers, ctrl) == reg::CTRL);
    assert!(offset_of!(Registers, status) == reg::STATUS);
    assert!(offset_of!(Registers, frame_index) == reg::FRAME_INDEX);
    assert!(offset_of!(Registers, id) == reg::ID);
    assert!(offset_of!(Registers, replay_status) == reg::REPLAY_STATUS);
    assert!(offset_of!(Registers, replay_ack) == reg::REPLAY_ACK);
    assert!(offset_of!(Registers, feature_index) == reg::FEATURE_INDEX);
    assert!(offset_of!(Registers, feat_dropped) == reg::FEAT_DROPPED);
    assert!(offset_of!(Registers, lane_src) == reg::LANE_SRC);
    assert!(offset_of!(Registers, miso_delay) == reg::MISO_DELAY);
    assert!(offset_of!(Registers, frame) == reg::frame(0));
    assert!(offset_of!(Registers, feature) == reg::feature_lo(0));
    assert!(core::mem::size_of::<Registers>() == reg::feature_hi(CHANNELS - 1) + 4);
    // Inside the block's 4 KB page.
    assert!(core::mem::size_of::<Registers>() <= 0x1000);
};

/// Set by the first [`Acq::take`], so that there is only ever one handle.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// The one handle on the acquisition registers.
#[expect(
    dead_code,
    reason = "the acquisition smoke test and the feature task are its first users"
)]
pub struct Acq {
    regs: MmioRegisters<'static>,
}

/// The two words of one channel's FEATURE entry, as read.
#[derive(Clone, Copy, Debug)]
#[expect(
    dead_code,
    reason = "the acquisition smoke test and the feature task are its first users"
)]
pub struct FeatureWords {
    /// Bits 31:0 of the sum of squares.
    pub lo: u32,
    /// The crossing count in bits 31:16, bits 47:32 of the sum below.
    pub hi: u32,
}

#[expect(
    dead_code,
    reason = "the acquisition smoke test and the feature task are its first users"
)]
impl FeatureWords {
    /// The bin's threshold crossings.
    pub const fn count(self) -> u16 {
        acq::feature_count(self.hi)
    }

    /// The bin's mean square, as the telemetry frame carries it.
    pub const fn power(self) -> u32 {
        acq::feature_power(self.hi, self.lo)
    }
}

#[expect(
    dead_code,
    reason = "the acquisition smoke test, the feature task and the replay stream \
              are its first users"
)]
impl Acq {
    /// The handle, the first time this is called; `None` after that.
    ///
    /// Reading a register before the PL is programmed hangs the A9 on the
    /// bus. The boot flow programs the bitstream before it loads the
    /// firmware, so that cannot happen here; the C relies on the same.
    pub fn take() -> Option<Self> {
        if TAKEN.swap(true, Ordering::Relaxed) {
            return None;
        }
        // SAFETY: BASE is argus_acq_top's address in the block design, and
        // `Registers` matches its register map (checked above), so every
        // accessor reads or writes a real 32-bit register, aligned. The
        // page is mapped strongly ordered by the HAL and nothing remaps
        // it. TAKEN makes this the only handle, so nothing else in the
        // firmware accesses these registers behind its back.
        let regs = unsafe { Registers::new_mmio_at(BASE) };
        Some(Self { regs })
    }

    /// CTRL: see [`acq::ctrl`].
    pub fn ctrl(&self) -> u32 {
        self.regs.read_ctrl()
    }

    /// Writes CTRL. Callers read it first and change only their bits, as
    /// the C's hold handshake does.
    pub fn write_ctrl(&mut self, value: u32) {
        self.regs.write_ctrl(value);
    }

    /// STATUS: see [`acq::status`].
    pub fn status(&self) -> u32 {
        self.regs.read_status()
    }

    /// The sweep number of the frame in the read bank.
    pub fn frame_index(&self) -> u32 {
        self.regs.read_frame_index()
    }

    /// The fabric revision: see [`acq::is_accepted_id`].
    pub fn id(&self) -> u32 {
        self.regs.read_id()
    }

    /// REPLAY_STATUS, decoded.
    pub fn replay_status(&self) -> ReplayStatus {
        ReplayStatus(self.regs.read_replay_status())
    }

    /// Writes REPLAY_ACK: each set bit of [`acq::replay_ack`] clears its
    /// flag in REPLAY_STATUS.
    pub fn ack_replay(&mut self, bits: u32) {
        self.regs.write_replay_ack(bits);
    }

    /// The number of the bin in the feature read bank.
    pub fn feature_index(&self) -> u32 {
        self.regs.read_feature_index()
    }

    /// Bins lost because a feature hold outlasted a whole bin.
    pub fn feat_dropped(&self) -> u32 {
        self.regs.read_feat_dropped()
    }

    /// FRAME\[n\]: channel `n`'s sample, or `None` past the last channel.
    /// Only coherent with the other channels under the hold.
    pub fn frame(&self, n: usize) -> Option<u16> {
        let word = self.regs.read_frame(n).ok()?;
        // The C's (uint16_t) cast: the sample is the low half-word.
        let [b0, b1, ..] = word.to_le_bytes();
        Some(u16::from_le_bytes([b0, b1]))
    }

    /// FEATURE\[n\], low word then high word as the C reads them, or
    /// `None` past the last channel. Only coherent under the feature hold.
    pub fn feature(&self, n: usize) -> Option<FeatureWords> {
        let lo_index = n.checked_mul(2)?;
        let lo = self.regs.read_feature(lo_index).ok()?;
        let hi = self.regs.read_feature(lo_index.checked_add(1)?).ok()?;
        Some(FeatureWords { lo, hi })
    }
}
