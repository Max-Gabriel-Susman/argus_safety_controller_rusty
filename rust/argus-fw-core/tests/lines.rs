//! Every console line against the C's format string, rendered by hand with
//! the sample values shown beside it.
//!
//! Each test quotes the C's printf call so the two can be read side by
//! side. Where a board log of the C firmware exists, the sample values are
//! taken from it, so the expected text is a line the C actually printed.

use argus_fw_core::acq::{self, FrameCheck, ReplayStatus};
use argus_fw_core::lines::*;
use argus_fw_core::phy::{Duplex, Link, PhyId, Speed};
use argus_fw_core::timing::Timing;

fn timing(samples: &[u32]) -> Timing {
    let mut t = Timing::new();
    for &us in samples {
        t.record(us);
    }
    t
}

/// A Timing with `n` samples summing to `sum` and the longest `max`.
fn timing_of(n: u32, sum: u32, max: u32) -> Timing {
    let mut t = Timing::new();
    t.record(max);
    let rest = n - 1;
    let each = (sum - max) / rest;
    let extra = (sum - max) % rest;
    for i in 0..rest {
        t.record(each + u32::from(i < extra));
    }
    assert_eq!((t.n(), t.max()), (n, max));
    assert_eq!(t.avg(), sum / n);
    t
}

#[test]
fn constant_lines() {
    // xil_printf("Initializing Argus Safety Controller...\r\n");
    assert_eq!(INITIALIZING, "Initializing Argus Safety Controller...\r\n");
    // argus_console_printf("acq frame: hold never took effect\r\n");
    assert_eq!(
        ACQ_FRAME_HOLD_TIMEOUT,
        "acq frame: hold never took effect\r\n"
    );
    // xil_printf("acq features: hold never took effect\r\n");
    assert_eq!(
        ACQ_FEATURES_HOLD_TIMEOUT,
        "acq features: hold never took effect\r\n"
    );
    // xil_printf("replay: BRAM aperture cacheable; halves flushed before ack\r\n");
    assert_eq!(
        REPLAY_BRAM_CACHEABLE,
        "replay: BRAM aperture cacheable; halves flushed before ack\r\n"
    );
    // xil_printf("replay FAILED\r\n");
    assert_eq!(REPLAY_FAILED, "replay FAILED\r\n");
    // xil_printf("stream: priming\r\n");
    assert_eq!(STREAM_PRIMING, "stream: priming\r\n");
    // xil_printf("Argus Safety Controller initialized. IP 192.168.1.10\r\n");
    assert_eq!(
        INITIALIZED,
        "Argus Safety Controller initialized. IP 192.168.1.10\r\n"
    );
}

#[test]
fn replay_clock_and_its_warnings() {
    // xil_printf("replay clock: %u ms measured over 10 ms sleep\r\n", dt);
    let line = ReplayClock { ms: 10 };
    assert_eq!(
        line.to_string(),
        "replay clock: 10 ms measured over 10 ms sleep\r\n"
    );
    assert_eq!(line.warning(), None);

    // if (dt < 5u) xil_printf("WARNING: replay clock too slow -- ...");
    assert_eq!(
        ReplayClock { ms: 4 }.warning(),
        Some("WARNING: replay clock too slow -- retransmit timeouts will not fire\r\n")
    );
    // else if (dt > 20u) xil_printf("WARNING: replay clock too fast -- ...");
    assert_eq!(
        ReplayClock { ms: 21 }.warning(),
        Some("WARNING: replay clock too fast -- timeouts fire before replies arrive\r\n")
    );
    // The bounds themselves are fine.
    assert_eq!(ReplayClock { ms: 5 }.warning(), None);
    assert_eq!(ReplayClock { ms: 20 }.warning(), None);
    assert_eq!(
        ReplayClock { ms: 0 }.to_string(),
        "replay clock: 0 ms measured over 10 ms sleep\r\n"
    );
}

