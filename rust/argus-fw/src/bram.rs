//! The replay BRAM aperture at 0x4000_0000, as two byte arrays.
//!
//! Unlike the registers, the BRAM is plain memory: reading it has no side
//! effects, and the PL only reads it, through its own port. So it is safe
//! to hand out ordinary Rust references to it, and the replay client can
//! copy a chunk into a half with `copy_from_slice`, which is the C's
//! `memcpy` into `ARGUS_BRAM_HALF(h)`.
//!
//! The PL sees the BRAM itself, never the A9's caches. While the aperture
//! is mapped strongly ordered (the HAL's default) every store goes straight
//! to it; once it is remapped Normal write-back, as the C does for speed, a
//! half must be cleaned to the BRAM before it is acked. The layout in
//! [`argus_fw_core::acq::bram`] makes each half whole cache lines, so that
//! clean never touches the other half.

use core::sync::atomic::{AtomicBool, Ordering};

use argus_fw_core::acq::bram::{BASE, HALF_BYTES, HALVES};

/// The aperture as the PS writes it: half 0, then half 1.
type Halves = [[u8; HALF_BYTES]; HALVES];

/// Set by the first [`ReplayBram::take`], so that there is only ever one
/// reference to the aperture.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// The one handle on the replay BRAM.
#[expect(dead_code, reason = "the replay stream is its first user")]
pub struct ReplayBram {
    halves: &'static mut Halves,
}

#[expect(dead_code, reason = "the replay stream is its first user")]
impl ReplayBram {
    /// The handle, the first time this is called; `None` after that.
    pub fn take() -> Option<Self> {
        if TAKEN.swap(true, Ordering::Relaxed) {
            return None;
        }
        let ptr = core::ptr::with_exposed_provenance_mut::<Halves>(BASE);
        // SAFETY: BASE is the BRAM controller's address in the block
        // design, mapped by the HAL's MMU table; the two halves fit its
        // 64 KB (checked in argus-fw-core) and need only byte alignment.
        // The bitstream is loaded before the firmware runs, so the memory
        // answers. TAKEN makes this the only reference to it, and the PL
        // only reads the BRAM, so nothing changes it behind this one.
        let halves = unsafe { &mut *ptr };
        Some(Self { halves })
    }

    /// Half `h` (0 or 1) for the replay client to fill, or `None` for any
    /// other `h`.
    pub fn half(&mut self, h: usize) -> Option<&mut [u8; HALF_BYTES]> {
        self.halves.get_mut(h)
    }
}
