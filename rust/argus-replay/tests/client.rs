//! The replay client against a simulated relay.
//!
//! The relay here answers a request the way argus_sim's relay does: chunks
//! of `samples_per_chunk` samples, indexed from zero, the last one short,
//! each with its CRC. The tests then lose, reorder, duplicate and corrupt
//! those chunks and check what the client does with each.
//!
//! The workspace lints forbid unwrap and panic here too, so every test
//! returns a `Result` and fails through `?`.

use std::fmt::Debug;

use argus_replay::{Client, ConfigError, FetchError, Packet, Reject, State, Transport};
use argus_wire::crc::{Crc16, checksum};
use argus_wire::generated::{
    ARGUS_FRAME_MAGIC, ARGUS_MSG_REPLAY_CHUNK, ARGUS_MSG_REPLAY_REQUEST, ARGUS_REPLAY_VERSION,
    argus_replay_chunk_hdr_t, argus_replay_request_t,
};
use zerocopy::{FromBytes, IntoBytes};

type TestResult = Result<(), String>;

const CHANNELS: u16 = 96;
const ROW_BYTES: usize = CHANNELS as usize * 2;
const HALF: u16 = 147;
const TIMEOUT_MS: u32 = 200;
const RETRIES: u8 = 5;

/// Turns any error into the test's failure message.
fn ok<T, E: Debug>(result: Result<T, E>) -> Result<T, String> {
    result.map_err(|e| format!("{e:?}"))
}

/// Fails the test with `msg` unless `cond` holds.
fn ensure(cond: bool, msg: &str) -> TestResult {
    if cond { Ok(()) } else { Err(msg.to_string()) }
}

/// Records every datagram the client sends, or refuses them all.
#[derive(Default)]
struct Net {
    sent: Vec<Vec<u8>>,
    refuse: bool,
}

impl Transport for Net {
    type Error = &'static str;

    fn send(&mut self, datagram: &[u8]) -> Result<(), Self::Error> {
        if self.refuse {
            return Err("refused");
        }
        self.sent.push(datagram.to_vec());
        Ok(())
    }
}

impl Net {
    /// The last request sent, decoded, after checking its CRC.
    fn last_request(&self) -> Result<argus_replay_request_t, String> {
        let bytes = self.sent.last().ok_or("nothing sent")?;
        let req = ok(argus_replay_request_t::read_from_bytes(bytes))?;
        let (covered, _) = bytes.split_at(20);
        ensure(checksum(covered) == req.crc, "request CRC does not match")?;
        Ok(req)
    }
}

/// The dataset: a value for every (absolute sample, channel), distinct
/// enough that a misplaced chunk shows.
fn value(sample: u32, channel: u16) -> u16 {
    let v = sample.wrapping_mul(131) ^ (u32::from(channel) << 7);
    u16::try_from(v & 0xFFFF).unwrap_or(0)
}

