//! The text of every console line the C firmware prints.
//!
//! The harness and the person at the terminal both read these lines, so
//! each one is the C's format string rendered exactly: the same words, the
//! same field order and spacing, `%u` and `%d` as decimal, `%04x`, `%02x`
//! and `%08x` as zero-padded lowercase hex. Every line ends in `\r\n`, part
//! of the message as it is in the C, so the console's longest-message
//! figure counts the same bytes.
//!
//! A line that takes values is a struct whose [`Display`] is the line; one
//! that takes none is a `&str` constant. Either goes to the console as
//! `console.print(format_args!("{line}"))`. The struct fields are the C's
//! printf arguments, so the firmware fills them from the same registers and
//! counters the C reads.
//!
//! A few lines have no C equivalent: [`Banner`], [`Clk`], [`Heartbeat`],
//! [`PrintTest`], [`PrintTime`], [`Panic`] and [`Fault`]. They follow the
//! same rules.

use core::fmt::{self, Display, Formatter};

use argus_wire::frame::CHANNELS;

use crate::acq::{self, FrameCheck, ReplayStatus};
use crate::timing::Timing;

/// `argus-fw <rev> boot`: the first line, naming the commit the image was
/// built from, so a log says which firmware produced it. New: the C
/// firmware has no build identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Banner<'a> {
    /// The commit as 12 hex digits, with `-dirty` appended when the tree
    /// had uncommitted changes.
    pub rev: &'a str,
}

impl Display for Banner<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "argus-fw {} boot\r\n", self.rev)
    }
}

/// `clk: apu=<hz> gtc=<hz> iopll=<hz> uart=<hz>`: the clocks the firmware
/// runs on, as read from the PLL and divider registers. New: the C
/// hardcodes a 333 MHz timer, which on the Arty's 50 MHz reference is
/// really 325 MHz; this line shows what the registers say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clk {
    /// The Cortex-A9 core clock (CPU_6x4x).
    pub apu: u32,
    /// The global timer's clock (CPU_3x2x), which embassy-time counts.
    pub gtc: u32,
    /// The IO PLL's output, the source of the peripheral clocks.
    pub iopll: u32,
    /// The UART reference clock the baud divisors are computed from.
    pub uart: u32,
}

impl Display for Clk {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "clk: apu={} gtc={} iopll={} uart={}\r\n",
            self.apu, self.gtc, self.iopll, self.uart
        )
    }
}

/// `hb <n> con drop=<d> max=<m>`, every 5 s until telemetry has its own
/// lines: proof the executor and the time driver are alive, with the
/// console's drops and longest message as in the `stream: rx` line. New.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    /// Heartbeats since boot, from 1.
    pub n: u32,
    /// The console's drops since boot.
    pub con_drops: u32,
    /// The console's longest message since the last heartbeat.
    pub con_max: u32,
}

impl Display for Heartbeat {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "hb {} con drop={} max={}\r\n",
            self.n, self.con_drops, self.con_max
        )
    }
}

/// The length of [`PrintTest`], line end included.
pub const PRINT_TEST_BYTES: usize = 300;

/// `con: print test 0123456789...`: one [`PRINT_TEST_BYTES`]-byte line,
/// printed once at boot so the firmware can time how long a long print
/// holds the caller (see [`PrintTime`]). The digits repeat so that a
/// truncated or mangled copy on the terminal is easy to spot. New.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrintTest;

impl Display for PrintTest {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        const HEAD: &str = "con: print test ";
        const DIGITS: &str = "0123456789";
        f.write_str(HEAD)?;
        // Whole runs of digits, then the part of one that is left, so the
        // formatter sees a few long writes, as it does for a real line.
        let mut left = PRINT_TEST_BYTES - HEAD.len() - "\r\n".len();
        while left > 0 {
            let run = DIGITS.get(..left.min(DIGITS.len())).unwrap_or(DIGITS);
            f.write_str(run)?;
            left -= run.len();
        }
        f.write_str("\r\n")
    }
}

/// `con: <bytes>-byte print took <us> us`: how long printing [`PrintTest`]
/// kept the caller, which is the proof that a print does not wait for the
/// UART (at 115200 baud the line itself takes 26 ms on the wire). New.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrintTime {
    /// The length of the line that was timed.
    pub bytes: u32,
    /// From the call to its return, in microseconds.
    pub us: u32,
}

impl Display for PrintTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "con: {}-byte print took {} us\r\n", self.bytes, self.us)
    }
}

