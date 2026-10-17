# Running the loop end to end

**Repo:** `argus_safety_controller`. This is the runbook for bringing up the
full path — host relay → Zynq PS → PL → UDP telemetry → ROS graph — on
`argus-workstation-iii`. It spans three repos and a ROS workspace, so it lives
here, with the firmware, which is the thing being run.

It is one command. `argus_bringup`'s launch file starts the relay, the
receiver, the telemetry bridge and the decoder, streams the board console
into the same output, and (with `program:=true`) programs the FPGA and runs
the ELF through XSDB, in the order the firmware needs: the relay is
listening before the ELF sends its first fetch request.

```
launch ─┬─ preflight            route, dataset, training set, serial port
        ├─ console              /dev/ttyUSB1 → [console-2] lines
        ├─ dataset_relay_node   host → board   (UDP 5010)
        ├─ neural_udp_receiver  board → ROS    (UDP 5005)
        ├─ telemetry bridge     → /argus/sensors/neural_telemetry
        ├─ inference_node       → /cmd_vel
        └─ program.sh           bitstream + ELF via XSDB   (program:=true)
```

## One-time setup

Everything in this section survives reboots except the network address.

**ROS workspace.** All four packages built in one place:

```bash
source /opt/ros/humble/setup.bash
cd ~/Documents/argus_ws && colcon build
```

A package "not found" by `ros2 run` is a package that was not built *here* —
`ls install/` shows what is.

**Alias.** ROS and Vitis environments conflict, so neither goes in `.bashrc`.
In `~/.bash_aliases`:

```bash
alias rosenv='source /opt/ros/humble/setup.bash && source ~/Documents/argus_ws/install/setup.bash'
```

The workspace path is the one that goes stale when things move.

**Serial.** Membership of `dialout`, once, then log out and back in:

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

**Data.** Raw datasets live in `~/argus_data/`, outside every repo. A
converted segment for the relay:

```bash
python3 ~/Documents/argus_ws/src/argus_sim/tools/nwb_to_replay.py \
  ~/argus_data/indy_20161005_06_broadband.nwb --start 120 --seconds 10 \
  --out ~/argus_data/indy_20161005_06_s120_10s.bin
```

**Vitis.** Launch it with the workspace on the command line, always:

```bash
vitis -w ~/Documents/argus_safety_controller
```

A bare `vitis` from some other directory sets `${workspaceFolder}` to that
directory, and `launch.json` resolves the ELF path against it. The symptom is
`Failed to download .../safety_controller/build/safety_controller.elf` with a
path missing the repo directory.

**After every gateware build**, the platform has to be told: `arty_z7_platform`
→ Settings → `vitis-comp.json` → Update XSA (same path), then build the
platform, then the app. Rebuilding the app alone reprograms the *old*
bitstream. Check before Run:

```bash
ls -l --time-style=long-iso \
  ~/Documents/argus_safety_controller/safety_controller/_ide/bitstream/argus_neural_codec.bit \
  ~/Documents/argus-neural-codec/argus_neural_codec.xsa
```

The `.bit` must be newer than the `.xsa`.

## The run

```bash
rosenv
ros2 launch argus_bringup argus.launch.py program:=true
```

`Ctrl-C` ends it. `ros2 launch argus_bringup argus.launch.py --show-args`
lists the rest: `dataset:=` and `mat:=` pick the replay segment and the
training set, `relay:=`, `receiver:=`, `decode:=` and `console:=` switch
parts off, and `program:=false` (the default) leaves a board that is already
running alone. Nothing may hold `/dev/ttyUSB1` (a `screen`, see the table
below) and no node from a previous run may be alive: `pgrep -af -- --ros-args`
must print nothing. A leftover relay still owns UDP 5010 and answers some
of the board's fetches.

To use a saved counts+power model instead of training on the `.mat`, export
`ARGUS_MODEL_PATH=/path/to/model.pkl` (from `decode_test.py --save-model`)
before the launch.

**Build, run and judge in one step.** The hardware-in-the-loop cycle, with
timeouts and a verdict:

```bash
~/Documents/argus_ws/src/argus_bringup/scripts/hwtest.sh [--fabric] [--firmware] [--seconds N] [--no-decode]
```

`--fabric` rebuilds the bitstream (refuses on a timing failure) and
`--firmware` rebuilds the ELF. It then runs the launch above with
`program:=true` for N seconds (default 60), keeps the log at
`~/Documents/hwtest-<stamp>.log`, and prints `PASS` or `FAIL`, the last two
`stream:` lines and the last `feat:` line. `--judge <log>` re-judges a past
log. The board keeps running the previous firmware until it is reprogrammed,
so the lines before the `acq id=` banner in a log belong to the old image.