/// `count` rows of the dataset from `sample`, as little-endian bytes.
fn rows(sample: u32, count: u16) -> Vec<u8> {
    (sample..sample + u32::from(count))
        .flat_map(|s| (0..CHANNELS).map(move |c| value(s, c)))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// One chunk datagram with a correct CRC, built from its header fields.
fn chunk(mut hdr: argus_replay_chunk_hdr_t, payload: &[u8]) -> Vec<u8> {
    let (covered, _) = hdr.as_bytes().split_at(26);
    hdr.crc = Crc16::new().update(covered).update(payload).finish();
    let mut bytes = hdr.as_bytes().to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

/// A chunk header for `seq`, with the fields a relay would fill in.
fn header(seq: u32, offset: u32, samples: u16, index: u16, total: u16) -> argus_replay_chunk_hdr_t {
    argus_replay_chunk_hdr_t {
        magic: ARGUS_FRAME_MAGIC,
        version: u16::try_from(ARGUS_REPLAY_VERSION).unwrap_or(0),
        type_: u16::try_from(ARGUS_MSG_REPLAY_CHUNK).unwrap_or(0),
        seq,
        sample_offset: offset,
        sample_count: samples,
        chunk_index: index,
        chunk_total: total,
        channel_count: CHANNELS,
        payload_len: samples * CHANNELS * 2,
        crc: 0,
    }
}

/// The relay's whole answer to the last request: 7-sample chunks from
/// index 0, the last one carrying what is left.
fn answer(net: &Net) -> Result<Vec<Vec<u8>>, String> {
    let req = net.last_request()?;
    let (seq, offset, count) = (req.seq, req.sample_offset, req.sample_count);
    let total = count.div_ceil(7);
    Ok((0..total)
        .map(|i| {
            let first = u32::from(i) * 7;
            let samples = (count - i * 7).min(7);
            chunk(
                header(seq, offset + first, samples, i, total),
                &rows(offset + first, samples),
            )
        })
        .collect())
}

fn client() -> Result<Client, String> {
    ok(Client::new(CHANNELS, TIMEOUT_MS, RETRIES))
}

/// Feeds `datagram` and checks the outcome.
fn feed(cl: &mut Client, dst: &mut [u8], datagram: &[u8], expect: Packet) -> TestResult {
    let got = cl.on_packet(datagram, dst);
    ensure(got == expect, &format!("expected {expect:?}, got {got:?}"))
}

fn get(burst: &[Vec<u8>], index: usize) -> Result<&[u8], String> {
    burst
        .get(index)
        .map(Vec::as_slice)
        .ok_or(format!("no chunk {index}"))
}

#[test]
fn a_whole_burst_fills_the_destination() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];

    ok(cl.fetch(&mut net, 1000, HALF, 0))?;
    let req = net.last_request()?;
    ensure(req.magic == ARGUS_FRAME_MAGIC, "magic")?;
    ensure(u32::from(req.version) == ARGUS_REPLAY_VERSION, "version")?;
    ensure(u32::from(req.type_) == ARGUS_MSG_REPLAY_REQUEST, "type")?;
    ensure({ req.seq } == 1, "first seq is 1")?;
    ensure(
        { req.sample_offset } == 1000 && { req.sample_count } == HALF,
        "span",
    )?;
    ensure({ req.flags } == 0, "not a retransmit")?;
    ensure(
        cl.total_chunks() == 21 && cl.samples_per_chunk() == 7,
        "21 chunks of 7",
    )?;

    let burst = answer(&net)?;
    ensure(burst.len() == 21, "relay sends 21 chunks")?;
    for datagram in &burst {
        ensure(cl.state() == State::Waiting, "waiting until the last chunk")?;
        feed(&mut cl, &mut dst, datagram, Packet::Accepted)?;
    }
    ensure(cl.state() == State::Complete && cl.is_done(), "complete")?;
    ensure(dst == rows(1000, HALF), "every sample in place")?;

    let c = cl.counters();
    ensure(
        c.requests_sent == 1 && c.retransmits_sent == 0,
        "one request",
    )?;
    ensure(
        c.chunks_accepted == 21 && c.chunks_rejected == 0 && c.timeouts == 0,
        "counters",
    )?;
    ensure(cl.arrived_count() == 21, "arrived")
}

#[test]
fn chunks_are_placed_by_index_not_by_sample_offset() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;

    // Every chunk claims sample_offset 0, as if the dataset wrapped, and
    // they arrive in reverse. Placement by index still fills it in order.
    for i in (0..21u16).rev() {
        let first = u32::from(i) * 7;
        let datagram = chunk(header(1, 0, 7, i, 21), &rows(first, 7));
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(cl.state() == State::Complete, "complete")?;
    ensure(dst == rows(0, HALF), "placed by index")
}