#[test]
fn replay_clock_from_global_timer_ticks() {
    // The Arty's global timer runs at 325 MHz: 325000 ticks per ms.
    const HZ: u32 = 325_000_000;
    // A 10 ms sleep that overran by a few microseconds still reads 10.
    assert_eq!(ReplayClock::from_ticks(3_250_000, HZ).ms, 10);
    assert_eq!(ReplayClock::from_ticks(3_254_000, HZ).ms, 10);
    // One tick short of 10 ms reads 9: whole milliseconds, rounded down.
    assert_eq!(ReplayClock::from_ticks(3_249_999, HZ).ms, 9);
    // A timer believed to run at the C's 333 MHz but really at 325 MHz
    // makes 10 ms read 9: the 2.5% error the C carries.
    assert_eq!(ReplayClock::from_ticks(3_250_000, 333_333_333).ms, 9);
    // No millisecond divisor, or a figure past u32: shown as the maximum,
    // which the too-fast warning follows.
    assert_eq!(ReplayClock::from_ticks(3_250_000, 0).ms, u32::MAX);
    assert_eq!(ReplayClock::from_ticks(3_250_000, 999).ms, u32::MAX);
    assert_eq!(ReplayClock::from_ticks(u64::MAX, HZ).ms, u32::MAX);
    assert!(ReplayClock::from_ticks(1, 0).warning().is_some());
}

#[test]
fn acq_id_accepted() {
    // xil_printf("acq id=%08x\r\n", id);
    assert_eq!(AcqId { id: 0x4143_5133 }.to_string(), "acq id=41435133\r\n");
    assert_eq!(AcqId { id: 0x4143_5134 }.to_string(), "acq id=41435134\r\n");
}

#[test]
fn acq_id_unexpected() {
    // The contract's text: both accepted IDs, %08x.
    assert_eq!(
        AcqId { id: 0x4143_5132 }.to_string(),
        "acq id=41435132  EXPECTED 41435133 or 41435134 -- stale bitstream? \
         Rebuild in Vivado and re-export the XSA.\r\n"
    );
    // Zero padding, and the value unmapped reads return.
    assert_eq!(
        AcqId { id: 0x0000_00ab }.to_string(),
        "acq id=000000ab  EXPECTED 41435133 or 41435134 -- stale bitstream? \
         Rebuild in Vivado and re-export the XSA.\r\n"
    );
    assert!(
        AcqId { id: 0xDEAD_BEEF }
            .to_string()
            .starts_with("acq id=deadbeef  EXPECTED ")
    );
}

#[test]
fn acq_status() {
    // xil_printf("acq status=%08x frames/s=%u\r\n", status, f1 - f0);
    let line = AcqStatus {
        status: 0x0000_0001,
        frames_per_s: 30012,
    };
    assert_eq!(line.to_string(), "acq status=00000001 frames/s=30012\r\n");
}

#[test]
fn acq_timing() {
    // xil_printf("acq timing: reg %u ns/read, 96-word frame read %u us,"
    //            " sweep 33 us (read is held)\r\n", reg_ns, frame_us);
    let line = AcqTiming {
        reg_ns: 1083,
        frame_us: 114,
    };
    assert_eq!(
        line.to_string(),
        "acq timing: reg 1083 ns/read, 96-word frame read 114 us, sweep 33 us (read is held)\r\n"
    );
}

/// A frame as the built-in source sends it at sample index `idx`.
fn ident_frame(idx: u8) -> [u16; 96] {
    core::array::from_fn(|n| acq::frame_word(u16::try_from(n).unwrap_or(0), idx))
}

#[test]
fn acq_frame_internal() {
    // argus_console_printf("acq frame %u: [0]=%04x [%d]=%04x idx=%02x expect=%02x bad=%d\r\n",
    //     fi, frame[0], 95, frame[95], idx, (fi - 1u) & 0xFFu, bad);
    // Frame 4660 holds sample index 0x33, one behind 4660 = 0x1234.
    let line = AcqFrame::new(4660, &ident_frame(0x33), false);
    assert_eq!(
        line.to_string(),
        "acq frame 4660: [0]=0033 [95]=9f33 idx=33 expect=33 bad=0\r\n"
    );

    // Frame 0: (0 - 1) & 0xFF is ff. Three words wrong.
    let mut frame = ident_frame(0x07);
    frame[1] = 0;
    frame[40] = 0xFFFF;
    frame[95] = 0x0007;
    let line = AcqFrame::new(0, &frame, false);
    assert_eq!(
        line.to_string(),
        "acq frame 0: [0]=0007 [95]=0007 idx=07 expect=ff bad=3\r\n"
    );
}