**Manual, for debugging one piece.** Every node still runs on its own:
`ros2 run argus_sim dataset_relay_node --ros-args -p dataset_path:=...`,
`ros2 run argus_sensors neural_udp_receiver`, `screen /dev/ttyUSB1 115200`,
and Vitis → `safety_controller` → **Run**. Start the relay and the console
before Run: the firmware fetches within milliseconds of starting, and the
banner prints as the ELF starts.

## What healthy looks like

**Console** (`[console-2]` in the launch output), in order. The first line is
the one to read:

```
Initializing Argus Safety Controller...
acq id=41435133                       <- fabric revision ACQ3 matches firmware
acq status=00000001 frames/s=30012    <- sweep rate
acq timing: reg 213 ns/read, 96-word frame read 22 us, sweep 33 us (read is held)
acq frame 31652: [0]=00A3 [95]=9FA3 idx=A3 expect=A3 bad=0   <- identity pattern
acq features: bin 21  ch0 count 0 power 2012  ch14 count 0 power 2012  dropped 0
Configuring PHY for fixed 1000 Mbps mode
link speed for phy address 1: 1000
replay clock: 10 ms measured over 10 ms sleep
replay: BRAM aperture cacheable; halves flushed before ack
replay ok: [0][0]=8385 [0][5]=8194 [1][0]=839F [146][95]=8272
replay: req=1 rtx=0 ok=21 rej=0 to=0 chunks=21/21
stream: priming
Argus Safety Controller initialized. IP 192.168.1.10
tx 20 bin 18 skipped 0                <- one line per second, bin +20, skipped stays 0
```

Then every ~5 s three `stream:` lines, a `feat:` line and an `acq frame` line:

```
stream: halves=1001 underruns=0 failures=0 next=147147 pl: half=0 row=4 c0=0 c1=1
stream: fetch avg=2329 max=8204 us (n=1001)  flush avg=417 max=418 us  rtx=0 to=0 rej=0
stream: rx n=21042 copy avg=2 max=21 us  parse avg=77 max=208 us  loop iters=1753738 pkts=21022  gap avg=2155 max=2268 us (n=999)  con drop=0 max=89
feat: bin 98 dropped 0  ch14 count 1 power 14140  ch75 count 1 power 9892
acq frame 147006: [0]=7fb1 [95]=7f7f idx=b1 (ext) bad=96
```

The first `stream:` line is the replay state: `next` is the dataset offset
of the next fetch, `underruns` counts halves the fabric replayed stale. It
should stay 0: `halves` rises by about 1020 per interval, which is real time. The
second is its cost per half: `fetch` from request to last chunk, `flush`
the cache write-back, and the relay client's retransmit, timeout and
reject counters. Real time needs fetch + flush under 4.9 ms per half. The
first interval's fetch max of about 8 ms is priming. The third line is the
receive path per packet. `gap` is the idle wait from one fetch finishing
to the next request, about 2.1 ms of slack. `con drop` counts console
messages dropped because the console ring was full (since boot), and `max` is the
longest message in the interval. These lines are queued and drained into
the UART without blocking (`argus_console.c`), so they no longer stall the
loop.
`dropped` stays 0 and power sits in the thousands. The frame line's `bad=96`
is the identity-pattern check running on real data — expected, not a fault.
`tx` and `bin` differ by a few: `bin` restarts when streaming enters ext mode.

The first line is the one that matters. `acq id=41435132  EXPECTED 41435133
-- stale bitstream?` means the platform still carries the previous fabric;
the firmware then parks after `acq features: hold never took effect` and
sends nothing. See the one-time Vitis section.

**Receiver** (`[neural_udp_receiver-4]`):

```
frames ok=176 size=0 magic=0 ver=0 crc=0
```

`ok` rises by 100 per 5 s window (20 Hz), the four failure counters at 0.

**Echo** (another shell, `rosenv` first):
`ros2 topic echo /argus/neural_interface_bridge/neural_data --once` shows
`channel_count: 96`, 96 small integers in `channels` (crossing counts per
50 ms bin, mostly 0 with single digits on the live channels) and 96 values
in the thousands in `power` (mean-square, code²). `sample` is the bin
number and `t` is `sample × 0.04998`.

**Decoder** (`[inference_node-6]`): a line naming the model path, then
`offline 4-way intent accuracy: 0.440` (`.mat` path), then one line per
second with `sample` advancing by 20 and an `intent -> vx wz` pair.

## Known state, as of 2026-09-28

- **The pipeline is complete.** Replayed cortex → simulated chips → SPI →
  crossings and spike-band power in fabric → UDP → DDS → LDA → `/cmd_vel`
  at 20.0 Hz. Telemetry is paced by the fabric's bin counter, so the rate
  is exact.
