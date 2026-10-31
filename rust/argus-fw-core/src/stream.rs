//! The streaming state machine of `argus_replay.c`: keeps the PL's
//! ping-pong BRAM fed from the relay.
//!
//! The PL plays one half while the PS refills the other. The PL reports a
//! half consumed when it flips out of it; the PS fetches the next block of
//! samples straight into that half, makes it visible to the PL (a cache
//! clean, since the aperture is mapped write-back) and acks. If the PL flips
//! back into a half before its ack, it sets underrun and keeps playing stale
//! data; that is counted, not fatal.
//!
//! The stream primes half 0, then half 1, then puts the chain into external
//! mode with a soft reset, so playback starts at row 0 of half 0, and runs.
//! At most one fetch is in flight. A failed fetch retries the same half at
//! the same offset.
//!
//! Nothing here touches the hardware or the network. The firmware hands in
//! the PL ([`Pl`]: REPLAY_STATUS, REPLAY_ACK, CTRL and the cache clean), the
//! fetch ([`Fetch`]: the replay client and its socket) and a microsecond
//! [`Clock`], so the whole machine runs in host tests against a fake PL.

use argus_replay::State as FetchState;

use crate::acq::{ReplayStatus, ctrl, replay_ack};
use crate::lines::{StreamFetch, StreamHalves, StreamRx};
use crate::replay::SAMPLES_PER_HALF;
use crate::timing::Timing;

/// One half of the ping-pong BRAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// Bytes 0 to 28223 of the aperture; played first.
    Zero,
    /// Bytes 28224 to 56447.
    One,
}

impl Half {
    /// 0 or 1, the C's `half` and the index into the aperture.
    pub const fn index(self) -> usize {
        match self {
            Self::Zero => 0,
            Self::One => 1,
        }
    }

    /// The REPLAY_ACK bit that hands this half back to the PL.
    const fn ack(self) -> u32 {
        match self {
            Self::Zero => replay_ack::CONSUMED0,
            Self::One => replay_ack::CONSUMED1,
        }
    }
}

/// What the stream needs of the PL.
pub trait Pl {
    /// Reads REPLAY_STATUS.
    fn replay_status(&mut self) -> ReplayStatus;

    /// Writes REPLAY_ACK: each set bit clears its flag.
    fn ack_replay(&mut self, bits: u32);

    /// Writes CTRL whole.
    fn write_ctrl(&mut self, value: u32);

    /// Makes `half` as the PS wrote it visible to the PL, which reads the
    /// BRAM directly and never sees the A9's caches. Must be complete when
    /// it returns: the half is acked right after.
    fn clean_half(&mut self, half: Half);
}

/// What the stream needs of the replay client.
pub trait Fetch {
    /// Begins fetching one half's worth of samples ([`SAMPLES_PER_HALF`])
    /// from dataset sample `offset`. The firmware lands the samples in the
    /// half [`Stream::in_flight`] names. Returns whether the request left.
    fn start(&mut self, offset: u32) -> bool;

    /// Where the fetch stands. The stream waits for `Complete` or `Failed`.
    fn state(&self) -> FetchState;
}

/// A microsecond clock that wraps at 2^32, the C's `argus_now_us()`.
pub trait Clock {
    /// Microseconds since some fixed moment.
    fn now_us(&self) -> u32;
}

/// `stream_state_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Not begun.
    Off,
    /// Fetching half 0.
    Prime0,
    /// Fetching half 1.
    Prime1,
    /// Refilling whichever half the PL has finished with.
    Run,
}

/// The stream: where it is, what it has counted since it began, and the
/// timing figures since the last report.
#[derive(Clone, Debug)]
pub struct Stream {
    phase: Phase,
    /// Dataset offset of the next fetch.
    next_sample: u32,
    /// The half being fetched, if any.
    in_flight: Option<Half>,

    halves_filled: u32,
    underruns: u32,
    failures: u32,