/// `panic: <message> at <file>:<line>`: the last line before a panic parks
/// the core. New: the C has no panics. Without a location (Rust always
/// supplies one for a panic in this code) the ` at` part is left off.
#[derive(Clone, Copy, Debug)]
pub struct Panic<'a, M> {
    /// What the panic said.
    pub message: M,
    /// Where it was raised: the source file and line.
    pub location: Option<(&'a str, u32)>,
}

impl<M: Display> Display for Panic<'_, M> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "panic: {}", self.message)?;
        if let Some((file, line)) = self.location {
            write!(f, " at {file}:{line}")?;
        }
        f.write_str("\r\n")
    }
}

/// The CPU exceptions that end in a [`Fault`] line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// An undefined instruction.
    Undefined,
    /// An instruction fetch from an address that cannot be read.
    PrefetchAbort,
    /// A load or store the memory system refused, such as an AXI read of
    /// unprogrammed PL.
    DataAbort,
}

impl FaultKind {
    /// The kind as the line names it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::PrefetchAbort => "prefetch abort",
            Self::DataAbort => "data abort",
        }
    }
}

/// `fault: <kind> at %08x`: the last line before an exception parks the
/// core. New: the C's BSP handlers loop silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    /// Which exception.
    pub kind: FaultKind,
    /// The address of the instruction that raised it.
    pub addr: u32,
}

impl Display for Fault {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "fault: {} at {:08x}\r\n", self.kind.as_str(), self.addr)
    }
}

/// The first line of the C firmware, after the D-cache is on.
pub const INITIALIZING: &str = "Initializing Argus Safety Controller...\r\n";

/// `acq_frame_check()` when the frame hold never took effect.
pub const ACQ_FRAME_HOLD_TIMEOUT: &str = "acq frame: hold never took effect\r\n";

/// `acq_smoke_test()` when the feature hold never took effect.
pub const ACQ_FEATURES_HOLD_TIMEOUT: &str = "acq features: hold never took effect\r\n";

/// After the BRAM aperture is remapped Normal write-back.
pub const REPLAY_BRAM_CACHEABLE: &str =
    "replay: BRAM aperture cacheable; halves flushed before ack\r\n";

/// The one-shot fetch ended Failed.
pub const REPLAY_FAILED: &str = "replay FAILED\r\n";

/// Streaming has begun: the first half is being fetched.
pub const STREAM_PRIMING: &str = "stream: priming\r\n";

/// The last line before the main loop.
pub const INITIALIZED: &str = "Argus Safety Controller initialized. IP 192.168.1.10\r\n";

/// After [`ReplayClock`] when the clock ran under 5 ms in a 10 ms sleep.
pub const REPLAY_CLOCK_TOO_SLOW: &str =
    "WARNING: replay clock too slow -- retransmit timeouts will not fire\r\n";

/// After [`ReplayClock`] when the clock ran over 20 ms in a 10 ms sleep.
pub const REPLAY_CLOCK_TOO_FAST: &str =
    "WARNING: replay clock too fast -- timeouts fire before replies arrive\r\n";

/// `replay clock: %u ms measured over 10 ms sleep`: the millisecond clock
/// the replay client's timeouts run on, checked against a 10 ms sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayClock {
    /// Milliseconds the clock advanced.
    pub ms: u32,
}

impl ReplayClock {
    /// The line for `ticks` of a timer counting at `hz`, in whole
    /// milliseconds rounded down, by the C's `t / (COUNTS_PER_SECOND /
    /// 1000)`.
    ///
    /// The C converts each reading to milliseconds and subtracts, so a
    /// 10.01 ms sleep reads 10 or 11 depending on where the first reading
    /// fell within its millisecond. This converts the difference of the raw
    /// readings instead, which reads 10 every time. A timer clock under
    /// 1 kHz has no millisecond divisor; the line then shows `u32::MAX` and
    /// warns that the clock is too fast, since nothing it measures can be
    /// trusted. Likewise a difference too large for a `u32`.
    pub fn from_ticks(ticks: u64, hz: u32) -> Self {
        let ms = ticks
            .checked_div(u64::from(hz / 1000))
            .and_then(|ms| u32::try_from(ms).ok())
            .unwrap_or(u32::MAX);
        Self { ms }
    }

