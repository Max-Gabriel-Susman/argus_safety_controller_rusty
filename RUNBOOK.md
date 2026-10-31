# Running the loop end to end

**Repo:** `argus_safety_controller`. This is the runbook for bringing up the
full path (host relay, Zynq PS, PL, UDP telemetry, ROS graph) on
`argus-workstation-iii`. It spans three repos and a ROS workspace, so it
lives here, with the firmware, which is the thing being run. The firmware
is the Rust one in `rust/`; README.md says how it is built and how it is
put together.

It is one command. `argus_bringup`'s launch file starts the relay, the
receiver, the telemetry bridge and the decoder, streams the board console
into the same output, and (with `program:=true`) programs the FPGA and runs
the ELF through `tools/program.sh`, in the order the firmware needs: the
relay is listening before the ELF sends its first fetch request.

```
launch -+- preflight            route, dataset, training set, serial port
        +- console              /dev/ttyUSB1 -> [console-2] lines
        +- dataset_relay_node   host -> board   (UDP 5010)
        +- neural_udp_receiver  board -> ROS    (UDP 5005)
        +- telemetry bridge     -> /argus/sensors/neural_telemetry
        +- inference_node       -> /cmd_vel
        +- program.sh           bitstream + ELF via XSDB   (program:=true)
```

## One-time setup

Everything in this section survives reboots except the network address.

**ROS workspace.** All four packages built in one place:

```bash
source /opt/ros/humble/setup.bash
cd ~/Documents/argus_ws && colcon build
```

A package "not found" by `ros2 run` is a package that was not built *here*;
`ls install/` shows what is.

**Alias.** ROS and Vitis environments conflict, so neither goes in `.bashrc`.
In `~/.bash_aliases`:

```bash
alias rosenv='source /opt/ros/humble/setup.bash && source ~/Documents/argus_ws/install/setup.bash'
```

The workspace path is the one that goes stale when things move.