#[test]
fn acq_frame_external() {
    // argus_console_printf("acq frame %u: [0]=%04x [%d]=%04x idx=%02x (ext) bad=%d\r\n",
    //     fi, frame[0], 95, frame[95], idx, bad);
    let line = AcqFrame::new(2_697_156, &ident_frame(0xA5), true);
    assert_eq!(
        line.to_string(),
        "acq frame 2697156: [0]=00a5 [95]=9fa5 idx=a5 (ext) bad=0\r\n"
    );

    // The fields can be filled directly too; the line shows them as given.
    let line = AcqFrame {
        frame_index: 12,
        first: 0x0102,
        last: 0xBEEF,
        check: FrameCheck { idx: 0x02, bad: 95 },
        ext: true,
    };
    assert_eq!(
        line.to_string(),
        "acq frame 12: [0]=0102 [95]=beef idx=02 (ext) bad=95\r\n"
    );
}

#[test]
fn acq_features() {
    // xil_printf("acq features: bin %u  ch0 count %u power %u  ch14 count %u power %u"
    //            "  dropped %u\r\n", bin, counts[0], power[0], counts[14], power[14], dropped);
    let line = AcqFeatures {
        bin: 1,
        count0: 0,
        power0: 7225,
        count14: 3,
        power14: 81234,
        dropped: 0,
    };
    assert_eq!(
        line.to_string(),
        "acq features: bin 1  ch0 count 0 power 7225  ch14 count 3 power 81234  dropped 0\r\n"
    );
}

#[test]
fn replay_ok() {
    // xil_printf("replay ok: [0][0]=%04x [0][5]=%04x [1][0]=%04x [146][95]=%04x\r\n",
    //     buf[0], buf[5], buf[96], buf[146 * 96 + 95]);
    let line = ReplayOk {
        s0_c0: 0x0001,
        s0_c5: 0x00a5,
        s1_c0: 0x1000,
        s146_c95: 0xfffe,
    };
    assert_eq!(
        line.to_string(),
        "replay ok: [0][0]=0001 [0][5]=00a5 [1][0]=1000 [146][95]=fffe\r\n"
    );
}

#[test]
fn replay_counters() {
    // xil_printf("replay: req=%d rtx=%d ok=%d rej=%d to=%d chunks=%d/%d\r\n",
    //     (int)requests_sent, (int)retransmits_sent, (int)chunks_accepted,
    //     (int)chunks_rejected, (int)timeouts, (int)arrived_count, (int)total_chunks);
    let line = ReplayCounters {
        requests: 1,
        retransmits: 0,
        accepted: 21,
        rejected: 0,
        timeouts: 0,
        arrived: 21,
        total: 21,
    };
    assert_eq!(
        line.to_string(),
        "replay: req=1 rtx=0 ok=21 rej=0 to=0 chunks=21/21\r\n"
    );

    // %d of (int) of a uint32_t past 2^31 is negative.
    let line = ReplayCounters {
        requests: 0x8000_0000,
        retransmits: u32::MAX,
        ..line
    };
    assert_eq!(
        line.to_string(),
        "replay: req=-2147483648 rtx=-1 ok=21 rej=0 to=0 chunks=21/21\r\n"
    );
}

#[test]
fn tx() {
    // argus_console_printf("tx %u bin %u skipped %u\r\n", tx_count, bin, tx_skipped);
    // From the C firmware's ACQ4 baseline, 2026-10-31.
    let line = Tx {
        count: 1860,
        bin: 1858,
        skipped: 0,
    };
    assert_eq!(line.to_string(), "tx 1860 bin 1858 skipped 0\r\n");
}

