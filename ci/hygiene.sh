#!/usr/bin/env bash
# Repo hygiene checks for argus_safety_controller.
set -uo pipefail

fails=0

note() { printf '\n--- %s\n' "$1"; }
bad()  { printf 'FAIL %s\n' "$1"; fails=$((fails+1)); }

note "build artifacts must not be tracked"
if git ls-files | grep -Ei '\.(o|d|elf|bin|bit|xsa|hwh|log|jou|str)$|(^|/)(Debug|Release|_ide|\.metadata|target)/'; then
    bad "generated Vitis, Vivado or cargo output is committed"
fi

note "no absolute host paths in tracked sources"
if git ls-files '*.c' '*.h' '*.rs' '*.ld' '*.x' '*.tcl' '*.toml' 'Makefile' \
   | xargs -r grep -nE '(/home/[a-z]|C:\\\\|/tools/Xilinx|/opt/Xilinx)' ; then
    bad "absolute toolchain or home paths leak into the repo"
fi

# note "no CRLF line endings"
# if git ls-files '*.c' '*.h' '*.sh' '*.yml' '*.yaml' '*.md' \
#    | xargs -r grep -lU $'\r' ; then
#     bad "CRLF found"
# fi

# note "no trailing whitespace"
# if git ls-files '*.c' '*.h' '*.sh' '*.yml' '*.yaml' \
#    | xargs -r grep -nE ' +$' ; then
#     bad "trailing whitespace found"
# fi

note "no file over 1 MiB"
while read -r f; do
    [ -f "$f" ] || continue
    sz=$(wc -c < "$f")
    if [ "$sz" -gt 1048576 ]; then
        printf '%s (%s bytes)\n' "$f" "$sz"
        bad "oversized file tracked"
    fi
done < <(git ls-files)

# The two C headers left in the tree are the wire contract (argus_wire.h,
# which gen-wire.sh turns into argus-wire's structs) and the replay client
# (argus_replay_client.h, the oracle argus-difftest runs beside the Rust).
note "the C contract headers are present where the Rust build expects them"
for h in argus_wire.h argus_replay_client.h; do
    [ -f "safety_controller/src/$h" ] || bad "safety_controller/src/$h missing"
done

if [ "$fails" -ne 0 ]; then
    printf '\n%d hygiene check(s) failed\n' "$fails"
    exit 1
fi
printf '\nhygiene clean\n'