**Rust.** [rustup](https://rustup.rs), once. The toolchain pinned in
`rust/rust-toolchain.toml` and the A9 target install themselves on the
first build.

**Vitis.** Only its `xsdb` is needed to program the board (at
`/tools/Xilinx/2026.1/Vitis`, or wherever `XILINX_VITIS` says). The IDE is
needed only to rebuild a Vitis platform.

**Serial.** Membership of `dialout`, once, then log out and back in (group
membership is read at login, so a new terminal is not enough):

```bash
sudo usermod -aG dialout "$USER"
```

**Network.** The board is on the USB Ethernet adapter, on its own subnet. The
address does not persist and NetworkManager will take the interface back
unless told not to:

```bash
sudo nmcli device set enx00e04c685e7e managed no
sudo ip addr add 192.168.1.20/24 dev enx00e04c685e7e
sudo ip link set enx00e04c685e7e up
ip route get 192.168.1.10          # must say dev enx00e04c685e7e, not wlp0s20f3
```

If the last line names the Wi-Fi interface, the board is unreachable. NordVPN
does this: its tunnel captures 192.168.1.0/24. Disconnect it before a run.
The board's addresses are fixed in the firmware: it is 192.168.1.10 and
sends to 192.168.1.20.

**Data.** Raw datasets live in `~/argus_data/`, outside every repo. A
converted segment for the relay:

```bash
python3 ~/Documents/argus_ws/src/argus_sim/tools/nwb_to_replay.py \
  ~/argus_data/indy_20161005_06_broadband.nwb --start 120 --seconds 10 \
  --out ~/argus_data/indy_20161005_06_s120_10s.bin
```

**The hardware.** `tools/program.sh` needs a bitstream and the PS
initialisation (`ps7_init.tcl`) to go with the ELF. It takes them from an
exported XSA (`ARGUS_XSA=...`), or from a Vitis platform build: this
checkout's if one is left from the C firmware, then the C firmware's
checkout (`~/Documents/argus_safety_controller`), then the gateware's own
export (`~/Documents/argus-neural-codec/argus_neural_codec.xsa`). After a
gateware build, either point `ARGUS_XSA` at the new export or update the
platform in Vitis (`arty_z7_platform`, Settings, `vitis-comp.json`, Update
XSA, then build the platform). The `program: bit` line it prints has the
bitstream's time; check it is the build you meant.

## The run

```bash
bash rust/tools/build-fw.sh          # if the source changed
rosenv
ros2 launch argus_bringup argus.launch.py program:=true
```

`Ctrl-C` ends it. `ros2 launch argus_bringup argus.launch.py --show-args`
lists the rest: `dataset:=` and `mat:=` pick the replay segment and the
training set, `relay:=`, `receiver:=`, `decode:=` and `console:=` switch
parts off, and `program:=false` (the default) leaves a board that is already
running alone. Nothing may hold `/dev/ttyUSB1` (a `screen`, see below) and
no node from a previous run may be alive: `pgrep -af -- --ros-args` must
print nothing. A leftover relay still owns UDP 5010 and answers some of the
board's fetches.

To use a saved counts+power model instead of training on the `.mat`, export
`ARGUS_MODEL_PATH=/path/to/model.pkl` (from `decode_test.py --save-model`)
before the launch.

**Build, run and judge in one step.** The hardware-in-the-loop cycle, with
timeouts and a verdict:

```bash
~/Documents/argus_ws/src/argus_bringup/scripts/hwtest.sh \
  --repo ~/Documents/argus_safety_controller_rusty --firmware --stage replay [--seconds N]
```

`--repo` names the checkout whose `tools/program.sh` programs the board
(without it, the C checkout), `--firmware` builds it first with
`rust/tools/build-fw.sh`, and `--stage` picks how much has to work:
`boot`, `acq`, `net`, `telemetry`, `replay` or `bitexact`, each including
the ones before (README.md has the table). The run lasts N seconds (default
60; `bitexact` needs at least 110), the log is kept at
`~/Documents/hwtest-<stamp>.log`, and the verdict prints `PASS` or `FAIL`
with the last two `stream:` lines and the last `feat:` line. `--judge <log>`
re-judges a past log. The board keeps running the previous firmware until
it is reprogrammed, so the lines before the `argus-fw ... boot` banner in a
log belong to the old image. One run holds the board at a time; a second
waits, then exits 3 (busy).

**Manual, for debugging one piece.** Every node still runs on its own:
`ros2 run argus_sim dataset_relay_node --ros-args -p dataset_path:=...`,
`ros2 run argus_sensors neural_udp_receiver`, the serial console below,
and `tools/program.sh`. Start the relay and the console first: the
firmware fetches within a second of starting, and the banner prints as the
ELF starts.

## The serial console

UART0 (MIO 14/15) at 115200 8N1, on the same micro-USB cable as JTAG. The
Arty enumerates two FTDI channels: the first is JTAG, the second the UART,
usually `/dev/ttyUSB1`, but the numbering shifts with other USB serial
devices. Interface `01` is the UART:

```bash
for d in /dev/ttyUSB*; do
  udevadm info -q property -n "$d" | grep -E 'ID_USB_INTERFACE_NUM|ID_MODEL='
done
```

Attach, before programming:

```bash
screen /dev/ttyUSB1 115200
screen -L -Logfile /tmp/argus-uart.log /dev/ttyUSB1 115200   # and keep a log
```

A blank screen means attached and waiting; nothing appears until the board
transmits. `picocom -b 115200 /dev/ttyUSB1` (exit `Ctrl-A Ctrl-X`) works as
well.

**Exiting.** `screen` has two exits and only one releases the port. Press
and release `Ctrl-A` first, then the next key:

| Keys | Effect |
| --- | --- |
| `Ctrl-A` then `k`, then `y` | Kill the session and release the port. This is the one you want. |
| `Ctrl-A` then `d` | Detach. The session keeps running *and keeps holding the port*. |
| `Ctrl-C` | Sent to the board. Does **not** exit. |
| Closing the window | Same as a detach. |

A detached session still owns `/dev/ttyUSB1`: the next `screen` shows
nothing or fails with *Device or resource busy*, and the launch's console
cannot open the port. It reads as a dead board. `screen -ls` lists the
sessions, `screen -X -S <session> quit` kills one, `pkill screen` all.

## What healthy looks like

**Console** (`[console-2]` in the launch output), in order. The values are
from a run on ACQ4 with the default `.bin` dataset:

```
argus-fw f9472167d6f3 boot            <- the commit this image was built from
Initializing Argus Safety Controller...
clk: apu=650000000 gtc=325000000 iopll=1000000000 uart=100000000
con: print test 0123456789...         <- 300 bytes, then how long the print took
con: 300-byte print took 6 us
replay clock: 10 ms measured over 10 ms sleep
acq id=41435134                       <- fabric revision ACQ4
acq status=00000001 frames/s=30012    <- sweep rate
acq timing: reg 154 ns/read, 96-word frame read 16 us, sweep 33 us (read is held)
acq frame 31513: [0]=0018 [95]=9f18 idx=18 expect=18 bad=0   <- identity pattern
acq features: bin 21  ch0 count 0 power 2012  ch14 count 0 power 2012  dropped 0
eth: phy 001c:c916                    <- the PHY's identifier: an RTL8211F
replay: BRAM aperture cacheable; halves flushed before ack
tx 20 bin 43 skipped 0                <- telemetry starts before the link is up
tx 40 bin 63 skipped 0
eth: link up 1000 full
telemetry: unsent link down 55 refused 0
tx 60 bin 83 skipped 0
telemetry: unsent link down 55 refused 18
replay ok: [0][0]=8385 [0][5]=8194 [1][0]=839f [146][95]=8272
replay: req=1 rtx=0 ok=21 rej=0 to=0 chunks=21/21
stream: priming
Argus Safety Controller initialized. IP 192.168.1.10
tx 80 bin 3 skipped 0                 <- one line per second, bin +20, skipped stays 0
```

Then every 5 s three `stream:` lines, a `feat:` line and an `acq frame`
line:

```
stream: halves=1256 underruns=0 failures=0 next=184632 pl: half=1 row=19 c0=1 c1=0
stream: fetch avg=1073 max=1288 us (n=1020)  flush avg=416 max=425 us  rtx=0 to=0 rej=0
stream: rx n=21420 copy avg=2 max=7 us  parse avg=24 max=32 us  loop iters=7742 pkts=21420  gap avg=3407 max=4118 us (n=1021)  con drop=0 max=144
feat: bin 123 dropped 0  ch14 count 2 power 13293  ch75 count 0 power 6726
acq frame 184505: [0]=8217 [95]=8006 idx=17 (ext) bad=96
```

Line by line:

- **`argus-fw <rev> boot`**: the commit the image was built from, with
  `-dirty` if the tree had uncommitted changes. If it is not the commit you
  expected, the board is running another build.
- **`clk:`**: the clocks as the PLL and divider registers set them. The
  Arty's PS reference is 50 MHz, so the A9 runs at 650 MHz and the global
  timer at 325 MHz. All timing figures use these, never a constant.
- **`con:`**: a 300-byte print returns in microseconds because it only
  copies into the console ring; a task drains the ring into the UART.
- **`acq id=41435134`**: ACQ4 (ACQ3, `41435133`, is accepted too). Anything
  else prints `EXPECTED ... -- stale bitstream?`. `deadbeef` means the
  address map disagrees with `argus_acq.h`.
- **`acq status=00000001 frames/s=30012`**: bit 0 is `ready`, bit 1 would
  be an assembler overrun. The rate is exactly 125e6 / (119 x 35).
- **`acq frame N: ... bad=0`**: one 96-word frame read under the hardware
  hold. Before streaming, every word carries its own chip and channel and
  `idx` equals `expect`. Once the PL plays the replay the line says `(ext)`
  and `idx` is the dataset's; with a real dataset `bad=96` is expected (the
  identity check runs on real data), with the relay's synthetic pattern it
  is 0.
