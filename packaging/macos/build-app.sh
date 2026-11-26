#!/usr/bin/env bash
# Builds dist/Milionar.app (macOS arm64, ad-hoc signed) from the files listed
# in packaging/macos/stage.txt. Run from anywhere; paths are taken relative to
# the repository root.
#
#   packaging/macos/build-app.sh [extra cargo args, e.g. --features test-hooks]
#
# DIST=<dir> puts the app somewhere else (default: dist/ in the repository).
# Needs cargo (rustup toolchain from rust-toolchain.toml), cmake, python3 and
# the Xcode command line tools (codesign, iconutil, sips, vtool, plutil, lipo).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
dist="${DIST:-$root/dist}"
app="$dist/Milionar.app"
manifest="packaging/macos/stage.txt"
target=aarch64-apple-darwin
target_dir="$root/target/package"
runner_dir="tools/upscalers/realesrgan-20220424"
runner="$runner_dir/realesrgan-ncnn-vulkan"

# Pinned runner and model (fetched once; the guard checks their hashes).
for f in "$runner" "$runner_dir/models/realesr-animevideov3-x3.param" \
    "$runner_dir/models/realesr-animevideov3-x3.bin"; do
    if [ ! -f "$f" ]; then
        tools/fetch-upscaler.sh macos
        break
    fi
done

version="$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' apps/milionar/Cargo.toml)"

# Release build. The deployment target is set for both rustc and the CMake
# build of SDL. Path remapping keeps the build machine's home and checkout
# paths out of the binary (panic locations, C __FILE__); the guard checks it.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
export MACOSX_DEPLOYMENT_TARGET=11.0
sep=$'\x1f'
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=/home${sep}--remap-path-prefix=$cargo_home=/cargo${sep}--remap-path-prefix=$root=/src"
cmap="-ffile-prefix-map=$HOME=/home -ffile-prefix-map=$cargo_home=/cargo -ffile-prefix-map=$root=/src"
export CFLAGS_aarch64_apple_darwin="$cmap"
export CXXFLAGS_aarch64_apple_darwin="$cmap"
cargo build --release --locked -p milionar --target "$target" --target-dir "$target_dir" "$@"
bin="$target_dir/$target/release/milionar"

archs="$(lipo -archs "$bin")"
[ "$archs" = arm64 ] || { echo "error: $bin is $archs, expected arm64" >&2; exit 1; }
# SDL is linked statically: only system libraries may be loaded.
if otool -L "$bin" | tail -n +2 | awk '{print $1}' | grep -Ev '^(/usr/lib/|/System/Library/)'; then
    echo "error: the binary links non-system libraries (above)" >&2
    exit 1
fi

minos() { vtool -arch arm64 -show-build "$1" | awk '/minos/ {print $2; exit}'; }
min_app="$(minos "$bin")"
min_runner="$(minos "$runner")"
min_macos="$(printf '%s\n%s\n' "$min_app" "$min_runner" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)"
echo "minimum macOS: $min_macos (app $min_app, runner $min_runner)"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
python3 packaging/guard.py stage --manifest "$manifest" --app "$app"
cp "$bin" "$app/Contents/MacOS/milionar"
chmod 755 "$app/Contents/MacOS/milionar"
sed -e "s/@VERSION@/$version/g" -e "s/@MIN_MACOS@/$min_macos/g" \
    packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Icon: drawn by our generator, scaled to the iconset sizes, packed by iconutil.
work="$(mktemp -d "$dist/.icon.XXXXXX")"
trap 'rm -rf "$work"' EXIT
python3 packaging/icon/make-icon.py "$work/icon-1024.png"
iconset="$work/milionar.iconset"
mkdir "$iconset"
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$work/icon-1024.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    if [ "$d" = 1024 ]; then
        cp "$work/icon-1024.png" "$iconset/icon_${s}x${s}@2x.png"
    else
        sips -z "$d" "$d" "$work/icon-1024.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
    fi
done
iconutil -c icns -o "$app/Contents/Resources/milionar.icns" "$iconset"

# No extended attributes may ship (download URLs, quarantine, Finder info);
# the guard allows only com.apple.provenance, which the system keeps.
xattr -cr "$app"
python3 packaging/guard.py check --manifest "$manifest" --app "$app" --unsigned

# Signing, inner code first. The runner keeps its upstream ad-hoc (linker)
# signature: re-signing would change its bytes and break the SHA-256 pin that
# the app checks before running it. Its arm64 slice must verify as it is (the
# x86_64 slice of the universal upstream file is unsigned and never runs here).
codesign --verify --strict --arch arm64 --verbose=1 "$app/Contents/Resources/upscaler/realesrgan-ncnn-vulkan"
codesign --force --sign - --identifier io.github.007hacky007.milionar --timestamp=none "$app"
codesign --verify --deep --strict --verbose=2 "$app"

python3 packaging/guard.py check --manifest "$manifest" --app "$app"
echo "built $app ($version, macOS $min_macos+)"
