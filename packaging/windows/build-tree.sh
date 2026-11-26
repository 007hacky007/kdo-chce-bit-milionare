#!/usr/bin/env bash
# Builds the Windows x64 install tree dist/Milionar-windows-x64/ from the files
# listed in packaging/windows/stage.txt, cross-compiled on macOS with llvm-mingw
# (tools/build-windows.sh, target x86_64-pc-windows-gnullvm). The installer
# (packaging/windows/build-installer.sh) packs this tree unchanged.
#
#   packaging/windows/build-tree.sh [extra cargo args, e.g. --features test-hooks]
#
# DIST=<dir> puts the tree somewhere else (default: dist/ in the repository).
# Writes <dist>/Milionar-<version>-windows-x64.tree.sha256 (every file of the tree).
# Needs what tools/build-windows.sh needs (llvm-mingw, the gnullvm rustup target),
# CMake and python3.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
dist="${DIST:-$root/dist}"
tree="$dist/Milionar-windows-x64"
manifest="packaging/windows/stage.txt"
target=x86_64-pc-windows-gnullvm
target_dir="$root/target/package"
mingw="${LLVM_MINGW:-$root/tools/cache/llvm-mingw-20261104-ucrt-macos-universal}"
objdump="$mingw/bin/llvm-objdump"
readobj="$mingw/bin/llvm-readobj"
runner_dir="tools/upscalers/realesrgan-20220424-windows"
runner="$runner_dir/realesrgan-ncnn-vulkan.exe"

# Pinned runner, vcomp140.dll and model (fetched once; the guard checks their hashes).
for f in "$runner" "$runner_dir/vcomp140.dll" "$runner_dir/models/realesr-animevideov3-x3.param" \
    "$runner_dir/models/realesr-animevideov3-x3.bin"; do
    if [ ! -f "$f" ]; then
        tools/fetch-upscaler.sh windows
        break
    fi
done

version="$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' apps/milionar/Cargo.toml)"

# Icon, drawn by our generator; build.rs embeds it as the first icon group.
mkdir -p "$target_dir"
icon="$target_dir/milionar.ico"
python3 packaging/icon/make-icon.py "$target_dir/milionar.new.ico"
if cmp -s "$target_dir/milionar.new.ico" "$icon"; then
    rm "$target_dir/milionar.new.ico"
else
    mv "$target_dir/milionar.new.ico" "$icon"
fi
export MILIONAR_WINDOWS_ICON="$icon"

# Release build with path remapping: the build machine's home and checkout paths
# stay out of the binary (Rust panic locations and the C __FILE__ of SDL's CMake
# build); the guard checks it.
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
# The gnullvm standard library comes from rustup: use the toolchain of
# rust-toolchain.toml even when another cargo (e.g. Homebrew's) is first in PATH.
if command -v rustup >/dev/null 2>&1; then
    PATH="$(dirname "$(rustup which cargo)"):$PATH"
    export PATH
fi
sep=$'\x1f'
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME=/home${sep}--remap-path-prefix=$cargo_home=/cargo${sep}--remap-path-prefix=$root=/src"
cmap="-ffile-prefix-map=$HOME=/home -ffile-prefix-map=$cargo_home=/cargo -ffile-prefix-map=$root=/src"
export CFLAGS_x86_64_pc_windows_gnullvm="$cmap"
export CXXFLAGS_x86_64_pc_windows_gnullvm="$cmap"
LLVM_MINGW="$mingw" tools/build-windows.sh --locked --target-dir "$target_dir" "$@"
bin="$target_dir/$target/release/milionar.exe"

# x86-64 GUI executable that imports only DLLs of Windows itself (SDL, libunwind
# and the mingw-w64 runtime are linked statically; the UCRT is part of Windows 10+).
"$objdump" -f "$bin" | grep -q 'file format coff-x86-64' \
    || { echo "error: $bin is not an x86-64 PE" >&2; exit 1; }
"$objdump" -p "$bin" | grep -q 'Subsystem.*0002' \
    || { echo "error: $bin is not a GUI-subsystem executable" >&2; exit 1; }
system_dlls='^(kernel32|user32|gdi32|advapi32|shell32|ole32|oleaut32|combase|setupapi|winmm|imm32|version|bcryptprimitives|ntdll|api-ms-win-[a-z0-9-]+)\.dll$'
if "$objdump" -p "$bin" | awk '/DLL Name:/ {print tolower($3)}' | grep -Ev "$system_dlls"; then
    echo "error: $bin imports DLLs that are not part of Windows (above)" >&2
    exit 1
fi
# The runner needs vcomp140.dll (shipped beside it) and vulkan-1.dll (GPU driver).
if "$objdump" -p "$runner" | awk '/DLL Name:/ {print tolower($3)}' \
    | grep -Ev '^(kernel32|ole32|oleaut32|vcomp140|vulkan-1)\.dll$'; then
    echo "error: the runner imports unexpected DLLs (above)" >&2
    exit 1
fi
# Manifest (RT_MANIFEST), version (RT_VERSION) and icon (RT_GROUP_ICON) resources.
resources="$("$readobj" --coff-resources "$bin")"
for t in 'MANIFEST (ID 24)' 'VERSIONINFO (ID 16)' 'GROUP_ICON (ID 14)'; do
    grep -qF "$t" <<<"$resources" || { echo "error: $bin has no $t resource" >&2; exit 1; }
done

rm -rf "$tree"
mkdir -p "$tree"
python3 packaging/guard.py stage --manifest "$manifest" --app "$tree"
cp "$bin" "$tree/milionar.exe"
chmod 755 "$tree/milionar.exe"
# Files copied on macOS may carry quarantine or download xattrs; none may ship.
if command -v xattr >/dev/null 2>&1; then xattr -cr "$tree"; fi
python3 packaging/guard.py check --manifest "$manifest" --app "$tree"
python3 packaging/guard.py hashes --root "$tree" --out "$dist/Milionar-$version-windows-x64.tree.sha256"
echo "built $tree ($version, Windows x64)"
