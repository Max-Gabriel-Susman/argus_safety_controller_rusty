//! The replay BRAM aperture at 0x4000_0000, as two byte arrays.
//!
//! Unlike the registers, the BRAM is plain memory: reading it has no side
//! effects, and the PL only reads it, through its own port. So it is safe
//! to hand out ordinary Rust references to it, and the replay client can
//! copy a chunk into a half with `copy_from_slice`, which is the C's
//! `memcpy` into `ARGUS_BRAM_HALF(h)`.
//!
//! The HAL maps the aperture strongly ordered, like all of the PL: no write
//! merging, so a half's copy would be 14112 single-beat AXI writes, and an
//! unaligned access (which `copy_from_slice` may make) would fault. So the
//! aperture's 1 MB section is remapped Normal write-back first, as the C's
//! `Xil_SetTlbAttributes(ARGUS_BRAM_BASE, NORM_WB_CACHE)` does: the copy
//! lands in the D-cache and one clean per half writes it out in 32-byte
//! bursts. The PL sees the BRAM itself, never the A9's caches, so a half
//! must be cleaned before it is acked. The layout in
//! [`argus_fw_core::acq::bram`] makes each half whole cache lines, so that
//! clean never touches the other half.

use core::sync::atomic::{AtomicBool, Ordering};

use argus_fw_core::acq::bram::{BASE, HALF_BYTES, HALVES};
use argus_fw_core::stream::Half;
use zynq7000_hal::cache::{AlignmentError, clean_data_cache_range};
use zynq7000_hal::mmu::{section_attrs, update_l1_table};

/// The aperture as the PS writes it: half 0, then half 1.
type Halves = [[u8; HALF_BYTES]; HALVES];

/// Set by the first [`ReplayBram::take`], so that there is only ever one
/// reference to the aperture.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// The one handle on the replay BRAM.
pub struct ReplayBram {
    halves: &'static mut Halves,
}

impl ReplayBram {
    /// Remaps the aperture Normal write-back and returns the handle, the
    /// first time this is called; `None` after that, or if the section
    /// cannot be remapped.
    ///
    /// One 1 MB section; only the 64 KB aperture lives in it. The
    /// acquisition registers at 0x43C0_0000 are a different section and
    /// stay strongly ordered. The attributes are DDR's (inner and outer
    /// write-back write-allocate, shareable), which is what the C's
    /// `NORM_WB_CACHE` amounts to. The HAL cleans and invalidates the L1 and
    /// the TLB after the change.
    pub fn take() -> Option<Self> {
        if TAKEN.swap(true, Ordering::Relaxed) {
            return None;
        }
        let base = u32::try_from(BASE).ok()?;
        // BASE is 1 MB aligned, so this cannot fail.
        update_l1_table(base, section_attrs::DDR).ok()?;

        let ptr = core::ptr::with_exposed_provenance_mut::<Halves>(BASE);
        // SAFETY: BASE is the BRAM controller's address in the block
        // design, now mapped Normal memory; the two halves fit its 64 KB
        // (checked in argus-fw-core) and need only byte alignment. The
        // bitstream is loaded before the firmware runs, so the memory
        // answers. TAKEN makes this the only reference to it, and the PL
        // only reads the BRAM, so nothing changes it behind this one.
        let halves = unsafe { &mut *ptr };
        Some(Self { halves })
    }

    /// `half`, for the replay client to fill.
    pub fn half(&mut self, half: Half) -> Option<&mut [u8; HALF_BYTES]> {
        self.halves.get_mut(half.index())
    }

    /// Writes `half` out of the L1 and L2 caches to the BRAM, and returns
    /// once it is there, so the PL reads what the PS wrote.
    pub fn clean(&self, half: Half) -> Result<(), AlignmentError> {
        let Some(bytes) = self.halves.get(half.index()) else {
            return Ok(());
        };
        // The aperture is below 4 GB, so its addresses fit; the layout
        // makes them whole cache lines (checked in argus-fw-core).
        let addr = u32::try_from(bytes.as_ptr().addr()).map_err(|_| AlignmentError)?;
        clean_data_cache_range(addr, HALF_BYTES)
    }
}