    /// When the fetch in flight was requested.
    fetch_started_us: u32,
    /// Request to last chunk, per half.
    fetch: Timing,
    /// The cache clean, per half.
    flush: Timing,
    /// When the last fetch while running ended, until the next request
    /// leaves; `None` while priming, so priming is not counted.
    done_us: Option<u32>,
    /// Fetch done to next request sent, while running. Includes waiting for
    /// the PL to consume a half and whatever else the firmware does
    /// meanwhile.
    gap: Timing,
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream {
    /// A stream that has not begun.
    pub const fn new() -> Self {
        Self {
            phase: Phase::Off,
            next_sample: 0,
            in_flight: None,
            halves_filled: 0,
            underruns: 0,
            failures: 0,
            fetch_started_us: 0,
            fetch: Timing::new(),
            flush: Timing::new(),
            done_us: None,
            gap: Timing::new(),
        }
    }

    /// `argus_replay_stream_begin()`: resets the counters and requests half
    /// 0 from `first_sample`. If the request does not leave, the next
    /// [`service`](Self::service) tries again.
    pub fn begin<F: Fetch, C: Clock>(&mut self, first_sample: u32, fetch: &mut F, clock: &C) {
        *self = Self::new();
        self.next_sample = first_sample;
        self.phase = Phase::Prime0;
        self.fetch_into(Half::Zero, fetch, clock);
    }

    /// The half being fetched, which is where the firmware must land the
    /// chunks that arrive; `None` between fetches.
    pub const fn in_flight(&self) -> Option<Half> {
        self.in_flight
    }

    /// Whether priming is over and the PL is playing the halves.
    pub fn is_running(&self) -> bool {
        self.phase == Phase::Run
    }

    /// `argus_replay_stream_service()`, after the client has been polled:
    /// moves the stream on by at most one step. Call it often; the PL
    /// consumes a half every 4.9 ms.
    pub fn service<P: Pl, F: Fetch, C: Clock>(&mut self, pl: &mut P, fetch: &mut F, clock: &C) {
        match self.phase {
            Phase::Off => {}
            Phase::Prime0 => self.prime(Half::Zero, pl, fetch, clock),
            Phase::Prime1 => self.prime(Half::One, pl, fetch, clock),
            Phase::Run => self.run(pl, fetch, clock),
        }
    }

    /// The two priming phases: fetch `half`, then go on to the next phase.
    ///
    /// The C, when the request for half 1 does not leave, stays in PRIME0
    /// and fetches half 0 again at the already advanced offset, so the PL
    /// would start at sample 147. Here the stream moves to priming half 1
    /// either way, and a request that did not leave is retried for half 1
    /// at the same offset, as every other failed fetch is.
    fn prime<P: Pl, F: Fetch, C: Clock>(
        &mut self,
        half: Half,
        pl: &mut P,
        fetch: &mut F,
        clock: &C,
    ) {
        let Some(in_flight) = self.in_flight else {
            self.fetch_into(half, fetch, clock);
            return;
        };
        match fetch.state() {
            FetchState::Complete => {
                self.half_ready(in_flight, pl, clock);
                self.in_flight = None;
                match half {
                    Half::Zero => {
                        self.phase = Phase::Prime1;
                        self.fetch_into(Half::One, fetch, clock);
                    }
                    Half::One => {
                        enter_ext_mode(pl);
                        self.phase = Phase::Run;
                    }
                }
            }
            FetchState::Failed => {
                // Same half, same offset, next call.
                self.failures = self.failures.wrapping_add(1);
                self.in_flight = None;
            }
            FetchState::Idle | FetchState::Waiting => {}
        }
    }

    /// Running: count and clear an underrun, then either start refilling a
    /// consumed half or see the fetch in flight through.
    fn run<P: Pl, F: Fetch, C: Clock>(&mut self, pl: &mut P, fetch: &mut F, clock: &C) {
        let status = pl.replay_status();

        if status.underrun() {
            self.underruns = self.underruns.wrapping_add(1);
            pl.ack_replay(replay_ack::UNDERRUN);
        }

        let Some(in_flight) = self.in_flight else {
            // Half 0 first if both are pending, which only happens after an
            // underrun.
            if status.consumed0() {
                self.fetch_into(Half::Zero, fetch, clock);
            } else if status.consumed1() {
                self.fetch_into(Half::One, fetch, clock);
            }
            return;
        };

        match fetch.state() {
            FetchState::Complete => {
                self.half_ready(in_flight, pl, clock);
                pl.ack_replay(in_flight.ack());
            }
            // Not acked, so the PL still reports the half consumed and the
            // next call fetches it again at the same offset.
            FetchState::Failed => self.failures = self.failures.wrapping_add(1),
            FetchState::Idle | FetchState::Waiting => return,
        }
        self.in_flight = None;
        self.done_us = Some(clock.now_us());
    }

    /// `stream_fetch_into()`: requests the next block for `half`. A request
    /// that does not leave counts as a failure and leaves nothing in
    /// flight, so the caller's phase tries again.
    fn fetch_into<F: Fetch, C: Clock>(&mut self, half: Half, fetch: &mut F, clock: &C) {
        if !fetch.start(self.next_sample) {
            self.failures = self.failures.wrapping_add(1);
            return;
        }
        self.in_flight = Some(half);
        self.fetch_started_us = clock.now_us();
        if self.phase == Phase::Run
            && let Some(done_us) = self.done_us.take()
        {
            self.gap.record(self.fetch_started_us.wrapping_sub(done_us));
        }
    }

    /// `stream_half_ready()`: every sample of `half` is in. Records how long
    /// the fetch took, cleans the half to the BRAM and records how long that
    /// took, and moves the dataset offset on. The caller acks.
    fn half_ready<P: Pl, C: Clock>(&mut self, half: Half, pl: &mut P, clock: &C) {
        let now = clock.now_us();
        self.fetch.record(now.wrapping_sub(self.fetch_started_us));

        pl.clean_half(half);
        self.flush.record(clock.now_us().wrapping_sub(now));

        self.halves_filled = self.halves_filled.wrapping_add(1);
        self.next_sample = self.next_sample.wrapping_add(u32::from(SAMPLES_PER_HALF));
    }

    /// The `stream: halves=` line: the counts since the stream began, and
    /// `status`, REPLAY_STATUS read for the report.
    pub const fn halves_line(&self, status: ReplayStatus) -> StreamHalves {
        StreamHalves {
            halves: self.halves_filled,
            underruns: self.underruns,
            failures: self.failures,
            next: self.next_sample,
            pl: status,
        }
    }

    /// The fetch, flush and gap figures since the last report, and starts
    /// them again from none.
    pub fn take_timings(&mut self) -> StreamTimings {
        StreamTimings {
            fetch: self.fetch.take(),
            flush: self.flush.take(),
            gap: self.gap.take(),
        }
    }
}

/// `stream_enter_ext_mode()`: soft reset with ext_mode set, clear every
/// replay flag, then release with ext_mode kept. Playback starts at row 0
/// of half 0 with no partial first frame.
///
/// CTRL is written whole, as in the C, so this clears any hold bit; the
/// firmware never calls it while a hold is in progress.
fn enter_ext_mode<P: Pl>(pl: &mut P) {
    pl.write_ctrl(ctrl::SOFT_RESET | ctrl::EXT_MODE);
    pl.ack_replay(replay_ack::CONSUMED0 | replay_ack::CONSUMED1 | replay_ack::UNDERRUN);
    pl.write_ctrl(ctrl::ENABLE | ctrl::EXT_MODE);
}

/// The stream's timing figures over one report interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamTimings {
    /// Request to last chunk, per half.
    pub fetch: Timing,
    /// The cache clean, per half.
    pub flush: Timing,
    /// Fetch done to next request sent, while running.
    pub gap: Timing,
}

