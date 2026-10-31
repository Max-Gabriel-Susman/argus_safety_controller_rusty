//! The console on UART0 (MIO 15 TX, MIO 14 RX), 115200 8N1.
//!
//! For now every print waits for the UART: once its 64-byte FIFO is full,
//! each further character holds the executor for about 87 us, so a burst
//! of boot lines holds it for tens of milliseconds. That is harmless while
//! nothing else runs,
//! and is replaced by the non-blocking ring of `argus_fw_core::console`
//! before the replay stream needs the time.
//!
//! [`emergency_print`] is the way out for the panic and exception handlers,
//! which cannot reach the [`Console`] the main task owns.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicBool, Ordering};

use zynq7000_hal::uart::{Tx, Uart, UartId};

/// Set once UART0 is configured, so [`emergency_print`] knows that writing
/// to it will put characters on the wire. Before that, nothing can be
/// printed and a panic or fault parks silently.
static UART0_READY: AtomicBool = AtomicBool::new(false);

/// The console, and the two figures the `hb` and `stream: rx` lines report
/// about it.
pub struct Console {
    uart: Uart,
    /// Longest message printed since the last [`take_max_msg`](Self::take_max_msg).
    max_msg: u32,
}

impl Console {
    /// Takes over a configured UART0.
    pub fn new(uart: Uart) -> Self {
        UART0_READY.store(true, Ordering::Release);
        Self { uart, max_msg: 0 }
    }

    /// Prints one message, waiting for the UART to take every byte.
    pub fn print(&mut self, args: fmt::Arguments<'_>) {
        let mut counted = Counted {
            uart: &mut self.uart,
            written: 0,
        };
        // Writing to the UART cannot fail; an error here could only come
        // from a Display impl, and what was printed of the line stays.
        let _ = counted.write_fmt(args);
        let n = u32::try_from(counted.written).unwrap_or(u32::MAX);
        self.max_msg = self.max_msg.max(n);
    }

    /// Messages dropped since boot. A console that waits drops none.
    pub const fn drops(&self) -> u32 {
        0
    }

    /// The longest message printed since the last call, in bytes, or 0 if
    /// none was; resets it. Counted as the ring counts it, so the figure
    /// means the same once the console stops waiting.
    pub fn take_max_msg(&mut self) -> u32 {
        core::mem::take(&mut self.max_msg)
    }
}

/// Passes text to the UART and counts the bytes.
struct Counted<'a> {
    uart: &'a mut Uart,
    written: usize,
}

impl fmt::Write for Counted<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.uart.write_all(s.as_bytes());
        self.written = self.written.saturating_add(s.len());
        Ok(())
    }
}

/// Prints `args` straight to UART0 and waits until the last bit has left,
/// for the panic and exception handlers, which run when the [`Console`] may
/// be anywhere (even in the middle of a line). Does nothing if UART0 has
/// not been configured yet.
pub fn emergency_print(args: fmt::Arguments<'_>) {
    if !UART0_READY.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: UART0 is configured (UART0_READY is set only after that) and
    // the caller is about to park the core for good, so the Console that
    // owns the UART will never run again. The worst a second handle can do
    // is interleave this line with a half-written one, which is acceptable
    // for the last words before a stop.
    let mut tx = unsafe { Tx::steal(UartId::Uart0) };
    // Tx's fmt::Write waits for FIFO room and never fails.
    let _ = tx.write_fmt(args);
    // Parking does not stop the UART, but waiting here means that when the
    // handler goes on to park, the line is known to be on the wire.
    tx.flush();
}