#[test]
fn a_duplicate_is_rejected_and_not_counted_twice() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    let burst = answer(&net)?;

    feed(&mut cl, &mut dst, get(&burst, 4)?, Packet::Accepted)?;
    feed(
        &mut cl,
        &mut dst,
        get(&burst, 4)?,
        Packet::Rejected(Reject::Duplicate),
    )?;
    let c = cl.counters();
    ensure(c.chunks_accepted == 1 && c.chunks_rejected == 1, "one each")?;
    ensure(cl.arrived_count() == 1, "arrived once")
}

#[test]
fn a_retransmit_asks_for_the_first_gap_under_a_new_seq() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 500, HALF, 10))?;
    let first_burst = answer(&net)?;

    // Chunks 5, 6 and 7 are lost; so is 12, in a second gap.
    for (i, datagram) in first_burst.iter().enumerate() {
        if !matches!(i, 5..=7 | 12) {
            feed(&mut cl, &mut dst, datagram, Packet::Accepted)?;
        }
    }
    ok(cl.poll(&mut net, 209))?;
    ensure(net.sent.len() == 1, "no retransmit before the deadline")?;
    ok(cl.poll(&mut net, 210))?;
    ensure(net.sent.len() == 2, "retransmit at the deadline")?;

    // Only the first contiguous run, under seq 2.
    let req = net.last_request()?;
    ensure({ req.seq } == 2 && cl.seq() == 2, "new seq")?;
    ensure({ req.flags } == 1, "retransmit flag")?;
    ensure(
        { req.sample_offset } == 500 + 35 && { req.sample_count } == 21,
        "chunks 5 to 7",
    )?;

    // A late chunk of the first attempt is now stale, even one still missing.
    feed(
        &mut cl,
        &mut dst,
        get(&first_burst, 6)?,
        Packet::Rejected(Reject::StaleSeq),
    )?;

    // The answer is indexed from 0 within the retransmit, total 3.
    for datagram in answer(&net)? {
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(cl.state() == State::Waiting, "chunk 12 still missing")?;

    ok(cl.poll(&mut net, 410))?;
    let req = net.last_request()?;
    ensure({ req.seq } == 3, "third seq")?;
    ensure(
        { req.sample_offset } == 500 + 84 && { req.sample_count } == 7,
        "chunk 12",
    )?;
    for datagram in answer(&net)? {
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(cl.state() == State::Complete, "complete")?;
    ensure(dst == rows(500, HALF), "every sample in place")?;

    let c = cl.counters();
    ensure(
        c.requests_sent == 1 && c.retransmits_sent == 2 && c.timeouts == 2,
        "counters",
    )?;
    ensure(
        c.chunks_accepted == 21 && c.chunks_rejected == 1,
        "accepted and rejected",
    )
}

#[test]
fn a_retransmit_of_the_tail_asks_for_exactly_what_is_left() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    for datagram in answer(&net)?.iter().take(19) {
        feed(&mut cl, &mut dst, datagram, Packet::Accepted)?;
    }
    ok(cl.poll(&mut net, TIMEOUT_MS))?;
    let req = net.last_request()?;
    ensure(
        { req.sample_offset } == 133 && { req.sample_count } == 14,
        "chunks 19 and 20",
    )?;
    for datagram in answer(&net)? {
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(
        cl.state() == State::Complete && dst == rows(0, HALF),
        "complete",
    )
}

#[test]
fn the_short_final_chunk_is_accepted_and_only_there() -> TestResult {
    // 10 samples: one chunk of 7, then a final chunk of 3.
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; 10 * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, 10, 0))?;
    ensure(cl.total_chunks() == 2, "two chunks")?;

    // A short chunk where a full one belongs.
    let short_first = chunk(header(1, 0, 3, 0, 2), &rows(0, 3));
    feed(
        &mut cl,
        &mut dst,
        &short_first,
        Packet::Rejected(Reject::SampleCount),
    )?;
    // A full chunk where the short one belongs.
    let long_last = chunk(header(1, 7, 7, 1, 2), &rows(7, 7));
    feed(
        &mut cl,
        &mut dst,
        &long_last,
        Packet::Rejected(Reject::SampleCount),
    )?;

    for datagram in answer(&net)? {
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(
        cl.state() == State::Complete && dst == rows(0, 10),
        "complete",
    )
}

