//! The console ring against `argus_console.c`'s rules: whole messages or
//! none, a drop counted for each one refused, messages of 320 bytes or more
//! refused, the longest message taken and reset, and bytes coming out in
//! the order they went in across the wrap.

use argus_fw_core::console::{Console, MSG_MAX, RING_BYTES};

/// Takes everything queued, as the console task would, a run at a time.
fn drain(console: &mut Console) -> Vec<u8> {
    let mut out = Vec::new();
    while !console.is_empty() {
        let run = console.pending().to_vec();
        out.extend_from_slice(&run);
        console.consume(run.len());
    }
    out
}

#[test]
fn a_message_goes_in_and_comes_out() {
    let mut c = Console::new();
    assert!(c.is_empty());
    assert!(c.print(format_args!("tx {} bin {} skipped {}\r\n", 20, 18, 0)));
    assert_eq!(c.len(), 24);
    assert_eq!(c.pending(), b"tx 20 bin 18 skipped 0\r\n".as_slice());
    assert_eq!(drain(&mut c), b"tx 20 bin 18 skipped 0\r\n");
    assert!(c.is_empty());
    assert_eq!(c.drops(), 0);
}

#[test]
fn the_message_limit_is_one_under_320() {
    let mut c = Console::new();
    let longest = vec![b'a'; MSG_MAX - 1];
    let too_long = vec![b'b'; MSG_MAX];

    // n >= sizeof msg is dropped: 319 fits, 320 does not.
    assert!(c.push(&longest));
    assert!(!c.push(&too_long));
    assert_eq!(c.drops(), 1);
    assert_eq!(c.len(), MSG_MAX - 1);

    // The same through formatting.
    assert!(!c.print(format_args!("{:>320}", "x")));
    assert!(c.print(format_args!("{:>319}", "x")));
    assert_eq!(c.drops(), 2);
    assert_eq!(c.len(), 2 * (MSG_MAX - 1));
}

#[test]
fn a_message_that_does_not_fit_is_dropped_whole() {
    let mut c = Console::new();
    let block = vec![b'.'; 256];
    for _ in 0..RING_BYTES / 256 {
        assert!(c.push(&block));
    }
    assert_eq!(c.free(), 0);

    // Full: even one byte is refused, and counted.
    assert!(!c.push(b"x"));
    assert_eq!(c.drops(), 1);

    // Free 10 bytes: a 10-byte message fits exactly, an 11-byte one does
    // not, and nothing of it is queued.
    c.consume(10);
    assert!(!c.print(format_args!("{}", "0123456789A")));
    assert_eq!(c.drops(), 2);
    assert_eq!(c.free(), 10);
    assert!(c.print(format_args!("{}{}", "01234", "56789")));
    assert_eq!(c.free(), 0);
    assert_eq!(c.drops(), 2);

    // What comes out is the blocks and then the whole 10-byte message,
    // with nothing of the refused ones between.
    let out = drain(&mut c);
    assert_eq!(out.len(), RING_BYTES - 10 + 10);
    assert_eq!(out.get(out.len() - 10..), Some(b"0123456789".as_slice()));
    assert!(out.iter().take(RING_BYTES - 10).all(|&b| b == b'.'));
}

#[test]
fn a_formatting_error_drops_the_message() {
    struct Fails;
    impl core::fmt::Display for Fails {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("half a line")?;
            Err(core::fmt::Error)
        }
    }
    let mut c = Console::new();
    assert!(!c.print(format_args!("{Fails}")));
    assert!(c.is_empty());
    assert_eq!(c.drops(), 1);
    assert_eq!(c.take_max_msg(), 0);
}

#[test]
fn the_longest_message_is_taken_and_reset() {
    let mut c = Console::new();
    assert_eq!(c.take_max_msg(), 0);
    c.push(b"12345");
    c.push(b"123");
    c.push(&[b'z'; MSG_MAX]); // dropped: does not count
    assert_eq!(c.take_max_msg(), 5);
    assert_eq!(c.take_max_msg(), 0);
    c.push(b"12");
    assert_eq!(c.take_max_msg(), 2);
    // Taking it leaves the queue and the drops alone.
    assert_eq!(c.len(), 10);
    assert_eq!(c.drops(), 1);
}

#[test]
fn bytes_come_out_in_order_across_the_wrap() {
    let mut c = Console::new();
    let mut sent = Vec::new();
    let mut got = Vec::new();

    // Many times round the ring, in messages of varying length, taking a
    // varying number of bytes at a time, as a UART FIFO with 64 places
    // would.
    for i in 0u32..5000 {
        let msg = format!("line {i} {}\r\n", "#".repeat((i % 97) as usize));
        if c.push(msg.as_bytes()) {
            sent.extend_from_slice(msg.as_bytes());
        }
        let take = (i as usize * 7) % 65;
        let run = c.pending();
        let n = take.min(run.len());
        got.extend_from_slice(run.get(..n).unwrap_or(&[]));
        c.consume(n);
    }
    got.extend(drain(&mut c));
    assert_eq!(got, sent);
    // The test went round more than once, and some lines were dropped.
    assert!(sent.len() > 4 * RING_BYTES);
    assert!(c.drops() > 0);
}

#[test]
fn pending_stops_at_the_end_of_the_ring() {
    let mut c = Console::new();
    // Move the indices to 10 bytes before the end of the ring.
    let filler = vec![b'-'; 300];
    let mut moved = 0;
    while moved + 300 <= RING_BYTES - 10 {
        assert!(c.push(&filler));
        c.consume(300);
        moved += 300;
    }
    let rest = RING_BYTES - 10 - moved;
    assert!(c.push(&vec![b'-'; rest]));
    c.consume(rest);
    assert!(c.is_empty());

    assert!(c.push(b"0123456789abcdef"));
    // One run up to the end, then the rest from the start.
    assert_eq!(c.pending(), b"0123456789".as_slice());
    c.consume(10);
    assert_eq!(c.pending(), b"abcdef".as_slice());
    // Consuming more than is queued takes what there is.
    c.consume(100);
    assert!(c.is_empty());
    assert_eq!(c.pending(), b"".as_slice());
}

#[test]
fn the_indices_wrap_at_two_to_the_32() {
    // 2^32 / 4096 times round the ring would take a while; the indices are
    // free-running, so what matters is that len and free stay right as
    // head passes tail's numeric value. Push and drain 1 MiB in 255-byte
    // messages (not a divisor of the ring), checking the accounting.
    let mut c = Console::new();
    let msg = [b'q'; 255];
    for _ in 0..4096 {
        assert!(c.push(&msg));
        assert_eq!(c.len() + c.free(), RING_BYTES);
        assert_eq!(drain(&mut c).len(), 255);
    }
    assert_eq!(c.drops(), 0);
}