/// The receive path's figures over one report interval: the C's
/// `g_copy_us_*`, `g_parse_us_*`, `g_rx_n` and `g_loop_*`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxStats {
    /// Moving a datagram out of the socket.
    copy: Timing,
    /// The client's checks, CRC and copy into the half.
    parse: Timing,
    /// Wakeups of the replay task.
    loop_iters: u32,
    /// Datagrams those wakeups delivered.
    loop_pkts: u32,
}

impl RxStats {
    /// No datagrams yet.
    pub const fn new() -> Self {
        Self {
            copy: Timing::new(),
            parse: Timing::new(),
            loop_iters: 0,
            loop_pkts: 0,
        }
    }

    /// One datagram: `copy_us` to take it out of the socket, `parse_us` for
    /// the client to check and place it.
    pub fn datagram(&mut self, copy_us: u32, parse_us: u32) {
        self.copy.record(copy_us);
        self.parse.record(parse_us);
    }

    /// One wakeup of the replay task, which delivered `packets` datagrams:
    /// the C's `argus_replay_note_loop()`.
    pub fn wakeup(&mut self, packets: u32) {
        self.loop_iters = self.loop_iters.wrapping_add(1);
        self.loop_pkts = self.loop_pkts.wrapping_add(packets);
    }

    /// The figures so far, starting again from none.
    pub fn take(&mut self) -> Self {
        core::mem::take(self)
    }
}

