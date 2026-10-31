# tools/program.tcl -- what the Vitis Run button does, as an XSDB script.
#
#   xsdb tools/program.tcl [repo]
#   xsdb tools/program.tcl <bit> <xsa> <ps7_init.tcl> <elf>
#
# or tools/program.sh, which finds xsdb and the four files. The sequence is
# the one Vitis prints in its debug console: reset the system, configure the
# PL with the bitstream, load the hardware description, run ps7_init, reset
# the A9, download the ELF, go. With only a repo, the paths are that
# checkout's Vitis platform and app build outputs. The script refuses to run
# against a missing file rather than program half a system.
#
# Fails if a Vitis debug session holds the target -- stop it first.

set here [file dirname [file normalize [info script]]]

if {$argc >= 4} {
  lassign $argv bit xsa init elf
} else {
  set repo [expr {$argc >= 1 ? [lindex $argv 0] : [file dirname $here]}]
  set bit  $repo/safety_controller/_ide/bitstream/argus_neural_codec.bit
  set xsa  $repo/arty_z7_platform/export/arty_z7_platform/hw/argus_neural_codec.xsa
  set init $repo/safety_controller/_ide/psinit/ps7_init.tcl
  set elf  $repo/safety_controller/build/safety_controller.elf
}

foreach f [list $bit $xsa $init $elf] {
  if {![file exists $f]} {
    error "program.tcl: missing $f -- build the platform and app first (or set ARGUS_ELF / ARGUS_HW_DIR / ARGUS_XSA)"
  }
}

puts "program: bit  [clock format [file mtime $bit] -format %H:%M:%S]  $bit"
puts "program: init [clock format [file mtime $init] -format %H:%M:%S]  $init"
puts "program: elf  [clock format [file mtime $elf] -format %H:%M:%S]  $elf"

connect

# connect launches hw_server if none is running, and returns before the
# cable has enumerated: with Vitis closed, the first targets query finds
# nothing. Wait for the APU, up to 20 s.
set found 0
for {set i 0} {$i < 40} {incr i} {
  if {![catch {targets -set -nocase -filter {name =~ "APU*"}}]} {
    set found 1
    break
  }
  after 500
}
if {!$found} {
  error "program.tcl: no JTAG targets after 20 s -- is the board powered and its USB connected?\n[targets]"
}
puts "program: targets\n[targets]"

rst -system
after 3000

targets -set -filter {name =~ "xc7z020"}
fpga -file $bit

targets -set -nocase -filter {name =~ "APU*"}
loadhw -hw $xsa -mem-ranges {0x40000000 0xbfffffff}
configparams force-mem-access 1
source $init
ps7_init
ps7_post_config

targets -set -nocase -filter {name =~ "*A9*#0"}
rst -processor
dow $elf
con
configparams force-mem-access 0

puts "program: running"
disconnect
