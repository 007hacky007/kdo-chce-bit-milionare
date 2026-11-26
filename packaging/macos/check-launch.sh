#!/usr/bin/env bash
# Checks that the packaged app starts into the setup wizard with an empty
# data dir, in two steps:
#   1. release: dist/Milionar.app as built (no test hooks) is started through
#      LaunchServices (`open -n`), must stay up for a few seconds, log the
#      wizard start and go away on SIGTERM; then the same binary is run as a
#      child and must exit with status 0 on SIGTERM (SDL turns it into a quit
#      event);
#   2. test-hooks: a test-hooks variant is built into dist/check/ (the release
#      app is left alone), run with the normal video driver, and its window is
#      captured into SHOT_DIR.
#
#   packaging/macos/check-launch.sh SHOT_DIR [EMPTY_DATA_DIR]
#
# EMPTY_DATA_DIR must not exist or be empty (default: a new temporary dir);
# the steps use its subdirectories release/, direct/ and hooks/.
# MILIONAR_LANG=cs|en picks the wizard language of step 2 (default: the system's).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
[ $# -ge 1 ] || { echo "usage: $0 SHOT_DIR [EMPTY_DATA_DIR]" >&2; exit 2; }
shots="$1"
data="${2:-$(mktemp -d)}"
mkdir -p "$shots" "$data"
if [ -n "$(ls -A "$data")" ]; then
    echo "error: $data is not empty" >&2
    exit 1
fi
data="$(cd "$data" && pwd -P)"
fail() { echo "check-launch: FAIL: $*" >&2; exit 1; }

check_wizard_log() {
    grep -q "setup: no imported game data" "$1/milionar.log" || fail "no setup line in $1/milionar.log"
    grep -q "setup: page Welcome" "$1/milionar.log" || fail "no Welcome page in $1/milionar.log"
    [ ! -e "$1/game" ] || fail "a game/ dir appeared in the empty data dir $1"
}

# 1. The release app through LaunchServices.
app="$root/dist/Milionar.app"
[ -d "$app" ] || fail "$app missing; run packaging/macos/build-app.sh"
rel="$data/release"
mkdir "$rel"
exe="$app/Contents/MacOS/milionar"
open -n -a "$app" --args --data-dir "$rel" --windowed
pid=""
for _ in $(seq 50); do
    pid="$(pgrep -f -- "$exe --data-dir $rel" | head -1 || true)"
    [ -n "$pid" ] && break
    sleep 0.2
done
[ -n "$pid" ] || fail "the release app did not start"
sleep 5
kill -0 "$pid" 2>/dev/null || fail "the release app exited within 5 s"
check_wizard_log "$rel"
kill -TERM "$pid"
for _ in $(seq 50); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.2
done
if kill -0 "$pid" 2>/dev/null; then
    kill -KILL "$pid"
    fail "the release app did not quit on SIGTERM"
fi
echo "check-launch: release app via open OK (stayed up, wizard logged, gone after SIGTERM)"

# 1b. The same binary as a child, to see that SIGTERM ends it cleanly (exit 0,
# not killed by the signal).
direct="$data/direct"
mkdir "$direct"
"$exe" --data-dir "$direct" --windowed &
pid=$!
sleep 5
kill -0 "$pid" 2>/dev/null || fail "the release app (direct) exited within 5 s"
check_wizard_log "$direct"
kill -TERM "$pid"
( sleep 15; kill -KILL "$pid" 2>/dev/null ) &
watchdog=$!
status=0
wait "$pid" || status=$?
kill "$watchdog" 2>/dev/null || true
[ "$status" = 0 ] || fail "the release app exited with status $status after SIGTERM"
echo "check-launch: release app direct OK (exit 0 after SIGTERM)"

# 2. The test-hooks variant with a window capture.
DIST="$root/dist/check" "$root/packaging/macos/build-app.sh" --features test-hooks
happ="$root/dist/check/Milionar.app"
hooks="$data/hooks"
mkdir "$hooks"

unset SDL_VIDEODRIVER
export MILIONAR_CAPTURE_DIR="$shots"
export MILIONAR_CAPTURE_MS=1000
export MILIONAR_SCRIPT="6000:quit"
"$happ/Contents/MacOS/milionar" --data-dir "$hooks" --windowed &
pid=$!
( sleep 30; kill "$pid" 2>/dev/null ) &
watchdog=$!
status=0
wait "$pid" || status=$?
kill "$watchdog" 2>/dev/null || true

[ "$status" = 0 ] || fail "the test-hooks app exited with status $status"
check_wizard_log "$hooks"
ls "$shots"/*.png >/dev/null 2>&1 || fail "no window capture in $shots"
echo "check-launch: OK (wizard shown; captures in $shots, data dirs under $data)"
