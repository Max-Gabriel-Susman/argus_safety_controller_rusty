#!/usr/bin/env bash
# gen-wire.sh: generate argus-wire/src/generated.rs from argus_wire.h.
#
#   bash rust/tools/gen-wire.sh           regenerate the file
#   bash rust/tools/gen-wire.sh --check   fail if the committed file is stale
#
# The input is safety_controller/src/argus_wire.h, the copy the C firmware
# builds against. The vendor-drift CI job holds that copy equal to
# argus_core's, and --check holds generated.rs equal to it, so every link
# from argus_core to the Rust structs has a check.
set -euo pipefail

# Different bindgen versions word their output differently, which would make
# --check fail for reasons that have nothing to do with the header.
BINDGEN_VERSION=0.73.2

rust_dir=$(cd "$(dirname "$0")/.." && pwd)
header="$rust_dir/../safety_controller/src/argus_wire.h"
out="$rust_dir/argus-wire/src/generated.rs"

# From inside rust/, rustup picks the toolchain pinned in rust-toolchain.toml,
# so rustfmt below is the same one `cargo fmt` uses.
cd "$rust_dir"

if [ "$(bindgen --version 2>/dev/null)" != "bindgen $BINDGEN_VERSION" ]; then
    echo "gen-wire: needs bindgen $BINDGEN_VERSION. Install it with:" >&2
    echo "  cargo install bindgen-cli --locked --version $BINDGEN_VERSION" >&2
    exit 1
fi

# The zerocopy derives let argus-wire read these structs out of received
# bytes, and write them into bytes to send, without unsafe code.
derives=zerocopy::FromBytes,zerocopy::IntoBytes,zerocopy::KnownLayout
derives=$derives,zerocopy::Immutable,zerocopy::Unaligned

# Always generate into a scratch file first. If clang rejects the header (one
# of its static_asserts fails, say), bindgen exits with an error, and the
# committed generated.rs must not be left half written.
fresh=$(mktemp --suffix=.rs)
trap 'rm -f "$fresh"' EXIT

# Everything after `--` goes to clang. -fparse-all-comments turns the
# header's /* field comments */ into doc comments on the Rust fields.
# --rust-target 1.85 is the oldest Rust that knows edition 2024, which
# --rust-edition asks for to match the workspace.
bindgen "$header" \
    --use-core \
    --rust-target 1.85 \
    --rust-edition 2024 \
    --allowlist-type 'argus_.*_t' \
    --allowlist-var 'ARGUS_.*' \
    --with-derive-custom-struct "argus_.*=$derives" \
    --raw-line '// Generated from argus_wire.h by rust/tools/gen-wire.sh. Do not edit.' \
    --formatter none \
    --output "$fresh" \
    -- -std=c11 -fparse-all-comments

# Formatted exactly as `cargo fmt` would, so the two never disagree.
rustfmt --edition 2024 "$fresh"

if [ "${1:-}" = "--check" ]; then
    if ! diff -u "$out" "$fresh"; then
        echo "gen-wire: generated.rs is stale. Run bash rust/tools/gen-wire.sh and commit it." >&2
        exit 1
    fi
    echo "gen-wire: generated.rs matches argus_wire.h"
else
    # install, not cp: mktemp made the scratch file private (0600), and the
    # source file should get ordinary permissions (0644).
    install -m 644 "$fresh" "$out"
    echo "gen-wire: wrote argus-wire/src/generated.rs"
fi
