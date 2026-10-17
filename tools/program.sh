#!/usr/bin/env bash
# Program the Arty Z7 and run the firmware from the command line: what
# Vitis's Run button does, without Vitis. Builds still happen in Vitis.
#
#   tools/program.sh
#
# XILINX_VITIS overrides the install location.
set -euo pipefail

VITIS=${XILINX_VITIS:-/tools/Xilinx/2026.1/Vitis}
REPO=$(cd "$(dirname "$0")/.." && pwd)

if [ ! -x "$VITIS/bin/xsdb" ]; then
  echo "program.sh: xsdb not found at $VITIS/bin/xsdb (set XILINX_VITIS)"
  exit 1
fi

# settings64.sh references variables it does not set; relax -u for it.
if [ -f "$VITIS/settings64.sh" ]; then
  set +u
  # shellcheck disable=SC1091
  . "$VITIS/settings64.sh"
  set -u
fi

exec "$VITIS/bin/xsdb" "$REPO/tools/program.tcl" "$REPO"
