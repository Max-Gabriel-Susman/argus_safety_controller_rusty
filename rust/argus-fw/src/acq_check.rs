//! The acquisition chain's checks: `main.c`'s `acq_smoke_test()` at boot,
//! and the frame check the telemetry task repeats every 100 bins.
//!
//! The smoke test runs before any networking on purpose: an AXI read of
//! unprogrammed PL hangs the A9 with no timeout, and if that happens it
//! should be the first thing after the banner, not buried under network
//! output. Its lines prove the 125 MHz clock, reset release, the chips'
//! init sequence, the SPI sweep, the assembler and the AXI path.
//!
//! What changed from the C: its `usleep()` calls busy-wait, these sleeps
//! are embassy-time timers, so the console drains meanwhile (the C calls
//! `argus_console_flush()` instead).

use argus_fw_core::acq;
use argus_fw_core::lines::{
    ACQ_FEATURES_HOLD_TIMEOUT, ACQ_FRAME_HOLD_TIMEOUT, AcqFeatures, AcqFrame, AcqId, AcqStatus,
    AcqTiming,
};
use argus_wire::frame::CHANNELS;
use embassy_time::{Duration, Timer};
use zynq7000_hal::time::Hertz;

use crate::acq::Acq;
use crate::console::console;
use crate::read_global_timer;

/// How long the chain runs after enable before it is measured, as the C.
const SETTLE: Duration = Duration::from_millis(50);

/// The window the sweep rate is counted over: frames/s is FRAME_INDEX's
/// advance across it.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// The smoke test's wait for the first feature bin: at most this many
/// polls, one per [`FIRST_BIN_POLL`], as the C's 100 x `usleep(1000)`.
const FIRST_BIN_POLLS: u32 = 100;

/// See [`FIRST_BIN_POLLS`].
const FIRST_BIN_POLL: Duration = Duration::from_millis(1);

/// The C's `acq_smoke_test()`: the ID line, enable, the sweep rate and
/// STATUS, the read timing, one frame check and the first bin's features.
///
/// A wrong ID is reported and the test goes on, as in the C: the lines
/// after it are then the evidence of what the stale fabric does.
pub async fn smoke_test(acq: &mut Acq, gtc_hz: Hertz) {
    console!("{}", AcqId { id: acq.id() });

    acq.write_ctrl(acq::ctrl::ENABLE);
    Timer::after(SETTLE).await;

    // Measured with the hold clear, so this is the true sweep rate.
    let f0 = acq.frame_index();
    Timer::after(RATE_WINDOW).await;
    let f1 = acq.frame_index();
    console!(
        "{}",
        AcqStatus {
            status: acq.status(),
            frames_per_s: f1.wrapping_sub(f0),
        }
    );

    console!("{}", time_reads(acq, gtc_hz));
    frame_check(acq, false);

    // One bin is 50 ms, so the first swap has long happened after the
    // second above; the wait only matters on a fabric that is not running.
    // Counts are zero until the chips' 1.09 s warm-up has passed; power is
    // live from bin 0.
    for _ in 0..FIRST_BIN_POLLS {
        if acq.feature_index() != 0 {
            break;
        }
        Timer::after(FIRST_BIN_POLL).await;
    }
    match acq.read_features() {
        Ok(features) => console!("{}", AcqFeatures::new(&features, acq.feat_dropped())),
        Err(_) => console!("{ACQ_FEATURES_HOLD_TIMEOUT}"),
    }
}

/// The C's `acq_timing_report()`: how long 96 FRAME_INDEX reads and one
/// 96-word frame read take, on the global timer.
///
/// Kept because it is the number that justifies the hold, and a slower
/// interconnect after a block design change would otherwise be invisible.
/// A FRAME word comes through the assembler's registered read port and
/// FRAME_INDEX does not, so the two figures separate that port from the
/// GP0 round trip. The reads are volatile, so none is optimized away; the
/// C's figures also include its -O0 loop, these hardly any loop at all.
fn time_reads(acq: &Acq, gtc_hz: Hertz) -> AcqTiming {
    let t0 = read_global_timer();
    for _ in 0..CHANNELS {
        acq.frame_index();
    }
    let t1 = read_global_timer();
    for n in 0..CHANNELS {
        acq.frame(n);
    }
    let t2 = read_global_timer();
    AcqTiming::from_ticks(t1.wrapping_sub(t0), t2.wrapping_sub(t1), gtc_hz.to_raw())
}

/// The C's `acq_frame_check()`: one frame under the hold, its identity
/// check, and the `acq frame` line. `ext` says the chips play the replay,
/// whose sample index has no expected value.
pub fn frame_check(acq: &mut Acq, ext: bool) {
    match acq.read_frame() {
        Ok((frame_index, frame)) => console!("{}", AcqFrame::new(frame_index, &frame, ext)),
        Err(_) => console!("{ACQ_FRAME_HOLD_TIMEOUT}"),
    }
}
