//! The C replay client and the Rust one, side by side.
//!
//! Each scenario sets up both clients the same way (a channel count, a
//! timeout, a retry limit, a starting clock) and a relay, then takes a few
//! hundred random steps: start a fetch, deliver a datagram, step the clock,
//! poll, make sends fail. Datagrams are lost, duplicated, reordered,
//! corrupted, cut short, answered by a relay set up wrong, or replayed from
//! earlier requests. After every step both clients must agree on the state,
//! every counter, the last request sent byte for byte, and all of the
//! destination.
//!
//! The workspace lints forbid unwrap and panic here too, so the test
//! returns a `Result` and fails with the message of the first difference.

use std::fmt;

use argus_difftest::Failure;
use argus_difftest::c::{CClient, DST_BYTES, SENT_BYTES, Snapshot};
use argus_difftest::relay::Relay;
use argus_difftest::rng::{Rng, SEED_VAR, run_seed};
use argus_replay::{Client, FetchError, MAX_CHUNKS, State, Transport};

/// Scenarios per run, unless `ARGUS_DIFFTEST_SCENARIOS` says otherwise.
const SCENARIOS: u64 = 10_000;

/// Steps a scenario may take before it stops, done or not.
const MAX_STEPS: u32 = 600;

/// Records each request the Rust client sends, as the shim's send callback
/// does for the C one, and fails on demand.
struct Recorder {
    fails: bool,
    sends: u32,
    sent_len: u32,
    sent: [u8; SENT_BYTES],
}

impl Recorder {
    fn new() -> Self {
        Self {
            fails: false,
            sends: 0,
            sent_len: 0,
            sent: [0; SENT_BYTES],
        }
    }
}

impl Transport for Recorder {
    type Error = ();

    fn send(&mut self, datagram: &[u8]) -> Result<(), ()> {
        self.sends = self.sends.wrapping_add(1);
        self.sent_len = u32::try_from(datagram.len()).unwrap_or(u32::MAX);
        self.sent = [0; SENT_BYTES];
        let n = datagram.len().min(SENT_BYTES);
        if let (Some(dst), Some(src)) = (self.sent.get_mut(..n), datagram.get(..n)) {
            dst.copy_from_slice(src);
        }
        if self.fails { Err(()) } else { Ok(()) }
    }
}

/// The same snapshot the shim takes of the C client, from the Rust one.
fn rust_snapshot(client: &Client, net: &Recorder) -> Snapshot {
    let counters = client.counters();
    Snapshot {
        state: match client.state() {
            State::Idle => 0,
            State::Waiting => 1,
            State::Complete => 2,
            State::Failed => 3,
        },
        seq: client.seq(),
        arrived_count: client.arrived_count().into(),
        total_chunks: client.total_chunks().into(),
        samples_per_chunk: client.samples_per_chunk().into(),
        requests_sent: counters.requests_sent,
        retransmits_sent: counters.retransmits_sent,
        chunks_accepted: counters.chunks_accepted,
        chunks_rejected: counters.chunks_rejected,
        timeouts: counters.timeouts,
        sends: net.sends,
        sent_len: net.sent_len,
        sent: net.sent,
    }
}

/// What a step did, for the failure message.
enum Step {
    Fetch { offset: u32, count: u16 },
    Deliver { len: usize, mutation: Mutation },
    Advance { ms: u32 },
    Poll,
    SendFails(bool),
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch { offset, count } => write!(f, "fetch {count} samples from {offset}"),
            Self::Deliver { len, mutation } => write!(f, "deliver {len} bytes ({mutation:?})"),
            Self::Advance { ms } => write!(f, "clock +{ms} ms"),
            Self::Poll => write!(f, "poll"),
            Self::SendFails(fails) => write!(f, "sends fail: {fails}"),
        }
    }
}

/// What was done to a datagram on its way.
#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    /// One byte XORed with a nonzero value.
    Corrupt,
    Truncate,
    Extend,
    /// Random bytes, not a chunk at all.
    Garbage,
    /// A datagram delivered earlier, delivered again.
    Replay,
}

/// The two clients, their transports and destinations, and the network
/// between them and the relay.
struct Pair {
    c: CClient,
    rust: Client,
    net: Recorder,
    dst: Box<[u8; DST_BYTES]>,
    now: u32,
    relay: Relay,
    /// Datagrams on their way, in arrival order unless reordered.
    in_flight: Vec<Vec<u8>>,
    /// Datagrams already delivered, for replaying.
    history: Vec<Vec<u8>>,
    /// Requests seen so far, so each one is answered once.
    sends_seen: u32,
    /// The clients' timeout, which sizes the clock steps.
    timeout: u32,
    /// Percent of chunks the network loses, and whether it reorders.
    loss_pct: u64,
    reorder: bool,
}