#[test]
fn stream_halves() {
    // argus_console_printf("stream: halves=%u underruns=%u failures=%u next=%u"
    //     " pl: half=%u row=%u c0=%u c1=%u\r\n", halves, underruns, failures, next,
    //     rs & 1, (rs >> 8) & 0xFFFF, (rs >> 1) & 1, (rs >> 2) & 1);
    // From the C firmware's ACQ4 baseline, 2026-10-31.
    let line = StreamHalves {
        halves: 18348,
        underruns: 0,
        failures: 0,
        next: 2_697_156,
        pl: ReplayStatus(142 << 8),
    };
    assert_eq!(
        line.to_string(),
        "stream: halves=18348 underruns=0 failures=0 next=2697156 pl: half=0 row=142 c0=0 c1=0\r\n"
    );

    // Every PL field set, and the bits above the row ignored.
    let line = StreamHalves {
        halves: 1,
        underruns: 2,
        failures: 3,
        next: 294,
        pl: ReplayStatus(0xFF12_340F),
    };
    assert_eq!(
        line.to_string(),
        "stream: halves=1 underruns=2 failures=3 next=294 pl: half=1 row=4660 c0=1 c1=1\r\n"
    );
}

#[test]
fn stream_fetch() {
    // argus_console_printf("stream: fetch avg=%u max=%u us (n=%u)  flush avg=%u max=%u us"
    //     "  rtx=%u to=%u rej=%u\r\n", fetch_n ? fetch_sum / fetch_n : 0, fetch_max,
    //     fetch_n, flush_n ? flush_sum / flush_n : 0, flush_max, rtx, to, rej);
    // From the C firmware's ACQ4 baseline, 2026-10-31.
    let line = StreamFetch {
        fetch: timing_of(1020, 2_391_900, 3515),
        flush: timing_of(1020, 425_340, 418),
        retransmits: 0,
        timeouts: 0,
        rejected: 0,
    };
    assert_eq!(
        line.to_string(),
        "stream: fetch avg=2345 max=3515 us (n=1020)  flush avg=417 max=418 us  rtx=0 to=0 rej=0\r\n"
    );

    // Nothing recorded: the averages are 0, not a division by zero.
    let line = StreamFetch {
        fetch: Timing::new(),
        flush: Timing::new(),
        retransmits: 2,
        timeouts: 1,
        rejected: 3,
    };
    assert_eq!(
        line.to_string(),
        "stream: fetch avg=0 max=0 us (n=0)  flush avg=0 max=0 us  rtx=2 to=1 rej=3\r\n"
    );
}

#[test]
fn stream_rx() {
    // argus_console_printf("stream: rx n=%u copy avg=%u max=%u us  parse avg=%u max=%u us"
    //     "  loop iters=%u pkts=%u  gap avg=%u max=%u us (n=%u)"
    //     "  con drop=%u max=%u\r\n", rx_n, copy avg, copy_max, parse avg, parse_max,
    //     loop_iters, loop_pkts, gap avg, gap_max, gap_n, drops, take_max_msg);
    // From the C firmware's ACQ4 baseline, 2026-10-31.
    let line = StreamRx {
        copy: timing_of(21420, 3 * 21420, 21),
        parse: timing_of(21420, 77 * 21420, 231),
        loop_iters: 1_621_429,
        loop_pkts: 21423,
        gap: timing_of(1020, 2133 * 1020, 2247),
        con_drops: 0,
        con_max: 152,
    };
    assert_eq!(
        line.to_string(),
        "stream: rx n=21420 copy avg=3 max=21 us  parse avg=77 max=231 us  \
         loop iters=1621429 pkts=21423  gap avg=2133 max=2247 us (n=1020)  con drop=0 max=152\r\n"
    );

    // Small, hand-checkable averages: (1 + 2 + 4) / 3 = 2.
    let line = StreamRx {
        copy: timing(&[1, 2, 4]),
        parse: timing(&[10, 20, 31]),
        loop_iters: 9,
        loop_pkts: 3,
        gap: Timing::new(),
        con_drops: 4,
        con_max: 0,
    };
    assert_eq!(
        line.to_string(),
        "stream: rx n=3 copy avg=2 max=4 us  parse avg=20 max=31 us  \
         loop iters=9 pkts=3  gap avg=0 max=0 us (n=0)  con drop=4 max=0\r\n"
    );
}

