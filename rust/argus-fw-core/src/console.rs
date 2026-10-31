//! The ring behind the non-blocking console, ported from `argus_console.c`.
//!
//! At 115200 8N1 a character takes about 87 us on the wire, and the replay
//! stream has about 2 ms of slack per half, so a print that waits for the
//! UART costs an underrun. A message is instead formatted into this RAM
//! ring at once, and the console task moves bytes from [`Console::pending`]
//! into the UART's transmit FIFO only while it has room.
//!
//! A message goes in whole or not at all. One that does not fit the free
//! space, or that reaches [`MSG_MAX`] bytes, is dropped and counted, so a
//! busy console loses lines rather than printing halves of them.
//!
//! Where the C formats into a 320-byte stack buffer and then copies, this
//! formats straight into the ring's free space and only commits the bytes
//! once the whole message is written. The outcome is the same, without the
//! second copy or the buffer.

use core::fmt;

/// Size of the ring in bytes. A power of two, so the free-running indices
/// wrap into it with a mask.
pub const RING_BYTES: usize = 4096;
const _: () = assert!(RING_BYTES.is_power_of_two());

/// The C's formatting buffer size. A message of this many bytes or more is
/// dropped, so the longest that can be queued is one less (the C's buffer
/// also holds the terminating NUL).
pub const MSG_MAX: usize = 320;

/// Turns a free-running index into a place in the ring. Written as a
/// literal because the indices are `u32` and `RING_BYTES` a `usize`; the
/// assert ties the two together.
const MASK: u32 = 4095;
const _: () = assert!(MASK as usize + 1 == RING_BYTES);

/// The console ring and its two counters.
pub struct Console {
    ring: [u8; RING_BYTES],
    /// Next byte written; free-running, wraps at 2^32.
    head: u32,
    /// Next byte sent; free-running.
    tail: u32,
    /// Messages dropped since boot.
    drops: u32,
    /// Longest message queued since the last [`take_max_msg`](Self::take_max_msg).
    max_msg: u32,
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

impl Console {
    /// An empty ring. `const`, so the firmware can keep one in a static.
    pub const fn new() -> Self {
        Self {
            ring: [0; RING_BYTES],
            head: 0,
            tail: 0,
            drops: 0,
            max_msg: 0,
        }
    }

    /// Bytes queued and not yet taken.
    pub const fn len(&self) -> usize {
        self.head.wrapping_sub(self.tail) as usize
    }

    /// Whether every queued byte has been taken.
    pub const fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// Bytes free for new messages.
    pub const fn free(&self) -> usize {
        RING_BYTES - self.len()
    }

    /// Queues `msg` whole, or drops and counts it. Returns whether it was
    /// queued.
    pub fn push(&mut self, msg: &[u8]) -> bool {
        self.queue(|w| w.write_bytes(msg))
    }

    /// Formats `args` into the ring, or drops and counts the message if it
    /// does not fit or the formatting fails. Returns whether it was queued.
    pub fn print(&mut self, args: fmt::Arguments<'_>) -> bool {
        self.queue(|w| fmt::write(w, args))
    }

    /// Runs `write` against the free space and commits what it wrote, or
    /// drops the message if it failed or ran out of room.
    fn queue(&mut self, write: impl FnOnce(&mut RingWriter<'_>) -> fmt::Result) -> bool {
        // The limit is the C's two checks in one number: under MSG_MAX, and
        // no more than the free space.
        let limit = (MSG_MAX - 1).min(self.free());
        let mut w = RingWriter {
            ring: &mut self.ring,
            at: self.head,
            written: 0,
            limit,
        };
        if write(&mut w).is_err() {
            self.drops = self.drops.wrapping_add(1);
            return false;
        }
        // written <= MSG_MAX - 1, so it fits a u32.
        let n = u32::try_from(w.written).unwrap_or(0);
        self.head = self.head.wrapping_add(n);
        self.max_msg = self.max_msg.max(n);
        true
    }

    /// The oldest queued bytes, as one contiguous run: up to the newest
    /// byte, or to the end of the ring if the queue wraps. Send some or all
    /// of them, then [`consume`](Self::consume) that many.
    pub fn pending(&self) -> &[u8] {
        let start = (self.tail & MASK) as usize;
        let run = self.len().min(RING_BYTES - start);
        self.ring.get(start..start + run).unwrap_or(&[])
    }

    /// Marks the first `n` bytes of [`pending`](Self::pending) as sent. More
    /// than are queued is taken as all of them.
    pub fn consume(&mut self, n: usize) {
        let n = u32::try_from(n.min(self.len())).unwrap_or(0);
        self.tail = self.tail.wrapping_add(n);
    }

    /// Messages dropped since boot, because they did not fit the free space
    /// or reached [`MSG_MAX`] bytes.
    pub const fn drops(&self) -> u32 {
        self.drops
    }

    /// The longest message queued since the last call, in bytes, or 0 if
    /// none was; resets it.
    pub fn take_max_msg(&mut self) -> u32 {
        core::mem::take(&mut self.max_msg)
    }
}

/// Writes into the ring from `at` without moving the head, and fails as
/// soon as the message would exceed `limit` bytes.
struct RingWriter<'a> {
    ring: &'a mut [u8; RING_BYTES],
    at: u32,
    written: usize,
    limit: usize,
}

impl RingWriter<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) -> fmt::Result {
        if bytes.len() > self.limit - self.written {
            return Err(fmt::Error);
        }
        for &b in bytes {
            let slot = (self.at & MASK) as usize;
            let cell = self.ring.get_mut(slot).ok_or(fmt::Error)?;
            *cell = b;
            self.at = self.at.wrapping_add(1);
        }
        self.written += bytes.len();
        Ok(())
    }
}

impl fmt::Write for RingWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes())
    }
}