- **Replay runs at real time.** About 203 halves/s against the 204 the
  fabric plays: fetch 2.3–2.4 ms per half (the table-driven CRC-16 in
  `argus_wire.h`), flush 417 us, rtx/to/rej 0, no pbuf allocation failures.
  Two lwIP fixes got it there. `SYS_LIGHTWEIGHT_PROT 1` stops the pbuf
  pool corrupting under the Ethernet interrupts, which used to wedge
  the stream. `LWIP_ALLOW_MEM_FREE_FROM_OTHER_CONTEXT 1` makes the
  heap safe from the TX-done interrupt's `mem_free`, the likely cause of an
  occasional bad telemetry CRC. The last underruns came from the 5 s
  status block: about 470 characters through blocking `xil_printf` at
  115200 baud stalled the loop for 35 ms. Console output in the main loop
  now goes through a 4096-byte ring drained into the UART FIFO without
  blocking. `underruns` has stayed 0 over a whole 90 s run; one run showed
  a single underrun.
- **The codec is bit-exact on silicon.** `argus_sim/tools/hw_bitexact.py`
  matched every count and power in 1450 bins of a 90 s run (seven loops of
  the replay file) against `spike_features.py`.
- **Caveats.** The CRC fix has five clean 90 s runs behind it (7260
  frames, `crc=0` and size/magic/ver 0 in every one). At the old rate of
  about one bad frame in 2900, 7260 frames would show two or three. The
  chance of seeing none is about 8 %, so this is good evidence but not
  proof. Console lines are
  not printed at the moment they are queued; a line that does not fit in the ring is
  dropped whole and counted in `con drop`. Messages before the main loop
  (banner, smoke test, replay check) still block, which does not matter
  there.
- **Power is on the wire at frame version 3** (`power[96]`, mean-square =
  sum / 1500). Host and firmware must both be v3; a v2 end shows up as a
  climbing `ver=` on the receiver.

## Shutting down

`Ctrl-C` the launch once and let it finish; every node exits and the
console releases `/dev/ttyUSB1`. Then check that nothing survived:
`pgrep -af -- --ros-args` prints nothing. A node that outlived its launch
keeps its port (a relay keeps 5010) and confuses the next run; `kill` it.

For a manual run: stop the Vitis debug session first (pulling the USB cable
under a live session wedges XSDB), then `Ctrl-A k y` in `screen` (*not*
`Ctrl-A d`: a detached `screen` still owns `/dev/ttyUSB1`), then `Ctrl-C`
the nodes.

## When it does not work

| Symptom | Cause | Fix |
| --- | --- | --- |
| `Package 'argus_sensors' not found` | not built in `~/Documents/argus_ws` | `colcon build`, see one-time setup |
| `.../argus_ws/install/setup.bash: No such file` | `rosenv` points at an old workspace | fix the path in `~/.bash_aliases` |
| `topic ... does not appear to be published yet` | receiver not running | `receiver:=true` (the default) |
| `replay FAILED ... to=6 chunks=0/21` | relay not up before the ELF ran (manual run) | start the relay first, or use the launch |
| `replay FAILED` with the relay up | VPN tunnel has the subnet | `ip route get 192.168.1.10`; disconnect NordVPN |
| `acq id=41435131  EXPECTED 41435132 -- stale bitstream?` | fabric predates firmware | `build_bitstream.tcl`, re-read XSA, rebuild platform + app |
| `acq frame: hold never took effect` | same, older firmware | same |
| `Failed to download .../Documents/safety_controller/build/...elf` | Vitis launched without `-w` | relaunch `vitis -w ~/Documents/argus_safety_controller` |
| `Failed to connect to target 127.0.0.1:3121 Reason: abort` | previous XSDB session wedged | File → Exit, `pkill -f hw_server; pkill -f xsdb`, relaunch |
| `Opening COM4: Access denied` / blank console after attach | a `screen` is still attached elsewhere | `screen -ls`, `screen -X -S <id> quit` |
| `[screen is terminating]` immediately | same: a stale session holds the port | `screen -X -S "$(screen -ls \| awk '/tty\|pts/ {print $1; exit}')" quit`, then attach |
| Console attached but silent for minutes, receiver sees nothing | firmware parked on a stale fabric | `ls -l` the `.bit` vs the `.xsa`; Update XSA, build platform, build app |
| `The message type 'argus_core/msg/NeuralFrame' is invalid` | shell has base ROS but not the workspace overlay | `rosenv` in that shell |
| `Package 'argus_inference' not found` with the package built | same | `rosenv` |
| `Destination Host Unreachable` from ping | no route on the USB NIC | one-time network setup again |
| `tx ... skipped N` with N > 0 | hold failing on the fabric | should not happen on ACQ3; report it |
| `pgrep -af -- --ros-args` lists nodes before a run | a previous launch left a node alive | `kill` it; a stale relay still owns UDP 5010 |
| Receiver `ver=` climbing, `ok=0` | host `argus_wire.h` behind the board's | sync from `argus_core`, rebuild `argus_sensors` |