#[test]
fn a_wrong_sample_count_is_rejected() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    // A relay that sizes chunks at 6 samples: self-consistent, CRC good,
    // and wrong for us.
    let six = chunk(header(1, 0, 6, 0, 21), &rows(0, 6));
    feed(
        &mut cl,
        &mut dst,
        &six,
        Packet::Rejected(Reject::SampleCount),
    )?;
    ensure(dst.iter().all(|&b| b == 0), "nothing written")
}

#[test]
fn a_wrong_channel_count_is_rejected() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    let mut hdr = header(1, 0, 7, 0, 21);
    hdr.channel_count = 95;
    hdr.payload_len = 7 * 95 * 2;
    let payload = vec![0u8; 7 * 95 * 2];
    feed(
        &mut cl,
        &mut dst,
        &chunk(hdr, &payload),
        Packet::Rejected(Reject::ChannelCount),
    )
}

#[test]
fn a_wrong_chunk_total_is_rejected() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    let wrong = chunk(header(1, 0, 7, 0, 20), &rows(0, 7));
    feed(
        &mut cl,
        &mut dst,
        &wrong,
        Packet::Rejected(Reject::ChunkTotal),
    )?;

    // After a retransmit of the 16 chunks still missing, the original
    // burst's total of 21 is wrong.
    for datagram in answer(&net)?.iter().take(5) {
        feed(&mut cl, &mut dst, datagram, Packet::Accepted)?;
    }
    ok(cl.poll(&mut net, TIMEOUT_MS))?;
    let full = chunk(header(2, 35, 7, 0, 21), &rows(35, 7));
    feed(
        &mut cl,
        &mut dst,
        &full,
        Packet::Rejected(Reject::ChunkTotal),
    )
}

#[test]
fn a_crc_failure_is_rejected() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    let good = answer(&net)?;
    let good = get(&good, 0)?;

    // One payload bit flipped.
    let mut bad_payload = good.to_vec();
    if let Some(b) = bad_payload.get_mut(100) {
        *b ^= 0x01;
    }
    feed(
        &mut cl,
        &mut dst,
        &bad_payload,
        Packet::Rejected(Reject::Crc),
    )?;

    // One header byte changed (sample_offset), CRC left alone.
    let mut bad_header = good.to_vec();
    if let Some(b) = bad_header.get_mut(12) {
        *b ^= 0x80;
    }
    feed(
        &mut cl,
        &mut dst,
        &bad_header,
        Packet::Rejected(Reject::Crc),
    )?;
    ensure(dst.iter().all(|&b| b == 0), "nothing written")?;

    feed(&mut cl, &mut dst, good, Packet::Accepted)
}

#[test]
fn a_chunk_that_would_overflow_the_destination_is_rejected() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    // One byte short of the fetch.
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES - 1];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    let burst = answer(&net)?;
    for datagram in burst.iter().take(20) {
        feed(&mut cl, &mut dst, datagram, Packet::Accepted)?;
    }
    feed(
        &mut cl,
        &mut dst,
        get(&burst, 20)?,
        Packet::Rejected(Reject::Overflow),
    )?;
    ensure(cl.state() == State::Waiting, "still waiting")?;
    let tail = dst.get(140 * ROW_BYTES..).ok_or("tail")?;
    ensure(
        tail.iter().all(|&b| b == 0),
        "nothing written past chunk 19",
    )
}

