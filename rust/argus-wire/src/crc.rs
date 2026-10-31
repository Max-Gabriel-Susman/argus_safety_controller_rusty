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

/// What the register holds before the first byte.
const INIT: u16 = 0xFFFF;

/// One byte through the shift register, a bit at a time: the textbook
/// definition, and the only place the polynomial is applied.
///
/// A `const fn` can be run by the compiler during the build as well as by
/// the CPU at runtime. The table below is built that way.
const fn step(mut crc: u16, byte: u8) -> u16 {
    // `as` rather than u16::from(byte): trait methods such as From::from
    // cannot be called in a const fn. Widening u8 to u16 loses nothing.
    crc ^= (byte as u16) << 8;
    // `for` loops are not allowed in a const fn either; `while` is.
    let mut bit = 0;
    while bit < 8 {
        crc = if (crc & 0x8000) == 0 {
            crc << 1
        } else {
            (crc << 1) ^ POLY
        };
        bit += 1;
    }
    crc
}

/// Entry `i` is `step(0, i)`: what eight bit-serial steps do to a top byte
/// of `i`. One lookup replaces those eight steps, which is what took the
/// replay parse on the PS from about 450 us to about 60 us per chunk.
///
/// `static` gives exactly one copy at a fixed address, in read-only memory:
/// 512 bytes, computed at compile time, no startup cost.
static TABLE: [u16; 256] = build_table();

#[expect(
    clippy::indexing_slicing,
    reason = "const-evaluated: an out-of-range index fails the build instead of panicking"
)]
const fn build_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    // A u8 holds 0..=255 and cannot reach 256, so `while byte < 256` would
    // not even compile. Count with the u8 and stop after 255 instead.
    let mut byte: u8 = 0;
    loop {
        table[byte as usize] = step(0, byte);
        if byte == u8::MAX {
            // `loop` is an expression: `break value` is what it evaluates
            // to, and here that is also the function's return value.
            break table;
        }
        byte += 1;
    }
}

/// A CRC-16/CCITT-FALSE computation in progress.
///
/// Start with [`Crc16::new`], feed bytes with [`Crc16::update`] as many times
/// as needed, then read the result with [`Crc16::finish`]. The pieces do not
/// have to be contiguous in memory, which is how a replay chunk is covered:
/// the header up to its `crc` field, then the payload after the field.
///
/// ```
/// use argus_wire::crc::{Crc16, checksum};
///
/// let resumed = Crc16::new().update(b"1234").update(b"56789").finish();
/// assert_eq!(resumed, 0x29B1);
/// assert_eq!(resumed, checksum(b"123456789"));
/// ```
///
/// `update` takes the register by value and returns the new one; nothing is
/// modified in place. `#[must_use]` turns the easy mistake, writing
/// `crc.update(data);` and dropping the result, into a compiler warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct Crc16(u16);

impl Crc16 {
    /// A register holding the initial value, 0xFFFF.
    pub const fn new() -> Self {
        Self(INIT)
    }

    /// Runs `data` through the register, one table lookup per byte.
    #[expect(
        clippy::indexing_slicing,
        reason = "a u8 index into a 256-entry table is always in range"
    )]
    pub const fn update(self, mut data: &[u8]) -> Self {
        let mut crc = self.0;
        // Slice pattern: take the first byte, keep the rest. Nothing indexes
        // into `data`, so nothing can read past its end.
        while let [byte, rest @ ..] = data {
            let index = ((crc >> 8) as u8) ^ *byte;
            crc = (crc << 8) ^ TABLE[index as usize];
            data = rest;
        }
        Self(crc)
    }

    /// The checksum. CCITT-FALSE has no final XOR, so this is the register as
    /// it stands; going through `finish` keeps callers from depending on that.
    #[must_use]
    pub const fn finish(self) -> u16 {
        self.0
    }
}

/// Written out by hand on purpose. `#[derive(Default)]` would start the
/// register at 0 instead of 0xFFFF, which is a different CRC with the same
/// polynomial (CRC-16/XMODEM, check value 0x31C3): it would compile, run,
/// and reject every packet.
impl Default for Crc16 {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC-16/CCITT-FALSE of `data` in one call. Same as `crc16_ccitt()`.
#[must_use]
pub const fn checksum(data: &[u8]) -> u16 {
    Crc16::new().update(data).finish()
}

// Evaluated by the compiler on every build, for every target, including the
// Cortex-A9 cross-build. If the table or the update loop is wrong, the crate
// does not compile.
const _: () = assert!(checksum(b"123456789") == 0x29B1);

#[cfg(test)]
mod tests {
    use super::*;

    /// The definition with no table: every byte through eight bit-serial
    /// steps. Same as `crc16_ccitt_update_bitwise()` in `argus_wire.h`.
    fn bitwise(start: Crc16, data: &[u8]) -> Crc16 {
        Crc16(data.iter().fold(start.0, |crc, &byte| step(crc, byte)))
    }

    #[test]
    fn table_matches_argus_wire_h() {
        // Spot checks against argus_crc16_ccitt_table[] in argus_wire.h.
        assert_eq!(TABLE.first(), Some(&0x0000));
        assert_eq!(TABLE.get(0x01), Some(&0x1021));
        assert_eq!(TABLE.get(0x80), Some(&0x9188));
        assert_eq!(TABLE.last(), Some(&0x1EF0));
    }

    #[test]
    fn resumes_at_every_split() {
        let data = b"123456789";
        for split in 0..=data.len() {
            let (head, tail) = data.split_at(split);
            let crc = Crc16::new().update(head).update(tail).finish();
            assert_eq!(crc, 0x29B1, "split at {split}");
        }
    }

    #[test]
    fn table_driven_matches_bitwise() {
        // 1600 bytes of xorshift32 output, seeded with the frame magic
        // 0x41524753 ("ARGS"). Longer than the largest replay chunk.
        let mut state: u32 = 0x4152_4753;
        let mut buf = [0u8; 1600];
        for byte in &mut buf {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [low, ..] = state.to_le_bytes();
            *byte = low;
        }

        // Every length from 0 to 1600, each also split a third of the way in.
        let start = Crc16::new();
        for len in 0..=buf.len() {
            let (data, _) = buf.split_at(len);
            let (head, tail) = data.split_at(len / 3);
            assert_eq!(start.update(data), bitwise(start, data), "len {len}");
            assert_eq!(
                start.update(head).update(tail),
                bitwise(bitwise(start, head), tail),
                "len {len}, split at {}",
                len / 3
            );
        }
    }

    #[test]
    fn default_is_new_not_zero() {
        assert_eq!(Crc16::default(), Crc16::new());
        assert_ne!(Crc16::default().finish(), 0);
    }
}
