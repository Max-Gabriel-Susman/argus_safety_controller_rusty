//! The console on UART0 (MIO 15 TX, MIO 14 RX), 115200 8N1, which never
//! makes a print wait for the wire.
//!
//! [`console!`] formats a message straight into the RAM ring of
//! [`argus_fw_core::console`] and returns; [`drain`], a task of its own,
//! moves bytes from the ring into the UART's 64-byte transmit FIFO only
//! while the FIFO has room. This is the C's `argus_console_printf` and
//! `argus_console_service`, with the service call moved out of the
//! superloop into a task that sleeps while there is nothing to send.
//!
//! The ring lives in a static behind a critical section (interrupts off),
//! so that any task can print and the panic and fault handlers can reach
//! it. A print holds the critical section while it formats, a few
//! microseconds for the longest line, and the drain task while it fills
//! the FIFO, at most 64 register writes.
//!
//! Only [`emergency_print`] blocks: the panic and exception handlers send
//! what the ring still holds, then their own line, waiting on the UART,
//! because the core parks right after and nothing would drain the ring.

use core::cell::RefCell;
use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicBool, Ordering};

use argus_fw_core::console::Console;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use zynq7000_hal::uart::{Tx, Uart, UartId};

/// Formats a message into the console ring, or drops and counts it if it
/// does not fit; never waits. Takes `format!`'s arguments. A line ends in
/// `\r\n`, which the [`argus_fw_core::lines`] types already include.
macro_rules! console {
    ($($arg:tt)*) => {
        $crate::console::print(::core::format_args!($($arg)*))
    };
}
pub(crate) use console;

/// The ring every print goes into.
static RING: Mutex<CriticalSectionRawMutex, RefCell<Console>> =
    Mutex::new(RefCell::new(Console::new()));

/// Raised by every print, so the drain task can sleep while the ring is
/// empty instead of polling it.
static QUEUED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Set once UART0 is configured, so [`emergency_print`] knows that writing
/// to it will put characters on the wire. Before that, nothing can be
/// printed and a panic or fault parks silently.
static UART0_READY: AtomicBool = AtomicBool::new(false);

/// How long the drain task sleeps when the FIFO is full. A character takes
/// about 87 us at 115200 8N1, so 2 ms frees about 23 of the 64 places and
/// leaves about 41 still to send: the FIFO never runs dry while the ring
/// has bytes, and the task wakes about 500 times a second only while
/// there is text to move.
const REFILL_PERIOD: Duration = Duration::from_millis(2);

/// Takes over a configured UART0 and returns the transmit half for
/// [`drain`]. The receive half is unused: the firmware reads no input.
pub fn start(uart: Uart) -> Tx {
    UART0_READY.store(true, Ordering::Release);
    let (tx, _rx) = uart.split();
    tx
}

/// Queues one message; the body of [`console!`]. A message that does not
/// fit, or reaches the C's 320-byte limit, is dropped and counted.
pub fn print(args: fmt::Arguments<'_>) {
    RING.lock(|ring| {
        // The borrow can only fail if a Display impl inside `args` prints
        // in turn; that inner message is lost, uncounted.
        if let Ok(mut ring) = ring.try_borrow_mut() {
            ring.print(args);
        }
    });
    QUEUED.signal(());
}

/// Messages dropped since boot.
pub fn drops() -> u32 {
    RING.lock(|ring| ring.try_borrow().map_or(0, |ring| ring.drops()))
}

/// The longest message queued since the last call, in bytes, or 0 if none
/// was; resets it.
pub fn take_max_msg() -> u32 {
    RING.lock(|ring| {
        ring.try_borrow_mut()
            .map_or(0, |mut ring| ring.take_max_msg())
    })
}

/// Moves the ring into the UART's transmit FIFO, for good.
#[embassy_executor::task]
pub async fn drain(mut tx: Tx) -> ! {
    loop {
        let more = RING.lock(|ring| {
            ring.try_borrow_mut()
                .is_ok_and(|mut ring| fill_fifo(&mut tx, &mut ring))
        });
        if more {
            Timer::after(REFILL_PERIOD).await;
        } else {
            QUEUED.wait().await;
        }
    }
}

/// Sends queued bytes until the ring is empty or the FIFO is full. Returns
/// whether bytes are still queued.
fn fill_fifo(tx: &mut Tx, ring: &mut Console) -> bool {
    // Two passes at most: the run up to the end of the ring, then the run
    // from its start when the queue wraps.
    loop {
        let pending = ring.pending();
        if pending.is_empty() {
            return false;
        }
        let sent = pending
            .iter()
            .take_while(|&&byte| tx.write_fifo(byte).is_ok())
            .count();
        let full = sent < pending.len();
        ring.consume(sent);
        if full {
            return true;
        }
    }
}

/// Sends what the ring still holds, then `args`, straight to UART0, and
/// waits until the last bit has left; for the panic and exception
/// handlers, which park the core right after. Does nothing if UART0 has
/// not been configured yet.
pub fn emergency_print(args: fmt::Arguments<'_>) {
    if !UART0_READY.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: UART0 is configured (UART0_READY is set only after that) and
    // the caller is about to park the core for good, so the drain task that
    // owns the other Tx will never run again. The worst a second handle can
    // do is follow a FIFO the task left half filled, which only delays this
    // line.
    let mut tx = unsafe { Tx::steal(UartId::Uart0) };
    RING.lock(|ring| {
        // If the panic was raised inside a print, the ring is borrowed and
        // may be half written: skip what it holds and print the line alone.
        if let Ok(mut ring) = ring.try_borrow_mut() {
            while fill_fifo(&mut tx, &mut ring) {}
        }
    });
    // Tx's fmt::Write waits for FIFO room and never fails.
    let _ = tx.write_fmt(args);
    // Parking does not stop the UART, but waiting here means that when the
    // handler goes on to park, the line is known to be on the wire.
    tx.flush();
}
