#!/usr/bin/env bash
# Packs dist/Milionar.app (made by build-app.sh) into
# dist/Milionar-<version>-macos-arm64.dmg and writes next to it:
#   <dmg>.sha256      sha256sum line of the DMG
#   <app>.sha256      sha256sum list of every file of the app inside the DMG
# The DMG holds the app and an Applications symlink (made here, outside the
# guarded app tree). The copy that goes into the image is guarded, and the
# finished image is mounted read-only and guarded again; the file list is
# taken from the mounted image. DIST=<dir> as for build-app.sh.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
dist="${DIST:-$root/dist}"
app="$dist/Milionar.app"
manifest="packaging/macos/stage.txt"

guard() {
    python3 packaging/guard.py check --manifest "$manifest" --app "$1"
    codesign --verify --deep --strict "$1"
}

[ -d "$app" ] || { echo "error: $app missing; run packaging/macos/build-app.sh" >&2; exit 1; }
guard "$app"

version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")"
name="Milionar-$version-macos-arm64"
dmg="$dist/$name.dmg"

work="$(mktemp -d "$dist/.dmg.XXXXXX")"
mnt="$work/mnt"
src="$work/src"
cleanup() {
    if [ -d "$mnt" ] && mount | grep -q " on $mnt "; then
        hdiutil detach "$mnt" -quiet || hdiutil detach "$mnt" -force -quiet || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT
mkdir "$src" "$mnt"
ditto --noextattr --norsrc --noqtn "$app" "$src/Milionar.app"
guard "$src/Milionar.app"
ln -s /Applications "$src/Applications"

rm -f "$dmg"
hdiutil create -volname "Milionar $version" -srcfolder "$src" -fs HFS+ \
    -format UDZO -imagekey zlib-level=9 -ov "$dmg" >/dev/null
hdiutil verify "$dmg" >/dev/null

# Check what was actually imaged.
hdiutil attach -readonly -nobrowse -noautoopen -mountpoint "$mnt" "$dmg" >/dev/null
top="$(cd "$mnt" && find . -mindepth 1 -maxdepth 1 ! -name .fseventsd | sort | tr '\n' ' ')"
if [ "$top" != "./Applications ./Milionar.app " ] || [ "$(readlink "$mnt/Applications")" != /Applications ]; then
    echo "error: unexpected DMG contents: $top" >&2
    exit 1
fi
guard "$mnt/Milionar.app"
python3 packaging/guard.py hashes --root "$mnt/Milionar.app" --out "$dist/$name.app.sha256"
hdiutil detach "$mnt" -quiet

(cd "$dist" && shasum -a 256 "$name.dmg" > "$name.dmg.sha256")
echo "built $dmg"
cat "$dist/$name.dmg.sha256"