/// What a run saw, summed over its scenarios, to check that the scenarios
/// reach every corner they are meant to.
#[derive(Debug, Default)]
struct Coverage {
    scenarios: u64,
    steps: u64,
    init_refused: u64,
    fetch_refused: u64,
    complete: u64,
    failed: u64,
    retransmits: u64,
    accepted: u64,
    rejected: u64,
}

impl Pair {
    /// Both clients, configured alike, or `None` if both refuse the
    /// configuration (which they must agree on).
    fn new(
        rng: &mut Rng,
        channel_count: u16,
        timeout: u32,
        retries: u8,
    ) -> Result<Option<Self>, String> {
        let c = CClient::new(channel_count, timeout, retries);
        let rust = Client::new(channel_count, timeout, retries);
        let (c, rust) = match (c, rust) {
            (Ok(c), Ok(rust)) => (c, rust),
            (Err(-1), Err(_)) => return Ok(None),
            (c, rust) => {
                return Err(format!(
                    "init({channel_count}, {timeout}, {retries}): C {:?}, Rust {:?}",
                    c.err(),
                    rust.err()
                ));
            }
        };
        let dst = vec![0u8; DST_BYTES]
            .into_boxed_slice()
            .try_into()
            .map_err(|_| "destination size".to_string())?;

        // Most relays agree with the board; some stamp a different channel
        // count, and some cut chunks a sample short.
        let honest = Relay::for_channels(channel_count);
        let relay = match rng.below(20) {
            0 => Relay::for_channels(if channel_count > 1 {
                channel_count - 1
            } else {
                2
            }),
            1 if honest.samples_per_chunk > 1 => Relay {
                samples_per_chunk: honest.samples_per_chunk - 1,
                ..honest
            },
            _ => honest,
        };

        // Some clocks start just short of the 32-bit wrap.
        let now = if rng.chance(3, 10) {
            u32::MAX - rng.range_u32(0, 2 * timeout)
        } else {
            rng.next_u32()
        };

        Ok(Some(Self {
            c,
            rust,
            net: Recorder::new(),
            dst,
            now,
            relay,
            in_flight: Vec::new(),
            history: Vec::new(),
            sends_seen: 0,
            timeout,
            loss_pct: match rng.below(6) {
                0 | 1 => 0,
                2 => 1,
                3 => 5,
                4 => 20,
                _ => 50,
            },
            reorder: rng.chance(1, 3),
        }))
    }

    fn done(&self) -> bool {
        self.rust.is_done()
    }

    /// A fetch on both; they must agree on whether it started.
    fn fetch(&mut self, offset: u32, count: u16) -> Result<(), String> {
        let c = self.c.fetch(offset, count, self.now);
        let rust = self.rust.fetch(&mut self.net, offset, count, self.now);
        let agree = matches!(
            (c, &rust),
            (0, Ok(()))
                | (
                    -1,
                    Err(FetchError::NoSamples | FetchError::TooManyChunks | FetchError::Send(()))
                )
        );
        if agree {
            Ok(())
        } else {
            Err(format!("fetch returned C {c}, Rust {rust:?}"))
        }
    }

    fn deliver(&mut self, datagram: &[u8]) {
        self.c.on_packet(datagram);
        self.rust.on_packet(datagram, self.dst.as_mut_slice());
    }

    fn poll(&mut self) {
        self.c.poll(self.now);
        // The C poll returns nothing; a send failure inside it is visible
        // through the recorded request and the counters, which are compared.
        let _ = self.rust.poll(&mut self.net, self.now);
    }

    fn set_send_fails(&mut self, fails: bool) {
        self.c.set_send_fails(fails);
        self.net.fails = fails;
    }

    /// Checks that the two clients agree on their state, their counters,
    /// the last request and every byte of the destination.
    fn compare(&mut self) -> Result<(), String> {
        let c = self.c.snapshot();
        let rust = rust_snapshot(&self.rust, &self.net);
        if c != rust {
            return Err(format!(
                "the clients differ\n  C:    {c:?}\n  Rust: {rust:?}"
            ));
        }
        if let Some(at) = self.c.dst_diff(&self.dst) {
            return Err(format!(
                "the destinations differ from byte {at}\n  state: {c:?}"
            ));
        }
        Ok(())
    }

    /// The relay answers the latest request if a new one went out and was
    /// not refused by the transport, and the network loses, duplicates and
    /// reorders the answer.
    fn answer(&mut self, rng: &mut Rng) {
        if self.net.sends == self.sends_seen {
            return;
        }
        self.sends_seen = self.net.sends;
        // A request lost on the way out gets no answer at all.
        if self.net.fails || rng.chance(1, 20) {
            return;
        }
        let len = usize::try_from(self.net.sent_len).unwrap_or(0);
        let request = self.net.sent.get(..len).unwrap_or(&[]);
        for chunk in self.relay.answer(request) {
            if rng.below(100) < self.loss_pct {
                continue;
            }
            if rng.chance(1, 30) {
                self.in_flight.push(chunk.clone());
            }
            self.in_flight.push(chunk);
        }
        if self.reorder {
            rng.shuffle(&mut self.in_flight);
        }
    }

