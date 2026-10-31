/* Arty Z7-20: 512 MB of DDR3. The firmware uses 63 MB from 1 MB up (below
 * that is the OCM and what the boot ROM owns), then one 1 MB section that the
 * MMU maps uncached, for the Ethernet DMA descriptors. z7link.x, from
 * zynq7000-rt, includes this file and places every output section in the
 * regions named here. */
MEMORY
{
  OCM : ORIGIN = 0x00000000, LENGTH = 192K
  DDR : ORIGIN = 0x00100000, LENGTH = 63M
  OCM_UPPER(rx): ORIGIN = 0xFFFF0000, LENGTH = 64K
  /* 0x0400_0000, exactly one 1 MB MMU section. */
  UNCACHED : ORIGIN = ORIGIN(DDR) + LENGTH(DDR), LENGTH = 1M
}

/* XSDB loads the ELF straight into DDR and starts it at its entry point, with
 * no FSBL and no boot image, so the vectors, code, data and stacks all live
 * in DDR. */
REGION_ALIAS("VECTORS", DDR);
REGION_ALIAS("CODE", DDR);
REGION_ALIAS("DATA", DDR);
REGION_ALIAS("STACKS", DDR);

SECTIONS
{
  /* NOLOAD: the ELF carries no bytes for this section, so XSDB downloads
   * nothing here. The firmware writes what it needs at run time. */
  .uncached (NOLOAD) : ALIGN(4) {
    . = ALIGN(4);
    _sbss_uncached = .;
    *(.uncached .uncached.*);
    . = ALIGN(4);
    _ebss_uncached = .;
  } > UNCACHED
}
