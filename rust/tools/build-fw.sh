#!/usr/bin/env bash
# build-fw.sh: the release build of the firmware for the Cortex-A9.
#
#   bash rust/tools/build-fw.sh
#
# Works from any directory. The last line names the ELF and its size, so a
# caller (tools/program.sh, or a person) can take the path from it.
#
# The target is given here rather than in .cargo/config.toml, which only
# holds the A9's compiler and linker flags: a default target there would make
# the host crates' `cargo test` build for the A9 as well.
set -euo pipefail

TARGET=armv7a-none-eabihf

rust_dir=$(cd "$(dirname "$0")/.." && pwd)

# From inside rust/, rustup picks the toolchain pinned in rust-toolchain.toml
# and cargo finds the workspace and its .cargo/config.toml.
cd "$rust_dir"

# --locked: build exactly the dependency versions in the committed Cargo.lock.
cargo build --locked --release --target "$TARGET" -p argus-fw

elf="$rust_dir/target/$TARGET/release/argus-fw"
[ -f "$elf" ] || { echo "build-fw: cargo succeeded but $elf is missing" >&2; exit 1; }

# The ELF's size on disk includes debug info (the release profile keeps it for
# the debugger), so the bytes that are downloaded to DDR are shown as well.
loaded=$(size -A "$elf" | awk '$1 ~ /^\.(vector_table|text|rodata|data)$/ { n += $2 } END { print n + 0 }')
echo "build-fw: $elf ($(stat -c %s "$elf") bytes, $loaded loaded)"