/// The three `stream:` lines of `argus_replay_stream_report()`, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamReport {
    /// `stream: halves=...`
    pub halves: StreamHalves,
    /// `stream: fetch avg=...`
    pub fetch: StreamFetch,
    /// `stream: rx n=...`
    pub rx: StreamRx,
}

/// What the report needs from outside the stream: REPLAY_STATUS, the
/// client's counters and the console's figures, all read at report time.
#[derive(Clone, Copy, Debug)]
pub struct ReportInputs {
    /// REPLAY_STATUS.
    pub status: ReplayStatus,
    /// The replay client's counters since boot.
    pub client: argus_replay::Counters,
    /// Console messages dropped since boot.
    pub con_drops: u32,
    /// The console's longest message since the last report.
    pub con_max: u32,
}

impl StreamReport {
    /// `argus_replay_stream_report()`: the lines for now, after which every
    /// timing figure starts again from none.
    pub fn take(stream: &mut Stream, rx: &mut RxStats, inputs: &ReportInputs) -> Self {
        let timings = stream.take_timings();
        let rx = rx.take();
        Self {
            halves: stream.halves_line(inputs.status),
            fetch: StreamFetch {
                fetch: timings.fetch,
                flush: timings.flush,
                retransmits: inputs.client.retransmits_sent,
                timeouts: inputs.client.timeouts,
                rejected: inputs.client.chunks_rejected,
            },
            rx: StreamRx {
                copy: rx.copy,
                parse: rx.parse,
                loop_iters: rx.loop_iters,
                loop_pkts: rx.loop_pkts,
                gap: timings.gap,
                con_drops: inputs.con_drops,
                con_max: inputs.con_max,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::Cell;
    use std::format;
    use std::vec::Vec;

    use super::*;

    const CONSUMED0: u32 = 1 << 1;
    const CONSUMED1: u32 = 1 << 2;
    const UNDERRUN: u32 = 1 << 3;

    /// What the fake PL saw, in order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Op {
        Ctrl(u32),
        Ack(u32),
        Clean(Half),
    }

    /// A PL whose REPLAY_STATUS the test sets, and which records every
    /// write. Acks clear the matching status bits, as the fabric's do.
    #[derive(Default)]
    struct FakePl {
        status: u32,
        ops: Vec<Op>,
    }

    impl Pl for FakePl {
        fn replay_status(&mut self) -> ReplayStatus {
            ReplayStatus(self.status)
        }
        fn ack_replay(&mut self, bits: u32) {
            self.ops.push(Op::Ack(bits));
            // REPLAY_ACK bit n clears REPLAY_STATUS bit n + 1.
            self.status &= !(bits << 1);
        }
        fn write_ctrl(&mut self, value: u32) {
            self.ops.push(Op::Ctrl(value));
        }
        fn clean_half(&mut self, half: Half) {
            self.ops.push(Op::Clean(half));
        }
    }

    /// A fetch the test completes or fails by hand; records each offset
    /// requested, and refuses to send while `refuse` is set.
    struct FakeFetch {
        state: FetchState,
        offsets: Vec<u32>,
        refuse: bool,
    }

    impl FakeFetch {
        fn new() -> Self {
            Self {
                state: FetchState::Idle,
                offsets: Vec::new(),
                refuse: false,
            }
        }
    }

    impl Fetch for FakeFetch {
        fn start(&mut self, offset: u32) -> bool {
            if self.refuse {
                return false;
            }
            self.offsets.push(offset);
            self.state = FetchState::Waiting;
            true
        }
        fn state(&self) -> FetchState {
            self.state
        }
    }

    /// A clock the test moves.
    struct FakeClock(Cell<u32>);

    impl Clock for FakeClock {
        fn now_us(&self) -> u32 {
            self.0.get()
        }
    }

    impl FakeClock {
        fn advance(&self, us: u32) {
            self.0.set(self.0.get().wrapping_add(us));
        }
    }

    struct Rig {
        stream: Stream,
        pl: FakePl,
        fetch: FakeFetch,
        clock: FakeClock,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                stream: Stream::new(),
                pl: FakePl::default(),
                fetch: FakeFetch::new(),
                clock: FakeClock(Cell::new(1000)),
            }
        }
        fn begin(&mut self) {
            self.stream.begin(0, &mut self.fetch, &self.clock);
        }
        fn service(&mut self) {
            self.stream
                .service(&mut self.pl, &mut self.fetch, &self.clock);
        }
        fn complete(&mut self) {
            self.fetch.state = FetchState::Complete;
            self.service();
        }
        /// Begins and primes both halves; the stream is then running.
        fn primed() -> Self {
            let mut rig = Self::new();
            rig.begin();
            rig.complete();
            rig.complete();
            rig.pl.ops.clear();
            rig
        }
        fn halves(&self) -> StreamHalves {
            self.stream.halves_line(ReplayStatus(0))
        }
    }

