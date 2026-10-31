# Argus Safety Controller

The **Argus Safety Controller** is the firmware on the Cortex-A9 of the
Arty Z7-20's Zynq-7020. It publishes the gateware's neural features into
the Argus ROS 2 graph as UDP telemetry, and it feeds the gateware the
recorded cortex it decodes, fetched from a dataset relay on the host. Later
it will take inbound stimulus commands from the host, and its job will be
safety: coordinating system-level constraints, validating operating modes,
enforcing safety boundaries, and deciding when FPGA-based processing or
control paths may take part in the larger closed loop.

The firmware is written in Rust, on [Embassy](https://embassy.dev) and
[zynq7000-rs](https://github.com/ustgt-irs/zynq7000-rs), with smoltcp as
the IP stack. Everything that runs on the A9 is Rust. It replaced a C
application (the Vitis standalone BSP and a vendored lwIP), which was the
specification the Rust was ported from; the [last
section](#the-c-firmware) says where it went and what is left of it.

The whole stack, and the one command that runs it on the board, is in
[argus_bringup/README.md](https://github.com/Max-Gabriel-Susman/argus_bringup/blob/main/README.md);
day-to-day operation is in [RUNBOOK.md](RUNBOOK.md).

The plan:

- [x] 1. Move the neural decoding from the argus neural interface firmware
  into the gateware (argus-neural-codec) on the PL. Done at fabric revision
  ACQ3: threshold crossings and spike-band power come from the PL and go
  out at wire frame version 3. ACQ4 is current.
- [x] 2. Port the FreeRTOS implementation to bare metal.
- [x] 3. Port the bare-metal C firmware to Rust on Embassy.
- [ ] 4. Close the loop.

## What the firmware does

In order, as the C firmware did:

1. Turns the D-cache on, prints the banner and the clocks, checks the
   timer, then runs the acquisition smoke test: reads the fabric's ID,
   status, one 96-channel frame and one feature bin. This comes before any
   networking on purpose. An AXI read of an unprogrammed PL hangs the A9,
   and that should happen right after the banner, where it is unmistakable.
2. Brings up the network: GEM0 and the board's Realtek PHY, static IP
   192.168.1.10/24, gateway 192.168.1.1, MAC 00:0a:35:00:01:02, 1000 Mb/s.
3. Fetches one replay half (147 samples x 96 channels) from the relay at
   192.168.1.20:5010 into DDR and prints four samples of it, then starts
   streaming the dataset into the PL's replay BRAM ping-pong.
4. Runs, as Embassy tasks: for every new feature bin (one per 50 ms), reads
   the bin under the fabric's hold and sends one v3 telemetry frame to
   192.168.1.20:5005; refills each BRAM half the PL has finished playing
   (one every 4.9 ms); and prints the status lines.

Where the Rust has to differ from the C (tasks instead of a superloop,
smoltcp instead of lwIP), it keeps what can be observed: the register
sequences, the packets on the wire and the console lines. The comment at
each such place says what changed and why.

## The crates

The Cargo workspace is `rust/`. The toolchain is pinned in
`rust/rust-toolchain.toml` (Rust 1.97.0, with the `armv7a-none-eabihf`
target).

| Crate | What it is |
| --- | --- |
| `argus-wire` | The wire contract: CRC-16/CCITT-FALSE, the packet structs generated from `argus_wire.h` by `rust/tools/gen-wire.sh`, and the telemetry frame encoder. |
| `argus-replay` | The replay client protocol, ported from `argus_replay_client.h`: requests, chunk checks, retransmit of the first gap, timeouts. It reaches the network and the destination buffer only through traits. |
| `argus-fw-core` | Everything the firmware decides rather than touches: the register fields, the feature arithmetic, the console ring, the PHY driver over an MDIO trait, the stream state machine over a PL trait, and the exact text of every console line. |
| `argus-difftest` | Host-only tests that compile the C replay client and the C frame packing and drive them beside the Rust, step for step, over seeded random scenarios. |
| `argus-fw` | The binary: HAL setup, the acquisition register block, the MMU remap and cache maintenance, the Embassy tasks, and the glue between them. |

Why split it this way:

- **Most of the firmware can be tested on a PC.** The first three crates
  are `no_std` (no allocator, no operating system) and forbid `unsafe`, so
  they build and run unchanged in host tests and on the A9. A plain
  `cargo test` in `rust/` runs all of them; `argus-fw` is left out of the
  workspace's `default-members` because it only builds for the A9.
- **Decisions apart from register reads.** The C reads a register and
  divides on the next line. Here the division, the checks and the line
  formatting are in `argus-fw-core`, where a test can feed them sample
  values; `argus-fw` only moves bits between them and the hardware.
- **The wire and the protocol are shared contracts.** `argus-wire` is
  generated from the same header the host uses (and the C firmware used), and
  `rust/tools/gen-wire.sh --check` fails when the two disagree.
  `argus-replay` is independent of any socket, so the same client fills a
  DDR buffer, a BRAM half or a test vector, and `argus-difftest` can hold it
  to the C client.
- **One crate with `unsafe`.** Only `argus-fw` has it, and only where the
  hardware requires it (the register block's constructor, the MMU remap,
  cache maintenance, the exception vectors), each block with a
  `// SAFETY:` comment saying why it is sound. The workspace lints deny
  `unwrap`, `expect`, `panic` and slicing that can panic, and require a doc
  comment on every public item.

The tasks in `argus-fw`, all on one Embassy executor:

| Task | Module | Job |
| --- | --- | --- |
| console drain | `console.rs` | Moves the console ring into the UART FIFO, so a print never waits on the UART. |
| stack runner | `net.rs` | The embassy-net runner: frames between the GEM and smoltcp. |
| link | `net.rs` | Polls the PHY every 100 ms, sets the link state and the GEM's speed, prints `eth:` lines. |
| telemetry | `telemetry.rs` | Waits for FEATURE_INDEX to change, reads the bin under hold, sends the frame; prints the `tx`, `feat:` and acq frame lines. |
| replay | `replay.rs` | The one-shot fetch, then the stream: the client over a UDP socket, halves into BRAM, clean, ack; answers the telemetry task's request for the `stream:` lines. |

No task may hold the executor for more than about 100 us, because a BRAM
half lasts 4.9 ms and a blocking print of the status block is what caused
the C firmware's last underruns. The one known exception is the cache clean
of a half (about 420 us), which has not caused an underrun.

## Building

Install [rustup](https://rustup.rs); the pinned toolchain, its components
and the A9 target install themselves on the first cargo command in `rust/`.
Then, from anywhere:

```bash
bash rust/tools/build-fw.sh
```

It runs `cargo build --locked --release --target armv7a-none-eabihf -p
argus-fw` and prints the ELF's path and size:
`rust/target/armv7a-none-eabihf/release/argus-fw`. The compiler and linker
flags for the A9 (the CPU, its float unit, the `z7link.x` linker script,
frame pointers) are in `rust/.cargo/config.toml`. The build stamps the
commit into the boot banner, with `-dirty` when the tree has uncommitted
changes.

The host tests, lints and docs, from `rust/`:

```bash
cargo test --locked                              # every host test, the difftests included
cargo clippy --locked --all-targets -- -D warnings
cargo doc --locked --no-deps --open
bash tools/gen-wire.sh --check                   # generated.rs matches argus_wire.h (needs bindgen-cli 0.73.2)
```

CI (`.github/workflows/ci.yml`) runs the same checks, plus clippy and a
build of the `no_std` crates for the A9.

## Programming the board

```bash
tools/program.sh
```

does what Vitis's Run button does, through XSDB and without Vitis: resets
the system, configures the PL with the bitstream, runs `ps7_init` and
`ps7_post_config` (PLLs, DDR, MIO), downloads the ELF to DDR and starts it.
There is no FSBL and no boot image; the PL is always programmed before the
firmware runs, and the download does not survive a power cycle.

By default it loads the Rust release build when this checkout has one.
`ARGUS_ELF` picks another ELF; `ARGUS_XSA` takes the bitstream and
`ps7_init.tcl` out of an exported XSA, and `ARGUS_HW_DIR` from another
checkout's Vitis platform build. With neither, the hardware comes from
this checkout's Vitis platform build if one is left from the C days, then
the C firmware's checkout (`~/Documents/argus_safety_controller`), then the
gateware's own export.
`XILINX_VITIS` overrides where Vitis is installed. Stop any Vitis debug
session first: XSDB cannot share the target.

Attach the serial console before programming (RUNBOOK.md has how): the
banner prints within milliseconds of the ELF starting.

## Testing on hardware

`hwtest.sh` in argus_bringup builds, programs, runs the whole stack under a
timeout and prints a verdict:

```bash
scripts/hwtest.sh --repo ~/Documents/argus_safety_controller_rusty --firmware --stage replay
```

- `--repo` is the firmware checkout whose `tools/program.sh` programs the
  board (the default is the C checkout).
- `--firmware` builds first; a checkout with `rust/tools/build-fw.sh` is
  built with it.
- `--stage` judges a ladder, each stage including the ones before it:

  | Stage | Passes when |
  | --- | --- |
  | `boot` | the banner, at least three more console lines, no `panic:` or `fault:` |
  | `acq` | plus `acq id=` without EXPECTED, a `feat:` line, no "hold never took effect" |
  | `net` | plus at least 8 of 10 pings answered |
  | `telemetry` | plus `tx` lines advancing, the host's `frames ok=` climbing, and none of its reject counters (`size`, `magic`, `ver`, `crc`) rising after the banner |
  | `replay` | plus `replay ok:`, at least 1015 more halves between the last two `stream:` reports, underruns 0, rtx 0, to 0, rej 0 |
  | `bitexact` | plus `hw_bitexact.py` exact on 1450 bins against the model (runs at least 110 s) |

- `--seconds N` sets the run length (default 60), `--judge <log>`
  re-judges a past log, and `--no-decode` leaves the decoder out.

The log is kept at `~/Documents/hwtest-<stamp>.log`; the board's lines in
it are prefixed `[console-N]`. Exit status 0 is PASS, 1 FAIL, 3 the board is
busy or on hold, 4 not ready (stale processes, ports in use, the console
missing or held). On 3 and 4 nothing was judged.

## The console contract

115200 8N1 on UART0 (MIO 15 TX, MIO 14 RX), every line ending in `\r\n`.
The harness and people read these lines, so the text is fixed: the same
words, field order and spacing as the C firmware, decimal unless shown as
`%04x` or `%08x`. The text of every line is in
`rust/argus-fw-core/src/lines.rs`, with host tests that hold it exact.
Lines marked *new* have no C equivalent.

| When | Line |
| --- | --- |
| first line (*new*) | `argus-fw <rev> boot`: the build's commit, 12 hex digits, `-dirty` appended when the tree had uncommitted changes |
| next | `Initializing Argus Safety Controller...` |
| boot (*new*) | `clk: apu=<hz> gtc=<hz> iopll=<hz> uart=<hz>`, read from the PLL and divider registers |
| boot (*new*) | `con: print test ...` (300 bytes), then `con: 300-byte print took <n> us`: the console returns without waiting on the UART |
| boot | `replay clock: %u ms measured over 10 ms sleep`, plus two WARNING lines when outside 5 to 20 ms |
| acq | `acq id=%08x`, or the same with `  EXPECTED 41435133 or 41435134 -- stale bitstream? Rebuild in Vivado and re-export the XSA.` |
| acq | `acq status=%08x frames/s=%u` |
| acq | `acq timing: reg %u ns/read, 96-word frame read %u us, sweep 33 us (read is held)` |
| acq | `acq frame %u: [0]=%04x [95]=%04x idx=%02x expect=%02x bad=%d`, with `(ext)` in place of `expect=%02x` once the PL plays the replay |
| acq | `acq features: bin %u  ch0 count %u power %u  ch14 count %u power %u  dropped %u` |
| acq, on timeout | `acq frame: hold never took effect` / `acq features: hold never took effect` |
| net (*new*) | `eth: phy %04x:%04x`, then `eth: link up 1000 full` (or what was negotiated), and `eth: link down` on a change |
| replay | `replay: BRAM aperture cacheable; halves flushed before ack` |
| replay | `replay ok: [0][0]=%04x [0][5]=%04x [1][0]=%04x [146][95]=%04x` or `replay FAILED` |
| replay | `replay: req=%d rtx=%d ok=%d rej=%d to=%d chunks=%d/%d` |
| stream | `stream: priming`, then `Argus Safety Controller initialized. IP 192.168.1.10` |
| telemetry (*new*) | `telemetry: unsent link down <n> refused <m>`, when sending resumes after frames were dropped (before the link is up, say) |
| every 20 frames | `tx %u bin %u skipped %u` |
| every 100 frames | `stream: halves=%u underruns=%u failures=%u next=%u pl: half=%u row=%u c0=%u c1=%u` |
| next | `stream: fetch avg=%u max=%u us (n=%u)  flush avg=%u max=%u us  rtx=%u to=%u rej=%u` |
| next | `stream: rx n=%u copy avg=%u max=%u us  parse avg=%u max=%u us  loop iters=%u pkts=%u  gap avg=%u max=%u us (n=%u)  con drop=%u max=%u` |
| next | `feat: bin %u dropped %u  ch14 count %u power %u  ch75 count %u power %u`, then the acq frame line |
| on a panic (*new*) | `panic: <message> at <file>:<line>` |
| on an exception (*new*) | `fault: <kind> at <addr %08x>`, the kind being `undefined`, `prefetch abort` or `data abort` |

`halves`, `underruns` and `failures` count from the start of streaming;
the replay client's `rtx`, `to` and `rej` count from boot; the timing
figures reset at each report. In the `stream: rx` line, `copy` is moving a
datagram out of the socket, `parse` is the client's checks, CRC and copy
into the half, and `loop iters` and `pkts` are the replay task's wakeups
and the datagrams they delivered. All times come from the global timer at
the rate its registers say (325 MHz on this board); the C assumed 333 MHz,
so its microsecond figures read about 2.5% low.

### Telemetry

One `argus_frame_packet_t` (v3, 594 bytes) per feature bin: `sample` is the
bin's FEATURE_INDEX, `t` is bin x 1500 / 30012.0, then the 96 crossing
counts, the 96 powers (the 48-bit sum of squares divided by 1500), and a
CRC-16/CCITT-FALSE over bytes 0 to 591. A frame goes out when FEATURE_INDEX
changes, never on a timer, so the rate is the fabric's.

## Measured on the board

ACQ4 fabric, firmware at commit f947216, Oct 31 2026.

| | Rust | C (ACQ4 baseline, Oct 30) |
| --- | --- | --- |
| Halves per 5 s report (real time is about 1020) | 1020 to 1021 | 1020 |
| Underruns | 0 in five 90 s runs and one 110 s run | 1 in one of three 110 s runs |
| rtx / to / rej | 0 / 0 / 0 | 0 / 0 / 0 in September's ACQ3 runs |
| Fetch per half, avg / max (us) | 1071 to 1133 / 1278 to 2451 | about 2330 / 2540 to 3515 |
| Cache clean per half, avg / max (us) | 417 / 424 | 417 / 418 |
| Parse per datagram, avg / max (us) | 24 / 33 | 77 / 215 to 232 |
| Gap between fetches, avg / max (us) | about 3390 / 4118 | about 2150 / 2270 |
| Telemetry rejected by the host for a bad CRC | 0 of 1858 | 1 and 2 of about 1770 |
| `hw_bitexact.py` | 1450 of 1450 bins exact | 1 or 2 bins missing |
| Ping | 10 of 10 | not recorded |

The C figures were taken with its 333 MHz tick and read about 2.5% low.
Other Rust boot figures: `acq timing: reg 154 ns/read, 96-word frame read 16
us`, `frames/s=30012`, a 300-byte print returns in 6 us, and the
`replay clock` check reads 10 ms over a 10 ms sleep. The largest stall on
the executor is the per-half cache clean, which leaves about 0.8 ms of
headroom against the 4.9 ms half at the worst fetch seen.

## Documentation

This README, for building and running, and `RUNBOOK.md`, for running the
whole loop and the board's known state, are the prose. The API reference
is rustdoc: `cargo doc --no-deps --open` in `rust/`. Each crate's front
page says what it holds and why, and every public item is documented (the
`missing_docs` lint, an error in CI). The structs generated from
`argus_wire.h` carry that header's comments.

## The C firmware

The C application, its Vitis component files, the vendored lwIP and the
scripts that built them (`tools/build_firmware.sh`, `tools/vendor_lwip.sh`,
the Doxygen setup) were removed once the Rust firmware had matched it on
the board, bit for bit. They are in the history: commit 604e49b is the last
with all of it. The Rust comments still name the C files and functions
they were ported from (`main.c`, `argus_acq.h`, `argus_replay.c`,
`argus_console.c`, `argus_net.c`, `lwipopts.h`), so `git show
604e49b:safety_controller/src/main.c` and the like find the original.

Two headers stay in `safety_controller/src/`, because the Rust still
depends on them:

- `argus_replay_client.h`, the C replay client. `argus-difftest` compiles
  it and holds `argus-replay` to it, step for step.
- `argus_wire.h`, the wire contract (below).

**argus_wire.h.** The canonical copy is
`argus_core/include/argus_core/argus_wire.h`. The copy in
`safety_controller/src/` must stay byte-identical (CI diffs them), and
`rust/argus-wire/src/generated.rs` is generated from it. A change to the
header lands in `argus_core` first, and `argus_sensors` on the Orin changes
with it: all of them parse the same frames.

## Glossary

- **XSA** (Xilinx Support Archive): the handoff file Vivado exports, which
  tells Vitis (and `tools/program.sh`) what hardware is in the design; it
  carries the bitstream and `ps7_init.tcl`.
- **PS / PL**: the Zynq's processing system (the A9s and their
  peripherals) and programmable logic (the fabric).
- **Half**: one of the two 147-sample regions of the replay BRAM. The PL
  plays one while the PS refills the other.
- **Bin**: one 50 ms feature window (1500 samples at 30012 Hz).
