//! The Argus safety controller firmware, for the Zynq-7020 on the Arty Z7-20.
//!
//! For now: the HAL, embassy-time on the global timer and the console. It
//! prints the banner the harness anchors on, the C's first line, the clocks
//! as the registers set them, and then a heartbeat every 5 s. A panic or a
//! CPU exception prints one line saying what and where, then parks the
//! core.
//!
//! Errors before the console exists park the core silently, because there
//! is nowhere to say anything; the workspace lints deny `unwrap`, so each
//! is handled explicitly rather than by panicking.

#![no_std]
#![no_main]

mod console;

use aarch32_cpu::asm::nop;
use argus_fw_core::lines::{Banner, Clk, Fault, FaultKind, Heartbeat, INITIALIZING, Panic};
use core::panic::PanicInfo;
use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use zynq7000_hal::{clocks, generic_interrupt_handler, gpio, gtc, time::Hertz, uart};

use crate::console::{Console, emergency_print};

// Linked for its reset vector, exception vectors and MMU table; nothing in it
// is called by name.
use zynq7000_rt as _;

/// The Arty Z7-20's PS reference clock (the Zedboard's is 33.33 MHz). The HAL
/// derives every other clock from it and the PLL and divider registers.
const PS_CLOCK_FREQUENCY: Hertz = Hertz::from_raw(50_000_000);

/// The commit this image was built from, set by build.rs.
const REV: &str = env!("ARGUS_FW_REV");

/// How often the heartbeat line is printed.
const HEARTBEAT_PERIOD: Duration = Duration::from_secs(5);

#[zynq7000_rt::entry]
fn entry_point() -> ! {
    main();
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) -> ! {
    let Ok(periphs) = zynq7000_hal::init(zynq7000_hal::Config::default()) else {
        park()
    };
    // ps7_init has set the PLLs and dividers; this reads them back.
    let Ok(clocks) = clocks::Clocks::new_from_regs(PS_CLOCK_FREQUENCY) else {
        park()
    };

    // The global timer drives embassy-time.
    let gtc = gtc::GlobalTimerCounter::new(periphs.gtc, clocks.arm_clocks());
    zynq7000_hal::time_driver_gtc::init(clocks.arm_clocks(), gtc);

    let mio = gpio::mio::Pins::new(periphs.gpio);
    let Ok((uart_clocks, _baud_error)) =
        uart::ClockConfig::new_autocalc_with_error(clocks.io_clocks(), 115_200)
    else {
        park()
    };
    let Ok(uart) = uart::Uart::new_with_mio_for_uart_0(
        periphs.uart_0,
        uart::Config::new_with_clk_config(uart_clocks),
        (mio.mio15, mio.mio14),
    ) else {
        park()
    };
    let mut console = Console::new(uart);

    console.print(format_args!("{}", Banner { rev: REV }));
    console.print(format_args!("{INITIALIZING}"));
    console.print(format_args!(
        "{}",
        Clk {
            apu: clocks.arm_clocks().cpu_6x4x_clk().to_raw(),
            gtc: clocks.arm_clocks().cpu_3x2x_clk().to_raw(),
            iopll: clocks.io_clocks().ref_clk().to_raw(),
            uart: clocks.io_clocks().uart_clk().to_raw(),
        }
    ));

    let mut ticker = Ticker::every(HEARTBEAT_PERIOD);
    let mut beats: u32 = 0;
    loop {
        ticker.next().await;
        beats = beats.wrapping_add(1);
        let line = Heartbeat {
            n: beats,
            con_drops: console.drops(),
            con_max: console.take_max_msg(),
        };
        console.print(format_args!("{line}"));
    }
}

/// Stop here for good.
fn park() -> ! {
    loop {
        nop();
    }
}

#[zynq7000_rt::irq]
pub fn irq_handler() {
    // SAFETY: called only from the IRQ vector, once per interrupt, which is
    // the contract generic_interrupt_handler documents.
    let _ = unsafe { generic_interrupt_handler() };
}

/// Prints the `fault:` line for an exception at `addr`, then parks.
fn fault(kind: FaultKind, addr: usize) -> ! {
    // usize is 32 bits on the A9, so the address always fits.
    let addr = u32::try_from(addr).unwrap_or(u32::MAX);
    emergency_print(format_args!("{}", Fault { kind, addr }));
    park()
}

// The exception attribute moves each body into a trampoline whose parameter
// is always named `addr` (the address of the faulting instruction), so that
// is the name the bodies must use, whatever the signature says.

#[zynq7000_rt::exception(DataAbort)]
fn data_abort_handler(addr: usize) -> ! {
    fault(FaultKind::DataAbort, addr)
}

#[zynq7000_rt::exception(Undefined)]
fn undefined_handler(addr: usize) -> ! {
    fault(FaultKind::Undefined, addr)
}

#[zynq7000_rt::exception(PrefetchAbort)]
fn prefetch_handler(addr: usize) -> ! {
    fault(FaultKind::PrefetchAbort, addr)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let line = Panic {
        message: info.message(),
        location: info.location().map(|l| (l.file(), l.line())),
    };
    emergency_print(format_args!("{line}"));
    park()
}
