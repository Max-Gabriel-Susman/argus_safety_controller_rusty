#!/usr/bin/env bash
# Repo hygiene checks for argus_safety_controller.
# Vendored trees (lwIP) are exempt from style checks but not from artifact checks.
set -uo pipefail

VENDOR_RE='^(src/lwip/|third_party/)'
fails=0

note() { printf '\n--- %s\n' "$1"; }
bad()  { printf 'FAIL %s\n' "$1"; fails=$((fails+1)); }

first_party() { git ls-files "$@" | grep -Ev "$VENDOR_RE"; }

note "build artifacts must not be tracked"
if git ls-files | grep -Ei '\.(o|d|elf|bin|bit|xsa|hwh|log|jou|str)$|(^|/)(Debug|Release|_ide|\.metadata)/'; then
    bad "generated Vitis/Vivado output is committed"
fi

note "no absolute host paths in tracked sources"
if first_party '*.c' '*.h' '*.ld' '*.tcl' '*.mk' 'Makefile' \
   | xargs -r grep -nE '(/home/[a-z]|C:\\\\|/tools/Xilinx|/opt/Xilinx)' ; then
    bad "absolute toolchain or home paths leak into the repo"
fi

# note "no CRLF line endings"
# if first_party '*.c' '*.h' '*.sh' '*.yml' '*.yaml' '*.md' \
#    | xargs -r grep -lU $'\r' ; then
#     bad "CRLF found"
# fi

# note "no trailing whitespace"
# if first_party '*.c' '*.h' '*.sh' '*.yml' '*.yaml' \
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

note "vendored wire header is present where the build expects it"
[ -f safety_controller/src/argus_wire.h ] || bad "safety_controller/src/argus_wire.h missing"

if [ "$fails" -ne 0 ]; then
    printf '\n%d hygiene check(s) failed\n' "$fails"
    exit 1
fi
printf '\nhygiene clean\n'