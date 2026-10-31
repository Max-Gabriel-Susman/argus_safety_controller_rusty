//! The acquisition chain's checks: `main.c`'s `acq_smoke_test()` at boot,
//! and the feature loop that reads every new bin after it.
//!
//! The smoke test runs before any networking on purpose: an AXI read of
//! unprogrammed PL hangs the A9 with no timeout, and if that happens it
//! should be the first thing after the banner, not buried under network
//! output. Its lines prove the 125 MHz clock, reset release, the chips'
//! init sequence, the SPI sweep, the assembler and the AXI path.
//!
//! What changed from the C: its `usleep()` calls busy-wait, these sleeps
//! are embassy-time timers, so the console drains meanwhile (the C calls
//! `argus_console_flush()` instead). The feature loop is a task that polls
//! FEATURE_INDEX every millisecond rather than on every pass of a
//! superloop; a bin is 50 ms, so a new one is seen within 2% of a bin.

use argus_fw_core::acq::{self, BinCounter};
use argus_fw_core::lines::{
    ACQ_FEATURES_HOLD_TIMEOUT, ACQ_FRAME_HOLD_TIMEOUT, AcqFeatures, AcqFrame, AcqId, AcqStatus,
    AcqTiming, Feat,
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

/// How often the feature loop looks at FEATURE_INDEX. One GP0 read a
/// millisecond costs the executor about 0.1% of its time.
const BIN_POLL: Duration = Duration::from_millis(1);

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
fn frame_check(acq: &mut Acq, ext: bool) {
    match acq.read_frame() {
        Ok((frame_index, frame)) => console!("{}", AcqFrame::new(frame_index, &frame, ext)),
        Err(_) => console!("{ACQ_FRAME_HOLD_TIMEOUT}"),
    }
}

/// `main.c`'s main loop, the acquisition half: reads every new feature bin
/// under the feature hold and, every 100 bins (5 s), prints the `feat:`
/// line and runs the frame check.
///
/// A hold that never took effect is a fabric problem, not a reason to use
/// a torn bin: the bin is skipped and counted. The telemetry frame each bin
/// becomes, and the `tx` line, are not here yet; the bins read stand in for
/// the frames sent in deciding when to report.
#[embassy_executor::task]
pub async fn feature_loop(mut acq: Acq) -> ! {
    let mut bins = BinCounter::new();
    loop {
        Timer::after(BIN_POLL).await;
        if !bins.is_new(acq.feature_index()) {
            continue;
        }
        match acq.read_features() {
            Ok(features) => {
                if bins.read(features.bin) {
                    console!("{}", Feat::new(&features, acq.feat_dropped()));
                    let ext = acq.ext_mode();
                    frame_check(&mut acq, ext);
                }
            }
            Err(_) => bins.skipped(acq.feature_index()),
        }
    }
}