#[test]
fn feat() {
    // argus_console_printf("feat: bin %u dropped %u  ch14 count %u power %u"
    //     "  ch75 count %u power %u\r\n", bin, dropped, counts[14], power[14],
    //     counts[75], power[75]);
    // From the C firmware's ACQ4 baseline, 2026-10-31.
    let line = Feat {
        bin: 1798,
        dropped: 0,
        count14: 0,
        power14: 7225,
        count75: 0,
        power75: 8684,
    };
    assert_eq!(
        line.to_string(),
        "feat: bin 1798 dropped 0  ch14 count 0 power 7225  ch75 count 0 power 8684\r\n"
    );
}

/// A bin whose channel `n` has count `n` and power `1000 + n`.
fn numbered_bin(bin: u32) -> acq::Features {
    let mut features = acq::Features::new(bin);
    for n in 0..96u32 {
        // count in bits 31:16, sum = (1000 + n) * 1500 in the low word.
        features.set(n as usize, n << 16, (1000 + n) * 1500);
    }
    features
}

#[test]
fn feature_lines_from_a_bin() {
    // The C passes counts[0], power[0], counts[14], power[14] and, on the
    // status line, counts[14], power[14], counts[75], power[75].
    let features = numbered_bin(37);
    assert_eq!(
        AcqFeatures::new(&features, 2).to_string(),
        "acq features: bin 37  ch0 count 0 power 1000  ch14 count 14 power 1014  dropped 2\r\n"
    );
    assert_eq!(
        Feat::new(&features, 2).to_string(),
        "feat: bin 37 dropped 2  ch14 count 14 power 1014  ch75 count 75 power 1075\r\n"
    );
}

#[test]
fn acq_timing_from_global_timer_ticks() {
    // At 325 MHz, 96 reads of 1083 ns are 33790 ticks (103.97 us) and a
    // 114 us frame read is 37050 ticks.
    const HZ: u32 = 325_000_000;
    let line = AcqTiming::from_ticks(33_790, 37_050, HZ);
    assert_eq!(
        line,
        AcqTiming {
            reg_ns: 1083,
            frame_us: 114
        }
    );
    // Rounded down: one tick short of 114 us reads 113.
    assert_eq!(AcqTiming::from_ticks(0, 37_049, HZ).frame_us, 113);
    // The C's hardcoded 333 ticks per us on the same 37050 ticks reads
    // 111 us: the 2.5% it under-reports by on this board.
    assert_eq!(37_050 / 333, 111);
    // No rate, or a figure past u32, shows as the maximum.
    assert_eq!(
        AcqTiming::from_ticks(1, 1, 0),
        AcqTiming {
            reg_ns: u32::MAX,
            frame_us: u32::MAX
        }
    );
    assert_eq!(AcqTiming::from_ticks(0, u64::MAX, HZ).frame_us, u32::MAX);
}

#[test]
fn the_longest_line_fits_the_console() {
    // Every field at its widest. The console drops a message of 320 bytes
    // or more; none of these may come near it.
    let m = u32::MAX;
    // One sample of u32::MAX: the widest average and maximum.
    let wide = timing(&[m]);
    let lines = [
        StreamRx {
            copy: wide,
            parse: wide,
            loop_iters: m,
            loop_pkts: m,
            gap: wide,
            con_drops: m,
            con_max: m,
        }
        .to_string(),
        StreamHalves {
            halves: m,
            underruns: m,
            failures: m,
            next: m,
            pl: ReplayStatus(m),
        }
        .to_string(),
        AcqId { id: 0 }.to_string(),
        Clk {
            apu: m,
            gtc: m,
            iopll: m,
            uart: m,
        }
        .to_string(),
        Heartbeat {
            n: m,
            con_drops: m,
            con_max: m,
        }
        .to_string(),
    ];
    for line in lines {
        assert!(line.len() < argus_fw_core::console::MSG_MAX, "{line}");
    }
}

// The lines below have no C equivalent; the expected text is the console
// contract's.

