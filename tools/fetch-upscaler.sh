#!/usr/bin/env bash
# Fetches the pinned Real-ESRGAN ncnn runner and models (macOS and Windows
# builds of release v0.2.5.0 / 20220424) into the gitignored tools/upscalers/.
# Pins: tools/upscalers/upscaler.lock.json (the upscale tests check that the
# values below match it). Safe to run again: verified files are kept.
#
#   tools/fetch-upscaler.sh [macos|windows]...    (default: both)
#
# UPSCALER_ZIP_DIR=<dir> copies an already downloaded zip from <dir> instead
# of downloading it, when its sha256 matches.
# Needs bash, curl, unzip and sha256sum or shasum.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
up="$root/tools/upscalers"
dl="$up/downloads"
base="https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0"

PARAM_SHA=d1a5755008791d09b57e3425fc9dd0bd26b00fdf79c606210bc0e693f8230881
BIN_SHA=548a36f9c3f4ab8da56cd3b13badf23968bee207b396dad14d04b830e5f2ab2d

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

has_sha() { [ -f "$1" ] && [ "$(sha256 "$1")" = "$2" ]; }

# fetch_zip <name> <sha256>
fetch_zip() {
    local name=$1 want=$2 target="$dl/$1"
    mkdir -p "$dl"
    if has_sha "$target" "$want"; then
        echo "ok      $name"
        return
    fi
    if [ -n "${UPSCALER_ZIP_DIR:-}" ] && has_sha "$UPSCALER_ZIP_DIR/$name" "$want"; then
        cp "$UPSCALER_ZIP_DIR/$name" "$target.part"
        echo "copied  $name"
    else
        echo "fetch   $base/$name"
        curl -fL --retry 3 -o "$target.part" "$base/$name"
    fi
    if ! has_sha "$target.part" "$want"; then
        rm -f "$target.part"
        echo "error: $name does not match the pinned sha256 $want" >&2
        exit 1
    fi
    mv -f "$target.part" "$target"
}

# check_dir <dir> <exe> <exe sha256> [<extra file> <sha256>]...
check_dir() {
    local dir=$1 exe=$2 sha=$3
    shift 3
    has_sha "$dir/$exe" "$sha" || return 1
    has_sha "$dir/models/realesr-animevideov3-x3.param" "$PARAM_SHA" || return 1
    has_sha "$dir/models/realesr-animevideov3-x3.bin" "$BIN_SHA" || return 1
    while [ $# -gt 0 ]; do
        has_sha "$dir/$1" "$2" || return 1
        shift 2
    done
}

# install <zip> <zip sha256> <dir> <exe> <exe sha256> [<extra> <sha256>]...
install() {
    local zip=$1 zsha=$2 dir="$up/$3" exe=$4 esha=$5
    shift 5
    if check_dir "$dir" "$exe" "$esha" "$@"; then
        chmod +x "$dir/$exe"
        echo "ok      ${dir#"$root"/}"
        return
    fi
    fetch_zip "$zip" "$zsha"
    rm -rf "$dir.tmp"
    unzip -q "$dl/$zip" -d "$dir.tmp"
    # The zips do not keep the executable bit.
    chmod +x "$dir.tmp/$exe"
    if ! check_dir "$dir.tmp" "$exe" "$esha" "$@"; then
        rm -rf "$dir.tmp"
        echo "error: files in $zip do not match the pins" >&2
        exit 1
    fi
    rm -rf "$dir"
    mv "$dir.tmp" "$dir"
    echo "install ${dir#"$root"/}"
}

macos() {
    install realesrgan-ncnn-vulkan-20220424-macos.zip \
        e0ad05580abfeb25f8d8fb55aaf7bedf552c375b5b4d9bd3c8d59764d2cc333a \
        realesrgan-20220424 realesrgan-ncnn-vulkan \
        c1c35d92079085de96b9d547fd7e4464bc8a2e9ccf28d7b8c712d72ade91b7cc
}

windows() {
    install realesrgan-ncnn-vulkan-20220424-windows.zip \
        abc02804e17982a3be33675e4d471e91ea374e65b70167abc09e31acb412802d \
        realesrgan-20220424-windows realesrgan-ncnn-vulkan.exe \
        07e49f7cbb4ede01ae4dd4c399d3a7e5846e3d2085c3128eff881e55cb7b1a0c \
        vcomp140.dll 8f72ef2e483465444b2059fc6744d6cb22cd8d8a27f6fa56befd2a42dcd0f78b
}

if [ $# -eq 0 ]; then
    set -- macos windows
fi
for os in "$@"; do
    case "$os" in
        macos) macos ;;
        windows) windows ;;
        *)
            echo "usage: $0 [macos|windows]..." >&2
            exit 2
            ;;
    esac
done
