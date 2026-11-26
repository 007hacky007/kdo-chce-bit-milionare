#!/usr/bin/env bash
# Generates packaging/licenses/rust-crates.html (the licences of the Rust crates
# built into the app) with the pinned cargo-about, from about.toml and about.hbs.
#
#   tools/release/gen-licenses.sh           regenerate the committed file
#   tools/release/gen-licenses.sh --check   fail when it is not up to date
#
# cargo-about comes from CARGO_ABOUT=<binary>, else
# tools/cache/cargo-about-<pin>/bin/cargo-about, else PATH; it must be the pinned
# version. Install it with:
#   cargo install cargo-about --locked --features cli --version <pin> \
#       --root tools/cache/cargo-about-<pin>
# Runs offline (--frozen): the crates of Cargo.lock must already be fetched
# (`cargo fetch --locked`), so licence texts come only from the crate sources.
set -euo pipefail

pin=0.9.3
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
out=packaging/licenses/rust-crates.html

about="${CARGO_ABOUT:-}"
if [ -z "$about" ]; then
    if [ -x "tools/cache/cargo-about-$pin/bin/cargo-about" ]; then
        about="tools/cache/cargo-about-$pin/bin/cargo-about"
    else
        about="$(command -v cargo-about || true)"
    fi
fi
[ -n "$about" ] || { echo "error: cargo-about $pin not found (see the header of $0)" >&2; exit 1; }
got="$("$about" --version | awk '{print $2}')"
[ "$got" = "$pin" ] || { echo "error: $about is cargo-about $got, pinned $pin" >&2; exit 1; }

check=0
case "${1:-}" in
    --check) check=1 ;;
    "") ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

tmp="$(mktemp "${TMPDIR:-/tmp}/rust-crates.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
"$about" generate --frozen -m apps/milionar/Cargo.toml -c about.toml about.hbs -o "$tmp" 2>"$tmp.log" \
    || { cat "$tmp.log" >&2; rm -f "$tmp.log"; exit 1; }
rm -f "$tmp.log"

if [ "$check" = 1 ]; then
    if ! cmp -s "$tmp" "$out"; then
        echo "error: $out is out of date; run tools/release/gen-licenses.sh" >&2
        diff -u "$out" "$tmp" | head -40 >&2 || true
        exit 1
    fi
    echo "$out is up to date"
else
    mkdir -p "$(dirname "$out")"
    mv "$tmp" "$out"
    chmod 644 "$out"
    echo "wrote $out"
fi