    /// Takes the next datagram off the network and does something to it.
    fn next_datagram(&mut self, rng: &mut Rng) -> (Vec<u8>, Mutation) {
        if !self.history.is_empty() && (self.in_flight.is_empty() || rng.chance(1, 25)) {
            let i = usize::try_from(rng.below(self.history.len() as u64)).unwrap_or(0);
            if let Some(old) = self.history.get(i) {
                return (old.clone(), Mutation::Replay);
            }
        }
        if self.in_flight.is_empty() || rng.chance(1, 100) {
            let len = usize::try_from(rng.below(64)).unwrap_or(0);
            let garbage = (0..len).map(|_| rng.next_u32().to_le_bytes()[0]).collect();
            return (garbage, Mutation::Garbage);
        }
        let mut datagram = self.in_flight.remove(0);
        self.history.push(datagram.clone());
        if self.history.len() > 256 {
            self.history.remove(0);
        }
        let mutation = match rng.below(100) {
            0..=5 => {
                let i = usize::try_from(rng.below(datagram.len() as u64)).unwrap_or(0);
                let flip = rng.range_u32(1, 255).to_le_bytes()[0];
                if let Some(byte) = datagram.get_mut(i) {
                    *byte ^= flip;
                }
                Mutation::Corrupt
            }
            6..=7 => {
                let keep = usize::try_from(rng.below(datagram.len() as u64)).unwrap_or(0);
                datagram.truncate(keep);
                Mutation::Truncate
            }
            8 => {
                datagram.push(rng.next_u32().to_le_bytes()[0]);
                Mutation::Extend
            }
            _ => Mutation::None,
        };
        (datagram, mutation)
    }

    /// A fetch size: mostly one that fits a burst, sometimes none or too
    /// many.
    fn fetch_count(&self, rng: &mut Rng) -> u16 {
        let per_chunk = self.rust.samples_per_chunk();
        let max = per_chunk.saturating_mul(MAX_CHUNKS);
        match rng.below(20) {
            0 => 0,
            1 => rng.range_u16(max.saturating_add(1), u16::MAX),
            2 => rng.range_u16(0, u16::MAX),
            3..=8 if max >= 147 => 147,
            _ => rng.range_u16(1, max),
        }
    }

    /// One random step, applied to both clients.
    ///
    /// While datagrams are on their way, delivering them is the likeliest
    /// step, as on the wire where a burst arrives back to back; with none
    /// left, the clock and polls take over, which is how a gap becomes a
    /// retransmit.
    fn step(&mut self, rng: &mut Rng, fetches_left: &mut u32) -> Result<Step, String> {
        // Percent weights of fetch, deliver, clock, poll and send-fail.
        let weights: [u64; 5] = if self.in_flight.is_empty() {
            [10, 5, 40, 44, 1]
        } else {
            [10, 70, 4, 15, 1]
        };
        let mut roll = rng.below(100);
        let mut kind = 0;
        for (i, &w) in weights.iter().enumerate() {
            if roll < w {
                kind = i;
                break;
            }
            roll -= w;
        }

        // Start the next fetch once the last is over, or now and then
        // abandon one in the middle. Otherwise the fetch slot polls.
        let can_fetch = *fetches_left > 0
            && (self.done() || self.rust.state() == State::Idle || rng.chance(1, 8));
        let step = match kind {
            0 if can_fetch => {
                *fetches_left -= 1;
                let offset = if rng.chance(1, 5) {
                    u32::MAX - rng.range_u32(0, 300)
                } else {
                    rng.next_u32()
                };
                let count = self.fetch_count(rng);
                self.fetch(offset, count)?;
                Step::Fetch { offset, count }
            }
            1 => {
                let (datagram, mutation) = self.next_datagram(rng);
                self.deliver(&datagram);
                Step::Deliver {
                    len: datagram.len(),
                    mutation,
                }
            }
            2 => {
                let ms = self.clock_step(rng);
                self.now = self.now.wrapping_add(ms);
                Step::Advance { ms }
            }
            4 => {
                let fails = rng.chance(1, 4);
                self.set_send_fails(fails);
                Step::SendFails(fails)
            }
            _ => {
                self.poll();
                Step::Poll
            }
        };
        Ok(step)
    }