#[test]
fn malformed_datagrams_are_rejected() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;
    let good = answer(&net)?;
    let good = get(&good, 0)?;

    feed(
        &mut cl,
        &mut dst,
        &[0u8; 27],
        Packet::Rejected(Reject::Short),
    )?;

    let mut hdr = header(1, 0, 7, 0, 21);
    hdr.magic ^= 1;
    feed(
        &mut cl,
        &mut dst,
        &chunk(hdr, &rows(0, 7)),
        Packet::Rejected(Reject::NotAChunk),
    )?;
    let mut hdr = header(1, 0, 7, 0, 21);
    hdr.version = 2;
    feed(
        &mut cl,
        &mut dst,
        &chunk(hdr, &rows(0, 7)),
        Packet::Rejected(Reject::NotAChunk),
    )?;
    let mut hdr = header(1, 0, 7, 0, 21);
    hdr.type_ = u16::try_from(ARGUS_MSG_REPLAY_REQUEST).unwrap_or(0);
    feed(
        &mut cl,
        &mut dst,
        &chunk(hdr, &rows(0, 7)),
        Packet::Rejected(Reject::NotAChunk),
    )?;

    let index_past_total = chunk(header(1, 0, 7, 21, 21), &rows(0, 7));
    feed(
        &mut cl,
        &mut dst,
        &index_past_total,
        Packet::Rejected(Reject::ChunkIndex),
    )?;

    let (truncated, _) = good.split_at(good.len() - 1);
    feed(
        &mut cl,
        &mut dst,
        truncated,
        Packet::Rejected(Reject::Length),
    )?;

    let mut hdr = header(1, 0, 7, 0, 21);
    hdr.payload_len -= 2;
    let payload = rows(0, 7);
    let (short_payload, _) = payload.split_at(payload.len() - 2);
    feed(
        &mut cl,
        &mut dst,
        &chunk(hdr, short_payload),
        Packet::Rejected(Reject::PayloadLen),
    )?;

    ensure(cl.counters().chunks_rejected == 7, "each one counted")?;
    feed(&mut cl, &mut dst, good, Packet::Accepted)
}

#[test]
fn the_deadline_holds_across_the_millisecond_wrap() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let start = u32::MAX - 50;
    ok(cl.fetch(&mut net, 0, HALF, start))?;
    // The deadline is start + 200, which wraps to 149.
    for now in [start, u32::MAX, 0, 148] {
        ok(cl.poll(&mut net, now))?;
        ensure(cl.counters().timeouts == 0, &format!("no timeout at {now}"))?;
    }
    ok(cl.poll(&mut net, 149))?;
    ensure(cl.counters().timeouts == 1, "timeout at 149")?;
    ensure(net.sent.len() == 2, "retransmitted")?;

    // The next deadline is 349; a poll long after still counts as passed,
    // up to half the counter's range.
    ok(cl.poll(&mut net, 349 + 0x7FFF_FFFF))?;
    ensure(cl.counters().timeouts == 2, "long past is past")
}

#[test]
fn retries_exhausted_ends_in_failed() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, HALF, 0))?;

    let mut now = 0;
    for attempt in 1..=RETRIES {
        now += TIMEOUT_MS;
        ok(cl.poll(&mut net, now))?;
        ensure(
            cl.state() == State::Waiting,
            &format!("still trying after {attempt}"),
        )?;
    }
    now += TIMEOUT_MS;
    ok(cl.poll(&mut net, now))?;
    ensure(cl.state() == State::Failed && cl.is_done(), "failed")?;

    let c = cl.counters();
    ensure(
        c.requests_sent == 1 && c.retransmits_sent == 5,
        "five retransmits",
    )?;
    ensure(c.timeouts == 6, "six timeouts, the last one fatal")?;
    ensure(net.sent.len() == 6, "nothing sent on failing")?;

    // A failed fetch takes no more packets and no more polls.
    let late = answer(&net)?;
    feed(&mut cl, &mut dst, get(&late, 0)?, Packet::Ignored)?;
    ok(cl.poll(&mut net, now + 10 * TIMEOUT_MS))?;
    ensure(cl.counters() == c, "counters unchanged")?;

    // And a new fetch starts over.
    ok(cl.fetch(&mut net, 147, HALF, now))?;
    ensure(cl.state() == State::Waiting, "waiting again")?;
    ensure({ net.last_request()?.seq } == 7, "seq keeps counting")
}