- **`eth: phy 001c:c916`, `eth: link up 1000 full`**: the PHY answered at
  MDIO address 1 and autonegotiated. The board carries an RTL8211F
  (`001c:c916`), not the RTL8211E its manual names; the firmware drives
  both.
- **`telemetry: unsent ...`**: frames that could not be sent, counted from
  boot, printed when sending resumes. Before the link is up and while ARP
  for the host resolves, a few dozen are normal. Climbing later is not.
- **`replay ok:`**: the one-shot fetch into DDR, four samples printed.
  Proves the host-to-PS path before the PL depends on it. The fetch starts
  once the link is up.
- **`replay: req=1 rtx=0 ok=21 rej=0 to=0`**: one request, no
  retransmits, all 21 chunks accepted, none rejected, no timeouts.
- **`stream: halves=... underruns=... failures=...`**: `halves` counts
  BRAM halves filled since streaming began, `next` is the dataset offset of
  the next fetch, and `pl:` is the half the PL is playing, its row, and
  whether each half is waiting for a refill.
- **`stream: fetch ...`**: per half, `fetch` is request to last chunk and
  `flush` is the cache clean of the half before the ack. `rtx`, `to`, `rej`
  are the replay client's counters since boot.
- **`stream: rx ...`**: per datagram, `copy` out of the socket and `parse`
  (checks, CRC, copy into the half); `loop iters` and `pkts` are the replay
  task's wakeups and the datagrams they delivered; `gap` is the idle time
  from one fetch finishing to the next request; `con drop` counts console
  lines dropped whole because the ring was full (since boot), `max` the
  longest line in the interval.