    /// The warning to print after this line, if any: a clock that barely
    /// moves never fires a retransmit, one that races fires them before the
    /// replies can arrive.
    pub const fn warning(self) -> Option<&'static str> {
        if self.ms < 5 {
            Some(REPLAY_CLOCK_TOO_SLOW)
        } else if self.ms > 20 {
            Some(REPLAY_CLOCK_TOO_FAST)
        } else {
            None
        }
    }
}

impl Display for ReplayClock {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "replay clock: {} ms measured over 10 ms sleep\r\n",
            self.ms
        )
    }
}

/// `acq id=%08x`, with the stale-bitstream warning when the ID is not one
/// the firmware accepts.
///
/// The C expects ACQ3 alone and names the Vitis platform in its advice;
/// this accepts ACQ3 and ACQ4 and says so, and the advice fits a flow with
/// no platform to rebuild. The ID and the word `EXPECTED`, which the
/// harness looks for, are as the C prints them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcqId {
    /// The ID register.
    pub id: u32,
}

impl Display for AcqId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if acq::is_accepted_id(self.id) {
            write!(f, "acq id={:08x}\r\n", self.id)
        } else {
            write!(
                f,
                "acq id={:08x}  EXPECTED {:08x} or {:08x} -- stale bitstream? \
                 Rebuild in Vivado and re-export the XSA.\r\n",
                self.id,
                acq::ID_ACQ3,
                acq::ID_ACQ4
            )
        }
    }
}

/// `acq status=%08x frames/s=%u`: STATUS, and FRAME_INDEX's advance over one
/// second with the hold clear, the true sweep rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcqStatus {
    /// The STATUS register.
    pub status: u32,
    /// Frames counted in one second.
    pub frames_per_s: u32,
}

impl Display for AcqStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "acq status={:08x} frames/s={}\r\n",
            self.status, self.frames_per_s
        )
    }
}

/// `acq timing: ...`: what one GP0 register read and a 96-word frame read
/// cost, the figures that justify reading frames under a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcqTiming {
    /// Nanoseconds per FRAME_INDEX read.
    pub reg_ns: u32,
    /// Microseconds for FRAME\[0\] to FRAME\[95\].
    pub frame_us: u32,
}

impl AcqTiming {
    /// The figures from global timer ticks at `hz`: `reg_ticks` for 96
    /// FRAME_INDEX reads, `frame_ticks` for FRAME\[0\] to FRAME\[95\]. Both
    /// round down, as the C's integer division does.
    ///
    /// The C divides by a hardcoded 333 ticks per microsecond; the Arty's
    /// timer runs at 325 MHz, so its figures read about 2.5% low. This
    /// takes the rate the registers give. A figure that does not fit (or a
    /// zero rate) shows as `u32::MAX`.
    pub fn from_ticks(reg_ticks: u64, frame_ticks: u64, hz: u32) -> Self {
        let per_read = u64::from(hz).checked_mul(CHANNELS as u64);
        let reg_ns = reg_ticks
            .checked_mul(1_000_000_000)
            .zip(per_read)
            .and_then(|(ns, per_read)| ns.checked_div(per_read));
        let frame_us = frame_ticks
            .checked_mul(1_000_000)
            .and_then(|us| us.checked_div(u64::from(hz)));
        let fit = |v: Option<u64>| v.and_then(|v| u32::try_from(v).ok()).unwrap_or(u32::MAX);
        Self {
            reg_ns: fit(reg_ns),
            frame_us: fit(frame_us),
        }
    }
}

impl Display for AcqTiming {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "acq timing: reg {} ns/read, 96-word frame read {} us, sweep 33 us (read is held)\r\n",
            self.reg_ns, self.frame_us
        )
    }
}

/// `acq frame %u: [0]=%04x [95]=%04x idx=%02x expect=%02x bad=%d`, or with
/// `(ext)` in place of the expectation once the chips play the replay: a
/// frame read under hold and its identity check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcqFrame {
    /// FRAME_INDEX under the hold: the frame actually in the bank.
    pub frame_index: u32,
    /// Channel 0's word.
    pub first: u16,
    /// Channel 95's word.
    pub last: u16,
    /// What [`acq::check_frame`] found.
    pub check: FrameCheck,
    /// CTRL.ext_mode was set, so the index is the dataset's sample number
    /// and has no expected value.
    pub ext: bool,
}

impl AcqFrame {
    /// Checks `frame` and keeps what the line shows.
    pub fn new(frame_index: u32, frame: &[u16; CHANNELS], ext: bool) -> Self {
        let [first, ..] = frame;
        let [.., last] = frame;
        Self {
            frame_index,
            first: *first,
            last: *last,
            check: acq::check_frame(frame),
            ext,
        }
    }
}

