//! The Argus safety controller firmware, for the Zynq-7020 on the Arty Z7-20.
//!
//! For now: the HAL, embassy-time on the global timer and the console. It
//! prints the banner the harness anchors on, the C's first line, the clocks
//! as the registers set them, a timed long print and the C's replay clock
//! check; runs the C's acquisition smoke test; brings up GEM0 and the IP
//! stack (it answers ping at 192.168.1.10); then starts the telemetry task,
//! which sends every feature bin to the host, and the replay task, which
//! fetches one half from the relay and then keeps the PL's replay BRAM fed
//! (the chips then play the dataset instead of their built-in pattern).
//! Every print goes through the console
//! ring and returns at once; a task of its own feeds the UART. A panic or a
//! CPU exception prints one line saying what and where, then parks the
//! core.
//!
//! Errors before the console exists park the core silently, because there
//! is nowhere to say anything; the workspace lints deny `unwrap`, so each
//! is handled explicitly rather than by panicking.

#![no_std]
#![no_main]

mod acq;
mod acq_check;
mod bram;
mod console;
mod net;
mod replay;
mod telemetry;

use aarch32_cpu::asm::nop;
use argus_fw_core::lines::{
    Banner, Clk, Fault, FaultKind, INITIALIZING, PRINT_TEST_BYTES, Panic, PrintTest, PrintTime,
    REPLAY_BRAM_CACHEABLE, REPLAY_INIT_FAILED, ReplayClock,
};
use core::panic::PanicInfo;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use zynq7000_hal::{clocks, generic_interrupt_handler, gpio, gtc, time::Hertz, uart};

use crate::console::{console, emergency_print};

// Linked for its reset vector, exception vectors and MMU table; nothing in it
// is called by name.
use zynq7000_rt as _;

/// The Arty Z7-20's PS reference clock (the Zedboard's is 33.33 MHz). The HAL
/// derives every other clock from it and the PLL and divider registers.
const PS_CLOCK_FREQUENCY: Hertz = Hertz::from_raw(50_000_000);

/// The commit this image was built from, set by build.rs.
const REV: &str = env!("ARGUS_FW_REV");

#[zynq7000_rt::entry]
fn entry_point() -> ! {
    main();
}

#[embassy_executor::main]
async fn main(spawner: Spawner) -> ! {
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
    let tx = console::start(uart);
    let Ok(drain) = console::drain(tx) else {
        park()
    };
    spawner.spawn(drain);

    console!("{}", Banner { rev: REV });
    console!("{INITIALIZING}");
    console!(
        "{}",
        Clk {
            apu: clocks.arm_clocks().cpu_6x4x_clk().to_raw(),
            gtc: clocks.arm_clocks().cpu_3x2x_clk().to_raw(),
            iopll: clocks.io_clocks().ref_clk().to_raw(),
            uart: clocks.io_clocks().uart_clk().to_raw(),
        }
    );
    time_a_long_print();
    check_clock(clocks.arm_clocks().cpu_3x2x_clk()).await;

    // Before any networking: see acq_check.
    let Some(mut acq) = acq::Acq::take() else {
        park()
    };
    acq_check::smoke_test(&mut acq, clocks.arm_clocks().cpu_3x2x_clk()).await;

    // From here the telemetry and replay tasks share the registers.
    let Some(acq) = acq::share(acq) else { park() };

    // When there is no network (no PHY found) the bins are still read and
    // reported, and every frame counts as unsent.
    let stack = net::start(spawner, periphs.eth_0, mio.mio9, &clocks).await;
    let link = stack.and_then(|stack| Some(telemetry::Link::new(stack, telemetry::socket(stack)?)));
    let Ok(telemetry) = telemetry::telemetry(acq, link) else {
        park()
    };
    spawner.spawn(telemetry);

    // argus_replay_init(). The C stops at a replay init failure; here
    // telemetry carries on.
    if let Some(stack) = stack {
        let bram = bram::ReplayBram::take();
        if bram.is_some() {
            console!("{REPLAY_BRAM_CACHEABLE}");
        }
        let task = replay::socket(stack)
            .zip(argus_fw_core::replay::client())
            .zip(bram)
            .and_then(|((socket, client), bram)| {
                replay::replay(stack, socket, client, acq, bram).ok()
            });
        match task {
            Some(task) => spawner.spawn(task),
            None => console!("{REPLAY_INIT_FAILED}"),
        }
    }

    // Every job is a task now; this one has nothing left to do.
    loop {
        core::future::pending::<()>().await;
    }
}

/// Prints the 300-byte test line and then how long that print took, the
/// evidence that no print waits for the UART (the line is 26 ms of wire
/// time). New: the C has no such check.
fn time_a_long_print() {
    let start = Instant::now();
    console!("{PrintTest}");
    let us = start.elapsed().as_micros();
    console!(
        "{}",
        PrintTime {
            bytes: u32::try_from(PRINT_TEST_BYTES).unwrap_or(u32::MAX),
            us: u32::try_from(us).unwrap_or(u32::MAX),
        }
    );
}

/// The C's `argus_check_clock()`: sleeps 10 ms and prints how far the
/// global timer moved meanwhile, with a warning when that is outside 5 to
/// 20 ms. If the clock the replay client times out on did not advance, a
/// lost chunk would hang a fetch instead of being retransmitted.
///
/// The C busy-waits in `usleep()` and reads the same timer it slept on.
/// Here the sleep is an embassy-time timer, so the executor runs the other
/// tasks meanwhile, and the measurement is the raw global timer at its
/// clock as the registers give it (`gtc_hz`). The two agree only if the
/// time driver's tick conversion is right, which is what this checks.
async fn check_clock(gtc_hz: Hertz) {
    let start = read_global_timer();
    Timer::after(Duration::from_millis(10)).await;
    let ticks = read_global_timer().wrapping_sub(start);

    let line = ReplayClock::from_ticks(ticks, gtc_hz.to_raw());
    console!("{line}");
    if let Some(warning) = line.warning() {
        console!("{warning}");
    }
}

/// The global timer's 64-bit count. It counts CPU_3x2x cycles: the time
/// driver set its prescaler to 0 when it took the timer.
pub(crate) fn read_global_timer() -> u64 {
    // SAFETY: this handle only reads the two count registers, which the
    // time driver never writes after enabling the timer; reading them races
    // with nothing (the HAL's own `now()` steals the timer the same way).
    // The clock argument is unused by `read_timer`.
    let gtc = unsafe { gtc::GlobalTimerCounter::steal_fixed(None) };
    gtc.read_timer()
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
