#!/usr/bin/env bash
# Program the Arty Z7 and run the firmware from the command line: what
# Vitis's Run button does, without Vitis.
#
#   tools/program.sh
#   ARGUS_ELF=path/to/app.elf tools/program.sh
#
# What gets loaded, each part overridable:
#
#   ARGUS_ELF     the ELF to run. Default: the Rust firmware's release build,
#                 rust/target/armv7a-none-eabihf/release/argus-fw, when this
#                 checkout has one; otherwise the Vitis app's build output.
#   ARGUS_XSA     an exported XSA: the bitstream and ps7_init.tcl are taken
#                 out of it, so no Vitis platform build is needed.
#   ARGUS_HW_DIR  a checkout whose Vitis platform build supplies the
#                 bitstream, the XSA and ps7_init.tcl.
#
# With neither ARGUS_XSA nor ARGUS_HW_DIR, the hardware comes from the first
# of these that exists: this checkout's Vitis platform build, then
# ~/Documents/argus_safety_controller's (so the Rust firmware runs on the
# fabric and clock setup the C firmware was last built for), then the
# gateware's own export, ~/Documents/argus-neural-codec/argus_neural_codec.xsa.
#
# XILINX_VITIS overrides the install location.
set -euo pipefail

VITIS=${XILINX_VITIS:-/tools/Xilinx/2026.1/Vitis}
REPO=$(cd "$(dirname "$0")/.." && pwd)

if [ ! -x "$VITIS/bin/xsdb" ]; then
  echo "program.sh: xsdb not found at $VITIS/bin/xsdb (set XILINX_VITIS)"
  exit 1
fi

# The Vitis build's own files, relative to a checkout.
VITIS_BIT=safety_controller/_ide/bitstream/argus_neural_codec.bit
VITIS_XSA=arty_z7_platform/export/arty_z7_platform/hw/argus_neural_codec.xsa
VITIS_INIT=safety_controller/_ide/psinit/ps7_init.tcl
VITIS_ELF=safety_controller/build/safety_controller.elf
RUST_ELF=rust/target/armv7a-none-eabihf/release/argus-fw
C_CHECKOUT=$HOME/Documents/argus_safety_controller
CODEC_XSA=$HOME/Documents/argus-neural-codec/argus_neural_codec.xsa

# --- the ELF ----------------------------------------------------------------
if [ -n "${ARGUS_ELF:-}" ]; then
  elf=$ARGUS_ELF
elif [ -f "$REPO/$RUST_ELF" ]; then
  elf=$REPO/$RUST_ELF
else
  elf=$REPO/$VITIS_ELF
fi

# --- bitstream, hardware description, PS init ---------------------------------
from_xsa() {
  xsa=$1
  [ -f "$xsa" ] || { echo "program.sh: missing $xsa"; exit 1; }
  local out="${XDG_CACHE_HOME:-$HOME/.cache}/argus-program"
  rm -rf "$out" && mkdir -p "$out"
  # An XSA is a zip; take the bitstream and ps7_init.tcl out of it.
  python3 - "$xsa" "$out" <<'EOF'
import sys, zipfile, os
xsa, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(xsa) as z:
    names = [n for n in z.namelist()
             if n.endswith('.bit') or os.path.basename(n) == 'ps7_init.tcl']
    for n in names:
        with z.open(n) as src, open(os.path.join(out, os.path.basename(n)), 'wb') as dst:
            dst.write(src.read())
EOF
  bit=$(find "$out" -maxdepth 1 -name '*.bit' | head -1)
  init=$out/ps7_init.tcl
  [ -n "$bit" ] || { echo "program.sh: missing a .bit inside $xsa (export it with the bitstream)"; exit 1; }
  echo "program: hardware from $xsa"
}
from_checkout() {
  bit=$1/$VITIS_BIT
  xsa=$1/$VITIS_XSA
  init=$1/$VITIS_INIT
  echo "program: hardware from $1"
}

if [ -n "${ARGUS_XSA:-}" ]; then
  from_xsa "$ARGUS_XSA"
elif [ -n "${ARGUS_HW_DIR:-}" ]; then
  from_checkout "${ARGUS_HW_DIR%/}"
elif [ -f "$REPO/$VITIS_BIT" ]; then
  from_checkout "$REPO"
elif [ -f "$C_CHECKOUT/$VITIS_BIT" ]; then
  from_checkout "$C_CHECKOUT"
elif [ -f "$CODEC_XSA" ]; then
  from_xsa "$CODEC_XSA"
else
  from_checkout "$C_CHECKOUT"   # program.tcl names the file that is missing
fi

# settings64.sh references variables it does not set; relax -u for it.
if [ -f "$VITIS/settings64.sh" ]; then
  set +u
  # shellcheck disable=SC1091
  . "$VITIS/settings64.sh"
  set -u
fi

exec "$VITIS/bin/xsdb" "$REPO/tools/program.tcl" "$bit" "$xsa" "$init" "$elf"