impl Display for AcqFrame {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "acq frame {}: [0]={:04x} [{}]={:04x} idx={:02x} ",
            self.frame_index,
            self.first,
            CHANNELS - 1,
            self.last,
            self.check.idx
        )?;
        if self.ext {
            f.write_str("(ext)")?;
        } else {
            write!(f, "expect={:02x}", acq::expected_index(self.frame_index))?;
        }
        write!(f, " bad={}\r\n", self.check.bad)
    }
}

/// `acq features: bin %u  ch0 count %u power %u  ch14 count %u power %u
/// dropped %u`: the first bin read at boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcqFeatures {
    /// FEATURE_INDEX under the hold.
    pub bin: u32,
    /// Channel 0's crossing count.
    pub count0: u16,
    /// Channel 0's power.
    pub power0: u32,
    /// Channel 14's crossing count.
    pub count14: u16,
    /// Channel 14's power.
    pub power14: u32,
    /// FEAT_DROPPED.
    pub dropped: u32,
}

impl AcqFeatures {
    /// The line for `features`, with FEAT_DROPPED read as `dropped`.
    pub fn new(features: &acq::Features, dropped: u32) -> Self {
        let (count0, power0) = features.channel(0);
        let (count14, power14) = features.channel(14);
        Self {
            bin: features.bin,
            count0,
            power0,
            count14,
            power14,
            dropped,
        }
    }
}

impl Display for AcqFeatures {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "acq features: bin {}  ch0 count {} power {}  ch14 count {} power {}  dropped {}\r\n",
            self.bin, self.count0, self.power0, self.count14, self.power14, self.dropped
        )
    }
}

/// `replay ok: [0][0]=%04x [0][5]=%04x [1][0]=%04x [146][95]=%04x`: four
/// samples of the one-shot fetch, `[sample][channel]`, enough to tell a
/// shifted or truncated half from a good one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayOk {
    /// Sample 0, channel 0.
    pub s0_c0: u16,
    /// Sample 0, channel 5.
    pub s0_c5: u16,
    /// Sample 1, channel 0.
    pub s1_c0: u16,
    /// Sample 146, channel 95: the last word of the half.
    pub s146_c95: u16,
}

impl Display for ReplayOk {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "replay ok: [0][0]={:04x} [0][5]={:04x} [1][0]={:04x} [146][95]={:04x}\r\n",
            self.s0_c0, self.s0_c5, self.s1_c0, self.s146_c95
        )
    }
}

/// `replay: req=%d rtx=%d ok=%d rej=%d to=%d chunks=%d/%d`: the client's
/// counters after the one-shot fetch.
///
/// The C prints these unsigned counters with `%d` after an `(int)` cast;
/// they are printed here as the same signed values, which differ from the
/// unsigned ones only past 2^31.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayCounters {
    /// Requests sent, retransmits included.
    pub requests: u32,
    /// Retransmit requests sent.
    pub retransmits: u32,
    /// Chunks accepted.
    pub accepted: u32,
    /// Datagrams rejected.
    pub rejected: u32,
    /// Deadlines that passed with chunks missing.
    pub timeouts: u32,
    /// Chunks of the last fetch that arrived.
    pub arrived: u32,
    /// Chunks in the last fetch.
    pub total: u32,
}

impl Display for ReplayCounters {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "replay: req={} rtx={} ok={} rej={} to={} chunks={}/{}\r\n",
            as_int(self.requests),
            as_int(self.retransmits),
            as_int(self.accepted),
            as_int(self.rejected),
            as_int(self.timeouts),
            as_int(self.arrived),
            as_int(self.total)
        )
    }
}

/// The C's `(int)` cast of a `uint32_t`: the same bits, read as signed.
const fn as_int(v: u32) -> i32 {
    i32::from_ne_bytes(v.to_ne_bytes())
}

/// `tx %u bin %u skipped %u`, every 20 frames sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tx {
    /// Frames sent since boot.
    pub count: u32,
    /// The bin just sent.
    pub bin: u32,
    /// Bins skipped because the feature hold never took effect.
    pub skipped: u32,
}

impl Display for Tx {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tx {} bin {} skipped {}\r\n",
            self.count, self.bin, self.skipped
        )
    }
}