- **`feat:`**: the latest feature bin for two channels. `dropped` stays 0
  and power sits in the thousands.

`tx` and `bin` differ: `bin` restarts when streaming switches the fabric to
external mode.

**`underruns` is the number to watch over a long run.** It must stay 0
while `halves` rises by about 1020 per report, which is real time. Real
time needs fetch plus flush under 4.9 ms per half.

**Receiver** (`[neural_udp_receiver-4]`):

```
frames ok=1458 size=0 magic=0 ver=0 crc=0
```

`ok` rises by 100 per 5 s window (20 Hz), the four failure counters stay 0.

**Echo** (another shell, `rosenv` first):
`ros2 topic echo /argus/neural_interface_bridge/neural_data --once` shows
`channel_count: 96`, 96 small integers in `channels` (crossing counts per
50 ms bin, mostly 0 with single digits on the live channels) and 96 values
in the thousands in `power` (mean-square, code^2). `sample` is the bin
number and `t` is `sample x 0.04998`.

**Decoder** (`[inference_node-6]`): a line naming the model path, then
`offline 4-way intent accuracy: 0.440` (`.mat` path), then one line per
second with `sample` advancing by 20 and an `intent -> vx wz` pair.

## Known state, as of 2026-10-31

- **The Rust firmware does everything the C did, on ACQ4.** Replayed
  cortex, simulated chips, SPI, crossings and spike-band power in fabric,
  UDP, DDS, LDA, `/cmd_vel` at 20.0 Hz. Telemetry is paced by the fabric's
  bin counter, so the rate is exact.