    #[test]
    fn a_new_stream_does_nothing() {
        let mut rig = Rig::new();
        rig.service();
        assert!(rig.fetch.offsets.is_empty());
        assert!(rig.pl.ops.is_empty());
        assert_eq!(rig.stream.in_flight(), None);
    }

    #[test]
    fn priming_fills_half_0_then_half_1_then_enters_ext_mode() {
        let mut rig = Rig::new();
        rig.begin();
        assert_eq!(rig.fetch.offsets, [0]);
        assert_eq!(rig.stream.in_flight(), Some(Half::Zero));

        // Nothing changes while the fetch is in progress.
        rig.service();
        assert_eq!(rig.fetch.offsets, [0]);

        rig.complete();
        assert_eq!(rig.fetch.offsets, [0, 147]);
        assert_eq!(rig.stream.in_flight(), Some(Half::One));
        assert_eq!(rig.pl.ops, [Op::Clean(Half::Zero)]);
        assert!(!rig.stream.is_running());

        rig.complete();
        assert_eq!(rig.stream.in_flight(), None);
        assert!(rig.stream.is_running());
        // stream_half_ready, then stream_enter_ext_mode, register for
        // register. The primed halves are not acked one by one.
        assert_eq!(
            rig.pl.ops,
            [
                Op::Clean(Half::Zero),
                Op::Clean(Half::One),
                Op::Ctrl(ctrl::SOFT_RESET | ctrl::EXT_MODE),
                Op::Ack(0b111),
                Op::Ctrl(ctrl::ENABLE | ctrl::EXT_MODE),
            ]
        );
        assert_eq!(rig.halves().halves, 2);
        assert_eq!(rig.halves().next, 294);
    }