    /// A clock step: short, close either side of the timeout, long, or
    /// anywhere in the 32-bit range (which can put the deadline back
    /// ahead).
    fn clock_step(&self, rng: &mut Rng) -> u32 {
        let timeout = self.timeout;
        match rng.below(40) {
            0..=29 => rng.range_u32(0, timeout / 8),
            30..=36 => timeout.saturating_sub(2).wrapping_add(rng.range_u32(0, 4)),
            37..=38 => rng.range_u32(0, timeout.saturating_mul(10)),
            _ => rng.next_u32(),
        }
    }
}

/// One scenario from its seed, adding what it reached to `coverage`.
/// Returns the first difference between the clients.
fn scenario(seed: u64, coverage: &mut Coverage) -> Result<(), String> {
    let mut rng = Rng::new(seed);

    // 96 is the board; 124 the most a 30-chunk half allows; 1 and 722 the
    // ends of the valid range; 0 and past 722 refused; the rest anything.
    let channel_count = match rng.below(20) {
        0..=9 => 96,
        10 => 124,
        11 => 1,
        12 => 722,
        13 => 0,
        14 => rng.range_u16(723, u16::MAX),
        _ => rng.range_u16(1, 722),
    };
    let timeout = if rng.chance(1, 10) {
        rng.range_u32(0, 3)
    } else {
        rng.range_u32(5, 500)
    };
    let retries = u8::try_from(rng.below(7)).unwrap_or(0);

    let Some(mut pair) = Pair::new(&mut rng, channel_count, timeout, retries)? else {
        coverage.init_refused += 1;
        return Ok(());
    };
    pair.compare()?;

    let mut fetches_left = u32::try_from(rng.range(1, 4)).unwrap_or(1);
    for n in 0..MAX_STEPS {
        let step = pair.step(&mut rng, &mut fetches_left)?;
        coverage.steps += 1;
        if matches!(step, Step::Fetch { .. }) && pair.rust.state() != State::Waiting {
            coverage.fetch_refused += 1;
        }
        pair.compare().map_err(|e| {
            format!(
                "step {n}: {step} (channels {channel_count}, timeout {timeout}, \
                 retries {retries}, relay {:?}, now {})\n{e}",
                pair.relay, pair.now
            )
        })?;
        pair.answer(&mut rng);
        if fetches_left == 0 && pair.done() {
            break;
        }
    }

    let counters = pair.rust.counters();
    coverage.retransmits += u64::from(counters.retransmits_sent);
    coverage.accepted += u64::from(counters.chunks_accepted);
    coverage.rejected += u64::from(counters.chunks_rejected);
    match pair.rust.state() {
        State::Complete => coverage.complete += 1,
        State::Failed => coverage.failed += 1,
        State::Idle | State::Waiting => {}
    }
    Ok(())
}

/// The number of scenarios to run: `ARGUS_DIFFTEST_SCENARIOS`, or
/// [`SCENARIOS`].
fn scenario_count() -> Result<u64, String> {
    match std::env::var("ARGUS_DIFFTEST_SCENARIOS") {
        Ok(text) => text
            .trim()
            .parse()
            .map_err(|e| format!("ARGUS_DIFFTEST_SCENARIOS={text}: {e}")),
        Err(_) => Ok(SCENARIOS),
    }
}

#[test]
fn c_and_rust_clients_agree() -> Result<(), Failure> {
    let seed = run_seed()?;
    let scenarios = scenario_count()?;
    println!("replay client difftest: {scenarios} scenarios, {SEED_VAR}={seed:#x}");

    // Every scenario gets its own seed from the run's, so the run's seed
    // alone repeats the whole run.
    let mut seeds = Rng::new(seed);
    let mut coverage = Coverage::default();
    for i in 0..scenarios {
        let scenario_seed = seeds.next_u64();
        scenario(scenario_seed, &mut coverage).map_err(|e| {
            format!("{SEED_VAR}={seed:#x}, scenario {i} (seed {scenario_seed:#x}):\n{e}")
        })?;
        coverage.scenarios += 1;
    }
    println!("{coverage:#?}");

    // The scenarios are only worth something if they reach the paths that
    // matter. A full run that never retransmits or never fails a fetch has
    // stopped testing what it is for.
    if scenarios >= SCENARIOS {
        let c = &coverage;
        let reached = [
            ("a refused configuration", c.init_refused),
            ("a refused fetch", c.fetch_refused),
            ("a completed fetch", c.complete),
            ("a failed fetch", c.failed),
            ("a retransmit", c.retransmits),
            ("an accepted chunk", c.accepted),
            ("a rejected chunk", c.rejected),
        ];
        for (what, count) in reached {
            if count == 0 {
                return Err(Failure(format!(
                    "{SEED_VAR}={seed:#x}: no scenario reached {what}\n{c:#?}"
                )));
            }
        }
    }
    Ok(())
}
