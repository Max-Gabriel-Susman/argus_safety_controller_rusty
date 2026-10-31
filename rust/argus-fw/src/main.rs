//! The Argus safety controller firmware, for the Zynq-7020 on the Arty Z7-20.
//!
//! For now, the smallest firmware that brings up the HAL, embassy-time and
//! the console: zynq7000-rs's `embassy-hello` example with the Arty's 50 MHz
//! PS reference clock and its console (UART0 on MIO 15/14). It prints the
//! banner the harness anchors on and a heartbeat. Errors park the core rather
//! than unwrapping, because the workspace lints deny `unwrap`; printing why
//! comes with the panic and fault handlers.

#![no_std]
#![no_main]

use aarch32_cpu::asm::nop;
use core::panic::PanicInfo;
use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use zynq7000_hal::{clocks, generic_interrupt_handler, gpio, gtc, time::Hertz, uart};

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
async fn main(_spawner: Spawner) -> ! {
    let Ok(periphs) = zynq7000_hal::init(zynq7000_hal::Config::default()) else {
        park()
    };
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
    let Ok(mut console) = uart::Uart::new_with_mio_for_uart_0(
        periphs.uart_0,
        uart::Config::new_with_clk_config(uart_clocks),
        (mio.mio15, mio.mio14),
    ) else {
        park()
    };

    console.write_all(b"argus-fw ");
    console.write_all(REV.as_bytes());
    console.write_all(b" boot\r\nInitializing Argus Safety Controller...\r\n");

    let mut ticker = Ticker::every(Duration::from_secs(5));
    let mut beats: u32 = 0;
    loop {
        ticker.next().await;
        beats = beats.wrapping_add(1);
        let mut digits = [0u8; 10];
        console.write_all(b"hb ");
        console.write_all(decimal(beats, &mut digits));
        console.write_all(b"\r\n");
    }
}

/// `n` in decimal, written into the end of `buf` (ten digits hold any u32).
fn decimal(mut n: u32, buf: &mut [u8; 10]) -> &[u8] {
    let mut start = buf.len();
    for slot in buf.iter_mut().rev() {
        *slot = b'0' + u8::try_from(n % 10).unwrap_or(0);
        start -= 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    buf.get(start..).unwrap_or(&[])
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

#[zynq7000_rt::exception(DataAbort)]
fn data_abort_handler(_faulting_addr: usize) -> ! {
    park()
}

#[zynq7000_rt::exception(Undefined)]
fn undefined_handler(_faulting_addr: usize) -> ! {
    park()
}

#[zynq7000_rt::exception(PrefetchAbort)]
fn prefetch_handler(_faulting_addr: usize) -> ! {
    park()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    park()
}
