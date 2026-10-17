#!/usr/bin/env bash
# Build the platform and firmware from the command line: what the Vitis
# GUI's Update XSA -> Build platform -> Build app does. Then puts the
# bitstream where tools/program.tcl expects it and checks the outputs are
# fresh, so a stale platform cannot slip through the way it did on the
# first ACQ3 run.
#
#   tools/build_firmware.sh          # ~2 min
#
# XILINX_VITIS overrides the install; ARGUS_XSA the gateware export.
set -euo pipefail

VITIS=${XILINX_VITIS:-/tools/Xilinx/2026.1/Vitis}
REPO=$(cd "$(dirname "$0")/.." && pwd)
XSA=${ARGUS_XSA:-$(dirname "$REPO")/argus-neural-codec/argus_neural_codec.xsa}
BIT="$REPO/safety_controller/_ide/bitstream/argus_neural_codec.bit"
ELF="$REPO/safety_controller/build/safety_controller.elf"

[ -x "$VITIS/bin/vitis" ] || { echo "build_firmware: vitis not found at $VITIS/bin/vitis"; exit 1; }
[ -f "$XSA" ] || { echo "build_firmware: no XSA at $XSA"; exit 1; }

if [ -f "$VITIS/settings64.sh" ]; then
  set +u; . "$VITIS/settings64.sh"; set -u
fi

# The installed API reference, if present, names the method's real
# parameters; print them so a wrong guess costs one run, not several.
DOC="$VITIS/cli/api_docs/build/html/vitis.html"
if [ -f "$DOC" ]; then
  echo "build_firmware: API docs say:"
  grep -o 'update_hw([^)]*)' "$DOC" | sort -u | head -3 | sed 's/^/build_firmware:   /' || true
fi

t0=$(date +%s)
"$VITIS/bin/vitis" -s "$REPO/tools/build_firmware.py"

# The IDE keeps the bitstream it programs under _ide/bitstream; the CLI
# build does not refresh that copy, so take it from the XSA we just built
# against. An XSA is a zip; the .bit inside is named after the wrapper.
tmp=$(mktemp -d)
unzip -o -q -j "$XSA" '*.bit' -d "$tmp"
bit_in_xsa=$(ls "$tmp"/*.bit | head -1)
[ -n "$bit_in_xsa" ] || { echo "build_firmware: no .bit inside $XSA (exported without -include_bit?)"; exit 1; }
mkdir -p "$(dirname "$BIT")"
cp "$bit_in_xsa" "$BIT"
rm -rf "$tmp"

# Freshness: both outputs newer than the build started, and the bit newer
# than the XSA.
for f in "$BIT" "$ELF"; do
  [ -f "$f" ] || { echo "build_firmware: missing $f"; exit 1; }
  if [ "$(stat -c %Y "$f")" -lt "$t0" ]; then
    echo "build_firmware: $f was not rebuilt (older than this run)"; exit 1
  fi
done
echo "build_firmware: ok  bit $(date -r "$BIT" +%H:%M:%S)  elf $(date -r "$ELF" +%H:%M:%S)  ($(( $(date +%s) - t0 )) s)"
