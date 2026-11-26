#!/bin/sh
# Cross-build the Windows x64 desktop app on macOS with llvm-mingw (pinned in
# docs/build.md). Output: target/x86_64-pc-windows-gnullvm/release/milionar.exe
#
# Prerequisites:
#   rustup target add --toolchain 1.100 x86_64-pc-windows-gnullvm
#   llvm-mingw extracted into tools/cache/ (url and sha256 in docs/build.md), or
#   LLVM_MINGW=<dir> pointing at an extracted copy.
# Extra arguments are passed to cargo (e.g. --features test-hooks).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
mingw=${LLVM_MINGW:-$root/tools/cache/llvm-mingw-20261104-ucrt-macos-universal}
if [ ! -x "$mingw/bin/x86_64-w64-mingw32-clang" ]; then
    echo "llvm-mingw not found in $mingw (see docs/build.md)" >&2
    exit 1
fi
target=x86_64-pc-windows-gnullvm
t=X86_64_PC_WINDOWS_GNULLVM
export PATH="$mingw/bin:$PATH"
# The -gcc/-g++ names are llvm-mingw's clang wrappers; the cmake crate derives the resource
# compiler by replacing "gcc" with "windres" in the C compiler name, so -clang would make it
# use clang as RC.
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-gcc
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-g++
export AR_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-llvm-ar
export RC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-windres
eval "export CARGO_TARGET_${t}_LINKER=x86_64-w64-mingw32-clang"
# SDL is built by CMake (sdl3-src); the cmake crate sets CMAKE_SYSTEM_NAME=Windows for a
# cross build and takes the compilers from the CC_/CXX_ variables above.
cd "$root"
exec cargo build --release --target "$target" -p milionar "$@"
