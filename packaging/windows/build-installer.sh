#!/usr/bin/env bash
# Compiles the Windows installer dist/Milionar-<version>-windows-x64-setup.exe
# from the install tree made by build-tree.sh (checked again by the guard first)
# and packaging/windows/milionar.iss.
#
#   packaging/windows/build-installer.sh
#
# The Inno Setup 6 compiler (ISCC.exe) comes from one of:
#   ISCC=<path to ISCC.exe>   run it directly (Windows, e.g. Git Bash on CI);
#                             paths are converted with cygpath when it exists.
#   otherwise wine            installs the pinned Inno Setup
#                             (tools/upscalers/innosetup.lock.json) silently into
#                             MILIONAR_INNO_PREFIX (default target/package/wine-inno,
#                             never ~/.wine) and runs ISCC.exe there, every wine
#                             process with HOME=<prefix>-home.
# WINE=<wine binary> (default: wine). DIST=<dir> as for build-tree.sh.
# Writes <setup>.sha256 next to the installer.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
dist="${DIST:-$root/dist}"
tree="$dist/Milionar-windows-x64"
manifest="packaging/windows/stage.txt"
lock="tools/upscalers/innosetup.lock.json"

version="$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' apps/milionar/Cargo.toml)"
name="Milionar-$version-windows-x64-setup"

[ -d "$tree" ] || { echo "error: $tree missing; run packaging/windows/build-tree.sh" >&2; exit 1; }
python3 packaging/guard.py check --manifest "$manifest" --app "$tree"

work="$(mktemp -d "$dist/.installer.XXXXXX")"
trap 'rm -rf "$work"' EXIT
python3 packaging/icon/make-icon.py "$work/milionar.ico"
rm -f "$dist/$name.exe" "$dist/$name.exe.sha256"

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}
lockval() { python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["innosetup"][sys.argv[2]])' "$lock" "$1"; }

if [ -n "${ISCC:-}" ]; then
    if command -v cygpath >/dev/null 2>&1; then
        winpath() { cygpath -w "$1"; }
    else
        winpath() { printf '%s\n' "$1"; }
    fi
    # Git Bash (MSYS) would rewrite every argument that starts with / (the ISCC
    # switches /Qp /D /O /F) as a POSIX path, so conversion is off for ISCC only;
    # its paths are converted with winpath. Everything else (python3, sha256sum)
    # keeps MSYS's normal conversion of /c/... paths.
    run_iscc() { MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$ISCC" "$@"; }
else
    wine="${WINE:-wine}"
    command -v "$wine" >/dev/null 2>&1 || { echo "error: set ISCC or install wine" >&2; exit 1; }
    prefix="${MILIONAR_INNO_PREFIX:-$root/target/package/wine-inno}"
    mkdir -p "$prefix"
    prefix="$(cd "$prefix" && pwd -P)"
    home_real="$(cd "$HOME" && pwd -P)"
    case "$prefix" in
        "$home_real" | "$home_real/.wine") echo "error: refusing $prefix as the wine prefix" >&2; exit 1 ;;
    esac
    export WINEPREFIX="$prefix"
    # Every wine process runs with a scratch HOME beside the prefix and no XDG
    # overrides, so the links wineboot makes (Desktop, Documents...), wine's caches
    # and anything winemenubuilder would write stay out of the real home. The
    # overrides skip the Mono/Gecko prompts and winemenubuilder (.desktop files,
    # menus, icons for every shortcut and file type).
    wine_home="$prefix-home"
    mkdir -p "$wine_home"
    export WINEDEBUG=-all WINEDLLOVERRIDES="mscoree,mshtml=;winemenubuilder.exe=d"
    wine_run() {
        env -u XDG_CONFIG_HOME -u XDG_DATA_HOME -u XDG_CACHE_HOME -u XDG_DESKTOP_DIR \
            -u XDG_DOCUMENTS_DIR -u XDG_DOWNLOAD_DIR HOME="$wine_home" "$wine" "$@"
    }
    wineserver="$(dirname "$(command -v "$wine")")/wineserver"
    # shellcheck disable=SC2064 # expand now: the paths are fixed
    trap "[ -x '$wineserver' ] && HOME='$wine_home' '$wineserver' -k >/dev/null 2>&1; rm -rf '$work'" EXIT
    if [ ! -d "$WINEPREFIX/drive_c/users" ]; then
        wine_run wineboot -i >/dev/null 2>&1
    fi
    # Make the prefix's links to the (scratch) home plain folders inside the prefix.
    for link in "$WINEPREFIX"/drive_c/users/*/*; do
        if [ -L "$link" ]; then rm "$link" && mkdir "$link"; fi
    done
    # wine maps / to Z:.
    winpath() { printf 'Z:%s\n' "$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")" | tr / '\\'; }
    install_dir="$(lockval install_dir)"
    iscc_unix="$WINEPREFIX/drive_c/${install_dir#C:\\}/ISCC.exe"
    if [ ! -f "$iscc_unix" ]; then
        setup="$root/tools/cache/$(lockval name)"
        want="$(lockval sha256)"
        if [ ! -f "$setup" ] || [ "$(sha256 "$setup")" != "$want" ]; then
            mkdir -p "$root/tools/cache"
            curl -fsSL --retry 3 -o "$setup.part" "$(lockval url)"
            got="$(sha256 "$setup.part")"
            [ "$got" = "$want" ] || { echo "error: $setup.part has sha256 $got, pinned $want" >&2; exit 1; }
            [ "$(wc -c <"$setup.part" | tr -d ' ')" = "$(lockval bytes)" ] \
                || { echo "error: $setup.part has the wrong size" >&2; exit 1; }
            mv "$setup.part" "$setup"
        fi
        # shellcheck disable=SC2046 # the pinned arguments are split on purpose
        wine_run "$(winpath "$setup")" $(lockval install_args) >/dev/null 2>&1 || true
        [ -f "$iscc_unix" ] || { echo "error: Inno Setup did not install into $WINEPREFIX" >&2; exit 1; }
    fi
    run_iscc() { wine_run "$install_dir\\ISCC.exe" "$@"; }
fi

# ISCC prints the source and output paths; keep its log out of the terminal
# unless it fails.
if ! run_iscc /Qp "/DAppVersion=$version" "/DSourceDir=$(winpath "$tree")" \
    "/DIconFile=$(winpath "$work/milionar.ico")" "/O$(winpath "$dist")" "/F$name" \
    "$(winpath "$root/packaging/windows/milionar.iss")" >"$work/iscc.log" 2>&1; then
    grep -v '^\[mvk-' "$work/iscc.log" >&2 || true
    echo "error: ISCC failed" >&2
    exit 1
fi
[ -f "$dist/$name.exe" ] || { echo "error: ISCC wrote no $dist/$name.exe" >&2; exit 1; }
python3 packaging/guard.py scan "$dist/$name.exe"
(cd "$dist" && printf '%s  %s\n' "$(sha256 "$name.exe")" "$name.exe" > "$name.exe.sha256")
cat "$dist/$name.exe.sha256"
echo "built $dist/$name.exe"
