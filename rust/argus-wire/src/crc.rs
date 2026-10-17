//! CRC-16/CCITT-FALSE, the checksum on every Argus datagram.
//!
//! Width 16, polynomial 0x1021, initial value 0xFFFF, no reflection, no
//! final XOR. The standard check value, the CRC of the nine ASCII bytes
//! "123456789", is 0x29B1.
//!
//! Matches `crc16_ccitt()` and `crc16_ccitt_update()` in `argus_wire.h` bit
//! for bit. What differs is where the lookup table comes from: the header
//! carries 256 hand-copied entries, and here the compiler derives them from
//! the polynomial while it builds the crate.

/// x^16 + x^12 + x^5 + 1. The x^16 term is implied by the register width.
const POLY: u16 = 0x1021;

const fn step() {}