/// `stream: halves=%u underruns=%u failures=%u next=%u pl: half=%u row=%u
/// c0=%u c1=%u`: the stream's counts since it began, and where the PL is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamHalves {
    /// Halves filled and handed to the PL.
    pub halves: u32,
    /// Times the PL flipped into a half that had not been acked.
    pub underruns: u32,
    /// Fetches that failed or could not be sent.
    pub failures: u32,
    /// Dataset offset of the next fetch.
    pub next: u32,
    /// REPLAY_STATUS at the report.
    pub pl: ReplayStatus,
}

impl Display for StreamHalves {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream: halves={} underruns={} failures={} next={} pl: half={} row={} c0={} c1={}\r\n",
            self.halves,
            self.underruns,
            self.failures,
            self.next,
            self.pl.play_half(),
            self.pl.row(),
            u32::from(self.pl.consumed0()),
            u32::from(self.pl.consumed1())
        )
    }
}

/// `stream: fetch avg=%u max=%u us (n=%u)  flush avg=%u max=%u us  rtx=%u
/// to=%u rej=%u`: where a half's time goes since the last report, and the
/// client's loss counters since boot. A fetch near 200000 us is a
/// retransmit timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamFetch {
    /// Request sent to last chunk in, per half.
    pub fetch: Timing,
    /// The cache clean of a half.
    pub flush: Timing,
    /// The client's retransmits since boot.
    pub retransmits: u32,
    /// The client's timeouts since boot.
    pub timeouts: u32,
    /// The client's rejected datagrams since boot.
    pub rejected: u32,
}

impl Display for StreamFetch {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream: fetch avg={} max={} us (n={})  flush avg={} max={} us  rtx={} to={} rej={}\r\n",
            self.fetch.avg(),
            self.fetch.max(),
            self.fetch.n(),
            self.flush.avg(),
            self.flush.max(),
            self.retransmits,
            self.timeouts,
            self.rejected
        )
    }
}

/// `stream: rx n=%u copy avg=%u max=%u us  parse avg=%u max=%u us  loop
/// iters=%u pkts=%u  gap avg=%u max=%u us (n=%u)  con drop=%u max=%u`: the
/// receive path per replay datagram, the replay task's wakeups, the idle
/// gap between fetches, and the console's drops and longest message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamRx {
    /// Moving a datagram out of the socket. Its count is the line's `rx n`.
    pub copy: Timing,
    /// The client's checks, CRC and copy into the half. Recorded with
    /// `copy`, once per datagram, so their counts agree, as in the C, which
    /// keeps one count for both.
    pub parse: Timing,
    /// Wakeups of the replay task.
    pub loop_iters: u32,
    /// Datagrams those wakeups delivered.
    pub loop_pkts: u32,
    /// From one fetch completing to the next request leaving, while
    /// running.
    pub gap: Timing,
    /// The console's drops since boot.
    pub con_drops: u32,
    /// The console's longest message since the last report.
    pub con_max: u32,
}

impl Display for StreamRx {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stream: rx n={} copy avg={} max={} us  parse avg={} max={} us  \
             loop iters={} pkts={}  gap avg={} max={} us (n={})  con drop={} max={}\r\n",
            self.copy.n(),
            self.copy.avg(),
            self.copy.max(),
            self.parse.avg(),
            self.parse.max(),
            self.loop_iters,
            self.loop_pkts,
            self.gap.avg(),
            self.gap.max(),
            self.gap.n(),
            self.con_drops,
            self.con_max
        )
    }
}

/// `feat: bin %u dropped %u  ch14 count %u power %u  ch75 count %u power
/// %u`: the bin just sent, every 100 frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feat {
    /// The bin.
    pub bin: u32,
    /// FEAT_DROPPED.
    pub dropped: u32,
    /// Channel 14's crossing count.
    pub count14: u16,
    /// Channel 14's power.
    pub power14: u32,
    /// Channel 75's crossing count.
    pub count75: u16,
    /// Channel 75's power.
    pub power75: u32,
}

impl Feat {
    /// The line for `features`, with FEAT_DROPPED read as `dropped`.
    pub fn new(features: &acq::Features, dropped: u32) -> Self {
        let (count14, power14) = features.channel(14);
        let (count75, power75) = features.channel(75);
        Self {
            bin: features.bin,
            dropped,
            count14,
            power14,
            count75,
            power75,
        }
    }
}

impl Display for Feat {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "feat: bin {} dropped {}  ch14 count {} power {}  ch75 count {} power {}\r\n",
            self.bin, self.dropped, self.count14, self.power14, self.count75, self.power75
        )
    }
}