#[test]
fn packets_are_ignored_when_no_fetch_is_outstanding() -> TestResult {
    let mut cl = client()?;
    let mut dst = vec![0u8; usize::from(HALF) * ROW_BYTES];
    let stray = chunk(header(0, 0, 7, 0, 21), &rows(0, 7));
    feed(&mut cl, &mut dst, &stray, Packet::Ignored)?;
    ensure(cl.counters().chunks_rejected == 0, "not counted")?;

    let mut net = Net::default();
    ok(cl.fetch(&mut net, 0, 7, 0))?;
    for datagram in answer(&net)? {
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(cl.state() == State::Complete, "complete")?;
    let again = answer(&net)?;
    feed(&mut cl, &mut dst, get(&again, 0)?, Packet::Ignored)
}

#[test]
fn a_send_failure_still_leaves_the_fetch_waiting() -> TestResult {
    let mut cl = client()?;
    let mut net = Net {
        refuse: true,
        ..Net::default()
    };
    let got = cl.fetch(&mut net, 0, HALF, 0);
    ensure(
        got == Err(FetchError::Send("refused")),
        "send error reported",
    )?;
    ensure(cl.state() == State::Waiting, "waiting regardless")?;
    ensure(cl.counters().requests_sent == 1, "counted as sent")?;

    let got = cl.poll(&mut net, TIMEOUT_MS);
    ensure(got == Err("refused"), "poll reports it too")?;
    ensure(cl.counters().retransmits_sent == 1, "counted")?;

    net.refuse = false;
    ok(cl.poll(&mut net, 2 * TIMEOUT_MS))?;
    ensure(
        { net.last_request()?.sample_count } == HALF,
        "the whole burst again",
    )
}

#[test]
fn configuration_and_fetch_limits() -> TestResult {
    ensure(
        Client::new(0, 200, 5).err() == Some(ConfigError::ChannelCount),
        "zero channels",
    )?;
    ensure(
        Client::new(723, 200, 5).err() == Some(ConfigError::ChannelCount),
        "723 channels",
    )?;

    let mut cl = client()?;
    let mut net = Net::default();
    ensure(
        cl.fetch(&mut net, 0, 0, 0) == Err(FetchError::NoSamples),
        "no samples",
    )?;
    // 32 chunks of 7 is the most one burst may be.
    ensure(
        cl.fetch(&mut net, 0, 32 * 7 + 1, 0) == Err(FetchError::TooManyChunks),
        "33 chunks",
    )?;
    ensure(
        net.sent.is_empty() && cl.state() == State::Idle,
        "nothing started",
    )?;
    ok(cl.fetch(&mut net, 0, 32 * 7, 0))?;
    ensure(cl.total_chunks() == 32, "32 chunks")
}

#[test]
fn a_32_chunk_burst_uses_every_bit() -> TestResult {
    let mut cl = client()?;
    let mut net = Net::default();
    let mut dst = vec![0u8; 32 * 7 * ROW_BYTES];
    ok(cl.fetch(&mut net, 0, 32 * 7, 0))?;
    let burst = answer(&net)?;
    for datagram in burst.iter().take(31) {
        feed(&mut cl, &mut dst, datagram, Packet::Accepted)?;
    }
    ok(cl.poll(&mut net, TIMEOUT_MS))?;
    let req = net.last_request()?;
    ensure(
        { req.sample_offset } == 31 * 7 && { req.sample_count } == 7,
        "chunk 31",
    )?;
    for datagram in answer(&net)? {
        feed(&mut cl, &mut dst, &datagram, Packet::Accepted)?;
    }
    ensure(
        cl.state() == State::Complete && dst == rows(0, 32 * 7),
        "complete",
    )
}