#[test]
fn banner() {
    assert_eq!(
        Banner {
            rev: "93735e15236f"
        }
        .to_string(),
        "argus-fw 93735e15236f boot\r\n"
    );
    assert_eq!(
        Banner {
            rev: "2db3cc261ae9-dirty"
        }
        .to_string(),
        "argus-fw 2db3cc261ae9-dirty boot\r\n"
    );
}

#[test]
fn clk() {
    // The Arty Z7-20 as ps7_init leaves it: 50 MHz x 26 = 1300 MHz ARM PLL
    // over 2, 50 MHz x 20 = 1000 MHz IO PLL, UART reference IO PLL / 10.
    assert_eq!(
        Clk {
            apu: 650_000_000,
            gtc: 325_000_000,
            iopll: 1_000_000_000,
            uart: 100_000_000,
        }
        .to_string(),
        "clk: apu=650000000 gtc=325000000 iopll=1000000000 uart=100000000\r\n"
    );
}

#[test]
fn heartbeat() {
    assert_eq!(
        Heartbeat {
            n: 12,
            con_drops: 0,
            con_max: 66,
        }
        .to_string(),
        "hb 12 con drop=0 max=66\r\n"
    );
}

#[test]
fn print_test() {
    let line = PrintTest.to_string();
    assert_eq!(line.len(), PRINT_TEST_BYTES);
    assert!(line.starts_with("con: print test 0123456789012"));
    // 282 digits: 28 whole runs, then "01".
    assert!(line.ends_with("678901\r\n"), "{line}");
    // It is the long line the console must still take whole.
    assert!(line.len() < argus_fw_core::console::MSG_MAX);
}

#[test]
fn print_time() {
    assert_eq!(
        PrintTime { bytes: 300, us: 7 }.to_string(),
        "con: 300-byte print took 7 us\r\n"
    );
}

#[test]
fn panic_line() {
    assert_eq!(
        Panic {
            message: format_args!("index {} out of range", 96),
            location: Some(("argus-fw/src/main.rs", 42)),
        }
        .to_string(),
        "panic: index 96 out of range at argus-fw/src/main.rs:42\r\n"
    );
    assert_eq!(
        Panic {
            message: "explicit panic",
            location: None,
        }
        .to_string(),
        "panic: explicit panic\r\n"
    );
}

#[test]
fn fault() {
    let cases = [
        (
            FaultKind::Undefined,
            0x0010_02f0,
            "fault: undefined at 001002f0\r\n",
        ),
        (
            FaultKind::PrefetchAbort,
            0xdead_beef,
            "fault: prefetch abort at deadbeef\r\n",
        ),
        (
            FaultKind::DataAbort,
            0x8,
            "fault: data abort at 00000008\r\n",
        ),
    ];
    for (kind, addr, line) in cases {
        assert_eq!(Fault { kind, addr }.to_string(), line);
    }
}

#[test]
fn eth_phy() {
    // The Arty's RTL8211E, revision 5.
    let id = PhyId {
        id1: 0x001C,
        id2: 0xC915,
    };
    assert_eq!(EthPhy { id }.to_string(), "eth: phy 001c:c915\r\n");
}

#[test]
fn eth_link() {
    let cases = [
        (Speed::Mbps1000, Duplex::Full, "eth: link up 1000 full\r\n"),
        (Speed::Mbps100, Duplex::Full, "eth: link up 100 full\r\n"),
        (Speed::Mbps10, Duplex::Half, "eth: link up 10 half\r\n"),
    ];
    for (speed, duplex, line) in cases {
        assert_eq!(EthLinkUp { speed, duplex }.to_string(), line);
    }
    assert_eq!(ETH_LINK_DOWN, "eth: link down\r\n");
    assert_eq!(ETH_NO_PHY, "eth: no phy found\r\n");
}

#[test]
fn eth_speed_assumed() {
    // A link with autonegotiation complete and PHYSR unresolved.
    assert_eq!(
        EthSpeedAssumed { physr: 0x2400 }.to_string(),
        "eth: speed unresolved (physr 2400), assuming 1000 full\r\n"
    );
    // The link-up line that follows it names the same mode.
    let (speed, duplex) = Link::ASSUMED;
    assert_eq!(
        EthLinkUp { speed, duplex }.to_string(),
        "eth: link up 1000 full\r\n"
    );
}