- **Replay runs at real time with margin.** Five 90 s runs in a row and a
  110 s run: 1020 to 1021 halves per report, underruns 0, failures 0,
  rtx/to/rej 0. Fetch averages about 1.1 ms per half (the C's was about
  2.3 ms) and parse 24 us per datagram (the C's 77); the clean is 417 us
  in both. The C's ACQ4 baseline had one underrun in three 110 s runs.
- **Telemetry is clean.** 1858 frames in the 110 s run with `crc=0` and
  size/magic/ver 0; the C build lost one or two frames per run on ACQ4 to a
  bad CRC. Each frame is copied into a GEM transmit buffer that the HAL
  cleans to DDR before the GEM is given it and does not reuse until the
  GEM has handed its descriptor back, so nothing the GEM reads is written
  while it reads it.
- **The codec is bit-exact on silicon.** `argus_sim/tools/hw_bitexact.py`
  matched every count and power in 1450 of 1450 bins against
  `spike_features.py`.
- **Caveats.** The largest stall on the executor is the cache clean of a
  half, about 420 us; the worst fetch seen (2451 us) still left about
  0.8 ms of the 4.9 ms half. Console lines are not printed the moment they
  are queued; a line that does not fit in the ring is dropped whole and
  counted in `con drop` (0 so far). Power is on the wire at frame version
  3; a v2 host shows up as a climbing `ver=` on the receiver.

## Shutting down

`Ctrl-C` the launch once and let it finish; every node exits and the
console releases `/dev/ttyUSB1`. Then check that nothing survived:
`pgrep -af -- --ros-args` prints nothing. A node that outlived its launch
keeps its port (a relay keeps 5010) and confuses the next run; `kill` it.

For a manual run: `Ctrl-A k y` in `screen` (*not* `Ctrl-A d`), then
`Ctrl-C` the nodes. If a Vitis debug session is open, stop it before
pulling the USB cable: pulling it under a live session wedges XSDB.

## When it does not work

| Symptom | Cause | Fix |
| --- | --- | --- |
| No console output at all after programming | console attached after the banner, or attached to the JTAG channel | attach first; check `ID_USB_INTERFACE_NUM` is `01` |
| Banner, then nothing after `replay clock` | the A9 hung on the first AXI read: the PL is not programmed | check `program:` named a bitstream and `fpga -file` ran |
| `acq id=41435132  EXPECTED ... -- stale bitstream?` | the hardware `program.sh` used predates ACQ3 | `ARGUS_XSA=` the current export, or Update XSA and rebuild the platform |
| `acq id=deadbeef` | register block answering at the wrong address | address map vs `argus_acq.h` |
| `acq frame: hold never took effect` | `STATUS.held` never set after `CTRL.hold`: old bitstream or wedged fabric | same as a stale bitstream; `hwtest.sh` fails on it |
| `eth: no phy found` | nothing answered on MDIO | cable, power; the PHY reset is MIO 9 |
| `eth: phy ... is not an RTL8211E or RTL8211F` | a PHY the driver does not know | `rust/argus-fw-core/src/phy.rs` |
| No `eth: link up` | no carrier | cable, the host adapter up |
| `telemetry: unsent ...` climbing after the link is up | the host is not resolving | `ip route get 192.168.1.10`; host address 192.168.1.20 |
| `replay FAILED` with `to=` nonzero, `ok=0` | requests leave, no replies: relay not running, firewall, VPN route | start the relay first; `sudo ufw status`; disconnect NordVPN |
| `replay FAILED` with `rej=` nonzero | replies arrive but are rejected | `chunk_total` or `channel_count` mismatch between relay and firmware |
| `underruns` climbing | fetches slower than 4.9 ms per half | the `stream: fetch` and `gap` figures; a second relay answering (`pgrep -af -- --ros-args`) |
| `panic: ... at <file>:<line>` | a bug; the core parks | the file and line name it |
| `fault: data abort at <addr>` | a bad memory access; the core parks | `addr` is the faulting instruction; look it up in the ELF |
| `WARNING: replay clock advanced 0 ms` | the global timer is not running | retransmits never fire; check `clk: gtc=` |
| `program.sh: xsdb not found` | Vitis elsewhere | `XILINX_VITIS=` |
| `program.tcl: no JTAG targets after 20 s` | board off, USB out, or a wedged `hw_server` | power and cable; `pkill -f hw_server` if Vitis is closed |
| `Package 'argus_sensors' not found` | not built in `~/Documents/argus_ws` | `colcon build`, see one-time setup |
| `.../argus_ws/install/setup.bash: No such file` | `rosenv` points at an old workspace | fix the path in `~/.bash_aliases` |
| `topic ... does not appear to be published yet` | receiver not running | `receiver:=true` (the default) |
| `The message type 'argus_core/msg/NeuralFrame' is invalid` | the shell has base ROS but not the workspace overlay | `rosenv` in that shell |
| *Device or resource busy* on `screen`, or a silent console | a detached `screen` holds the port | `screen -ls`, `screen -X -S <id> quit` |
| `Destination Host Unreachable` from ping | no route on the USB NIC | one-time network setup again |
| `tx ... skipped N` with N > 0 | bins missed: the hold failing, or the telemetry task held up | should not happen; report it |
| `pgrep -af -- --ros-args` lists nodes before a run | a previous launch left a node alive | `kill` it; a stale relay still owns UDP 5010 |
| Receiver `ver=` climbing, `ok=0` | the host's `argus_wire.h` is behind the board's | sync from `argus_core`, rebuild `argus_sensors` |

The C firmware is no longer in this repository (commit 604e49b is the last
with it). Its own checkout, `~/Documents/argus_safety_controller`, still
builds in Vitis, and `hwtest.sh` without `--repo` programs that checkout.