    #[test]
    fn running_refills_the_consumed_half_cleans_it_and_acks_it() {
        let mut rig = Rig::primed();

        // Nothing consumed: nothing fetched.
        rig.service();
        assert_eq!(rig.fetch.offsets, [0, 147]);

        rig.pl.status = CONSUMED0;
        rig.service();
        assert_eq!(rig.fetch.offsets, [0, 147, 294]);
        assert_eq!(rig.stream.in_flight(), Some(Half::Zero));

        // At most one fetch in flight, whatever the PL says meanwhile.
        rig.pl.status |= CONSUMED1;
        rig.service();
        assert_eq!(rig.fetch.offsets, [0, 147, 294]);

        rig.complete();
        // Cleaned before it is acked.
        assert_eq!(
            rig.pl.ops,
            [Op::Clean(Half::Zero), Op::Ack(replay_ack::CONSUMED0)]
        );
        assert_eq!(rig.stream.in_flight(), None);
        assert_eq!(rig.pl.status, CONSUMED1);

        rig.service();
        assert_eq!(rig.fetch.offsets, [0, 147, 294, 441]);
        assert_eq!(rig.stream.in_flight(), Some(Half::One));
        rig.complete();
        assert_eq!(rig.pl.ops.last(), Some(&Op::Ack(replay_ack::CONSUMED1)));
        assert_eq!(rig.halves().halves, 4);
        assert_eq!(rig.halves().next, 588);
    }

    #[test]
    fn half_0_is_refilled_first_when_both_are_consumed() {
        let mut rig = Rig::primed();
        rig.pl.status = CONSUMED0 | CONSUMED1;
        rig.service();
        assert_eq!(rig.stream.in_flight(), Some(Half::Zero));
    }

    #[test]
    fn a_failed_fetch_retries_the_same_half_at_the_same_offset() {
        let mut rig = Rig::primed();
        rig.pl.status = CONSUMED1;
        rig.service();
        assert_eq!(rig.fetch.offsets.last(), Some(&294));

        rig.fetch.state = FetchState::Failed;
        rig.service();
        assert_eq!(rig.stream.in_flight(), None);
        // Neither cleaned nor acked.
        assert!(rig.pl.ops.is_empty());
        assert_eq!(rig.halves().failures, 1);
        assert_eq!(rig.halves().next, 294);

        rig.service();
        assert_eq!(rig.fetch.offsets.last(), Some(&294));
        assert_eq!(rig.stream.in_flight(), Some(Half::One));
    }

    #[test]
    fn a_failed_prime_retries_the_same_half() {
        let mut rig = Rig::new();
        rig.begin();
        rig.fetch.state = FetchState::Failed;
        rig.service();
        assert_eq!(rig.stream.in_flight(), None);
        rig.service();
        assert_eq!(rig.fetch.offsets, [0, 0]);
        assert_eq!(rig.stream.in_flight(), Some(Half::Zero));
        assert_eq!(rig.halves().failures, 1);
    }

    #[test]
    fn a_request_that_does_not_leave_is_a_failure_retried_next_time() {
        let mut rig = Rig::new();
        rig.fetch.refuse = true;
        rig.begin();
        assert_eq!(rig.stream.in_flight(), None);
        assert_eq!(rig.halves().failures, 1);

        rig.fetch.refuse = false;
        rig.service();
        assert_eq!(rig.fetch.offsets, [0]);
        assert_eq!(rig.stream.in_flight(), Some(Half::Zero));
    }

    #[test]
    fn a_refused_request_for_half_1_retries_half_1_at_its_offset() {
        let mut rig = Rig::new();
        rig.begin();
        rig.fetch.refuse = true;
        rig.complete();
        assert_eq!(rig.stream.in_flight(), None);

        rig.fetch.refuse = false;
        rig.service();
        assert_eq!(rig.fetch.offsets, [0, 147]);
        assert_eq!(rig.stream.in_flight(), Some(Half::One));
    }

    #[test]
    fn an_underrun_is_counted_and_acked_every_time_it_shows() {
        let mut rig = Rig::primed();
        rig.pl.status = UNDERRUN;
        rig.service();
        assert_eq!(rig.pl.ops, [Op::Ack(replay_ack::UNDERRUN)]);
        assert_eq!(rig.pl.status, 0);
        rig.service();
        rig.pl.status = UNDERRUN | CONSUMED0;
        rig.service();
        assert_eq!(rig.halves().underruns, 2);
        // The refill still starts in the same call.
        assert_eq!(rig.stream.in_flight(), Some(Half::Zero));
    }

    #[test]
    fn begin_resets_the_counts() {
        let mut rig = Rig::primed();
        rig.pl.status = UNDERRUN;
        rig.service();
        rig.begin();
        let line = rig.halves();
        assert_eq!((line.halves, line.underruns, line.failures), (0, 0, 0));
    }

    #[test]
    fn fetch_flush_and_gap_are_timed_and_reset_by_the_report() {
        let mut rig = Rig::new();
        rig.begin();
        rig.clock.advance(800);
        rig.complete();
        rig.clock.advance(600);
        rig.complete();

        // Running: consumed 3000 us after half 1 was ready.
        rig.clock.advance(3000);
        rig.pl.status = CONSUMED0;
        rig.service();
        rig.clock.advance(500);
        rig.complete();

        let t = rig.stream.take_timings();
        assert_eq!((t.fetch.n(), t.fetch.max(), t.fetch.avg()), (3, 800, 633));
        // The fake clean takes no time.
        assert_eq!((t.flush.n(), t.flush.max()), (3, 0));
        // Priming is not a gap.
        assert_eq!(t.gap.n(), 0);

        rig.clock.advance(4400);
        rig.pl.status = CONSUMED1;
        rig.service();
        let t = rig.stream.take_timings();
        assert_eq!(t.fetch.n(), 0);
        assert_eq!((t.gap.n(), t.gap.max()), (1, 4400));
        assert_eq!(rig.stream.take_timings(), StreamTimings::default());
    }

    #[test]
    fn the_report_carries_the_c_fields() {
        let mut rig = Rig::primed();
        let mut rx = RxStats::new();
        rx.wakeup(3);
        rx.wakeup(0);
        rx.datagram(4, 20);
        rx.datagram(6, 30);
        rx.datagram(5, 25);
        let inputs = ReportInputs {
            status: ReplayStatus(0x0012_3405),
            client: argus_replay::Counters {
                requests_sent: 3,
                retransmits_sent: 1,
                chunks_accepted: 63,
                chunks_rejected: 2,
                timeouts: 1,
            },
            con_drops: 7,
            con_max: 180,
        };
        let report = StreamReport::take(&mut rig.stream, &mut rx, &inputs);
        assert_eq!(
            format!("{}", report.halves),
            "stream: halves=2 underruns=0 failures=0 next=294 pl: half=1 row=4660 c0=0 c1=1\r\n"
        );
        assert_eq!(
            format!("{}", report.fetch),
            "stream: fetch avg=0 max=0 us (n=2)  flush avg=0 max=0 us  rtx=1 to=1 rej=2\r\n"
        );
        assert_eq!(
            format!("{}", report.rx),
            "stream: rx n=3 copy avg=5 max=6 us  parse avg=25 max=30 us  \
             loop iters=2 pkts=3  gap avg=0 max=0 us (n=0)  con drop=7 max=180\r\n"
        );
        assert_eq!(rx, RxStats::new());
    }
}
