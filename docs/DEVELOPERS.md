# For developers

How to build and run the port from source, and the command-line tool that imports, checks
and upscales the game data. The player guide is [install.md](install.md).

Related documents:

- [build.md](build.md): building the app and the release packages for macOS and Windows,
  the pinned toolchains and the release workflow.
- [../packaging/README.md](../packaging/README.md): the packaging scripts, the guard that
  checks every package, the shipped licences and the installer's behaviour.
- [upscale-pack.md](upscale-pack.md): the format of the upscaled art pack.
- [original-bugs.md](original-bugs.md): bugs of the original game and how the port
  handles them.
- [../RELEASE-NOTES.md](../RELEASE-NOTES.md): what is in each version and its known
  limitations.
- [../THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md): third-party code, tools and
  licences.

This repository contains no original game data. You need your own TopCD disc or ISO
image. The game data is imported from it once into a per-user data directory and read
from there; the disc is not needed at runtime.

## Running from a checkout

    cargo run --release -p milionar

This starts the game with the same setup wizard as the packages. Options of the app:
`--data-dir <DIR>` (another data directory), `--windowed` or `--fullscreen` (override
the saved setting).

## Importing the game data

The `milionar-tool` developer tool imports and checks the data from the command line:

    cargo run --release -p milionar-tool -- import <ISO|Setup.exe|installed folder>
    cargo run --release -p milionar-tool -- verify
    cargo run --release -p milionar-tool -- coverage

- `import` accepts the disc image (`.iso`), the release `Setup.exe` (for example on a
  mounted disc) or a folder where the original installer put the game. Every file is
  checked against the release's SHA-256 manifest; a failed or cancelled import leaves any
  earlier import untouched.
- `verify` re-hashes the imported files against the manifest.
- `coverage` decodes every question, audio entry and Smacker frame and checks the
  expected counts.

The data directory is

- macOS: `~/Library/Application Support/milionar-port/Milionar`
- Windows: `%APPDATA%\milionar-port\Milionar`

with the game files under `game/`. Pass `--data-dir <DIR>` to use another location.

## Upscaled art

Optionally the game can show the Smacker video frames upscaled 3x with Real-ESRGAN
(model `realesr-animevideov3`). The pack is generated on your machine from your imported
data; nothing is downloaded with the game.

    tools/fetch-upscaler.sh
    cargo run --release -p milionar-tool -- upscale

- `tools/fetch-upscaler.sh` downloads the pinned runner and models into
  `tools/upscalers/` (checked against SHA-256 values; never committed). By default it
  fetches both the macOS and the Windows build (about 97 MB of zips);
  `tools/fetch-upscaler.sh macos` or `tools/fetch-upscaler.sh windows` fetches one
  platform. `--runner-dir <DIR>` of `upscale` points at another copy.
- The runner needs a Vulkan GPU on Windows (the driver's `vulkan-1.dll`) and Metal on
  macOS (it ships MoltenVK). A GPU is required; there is no CPU mode.
- Time and disk: about 8 minutes on Apple silicon for all 3235 frames, and a pack of
  about 1.1 GB at `<data-dir>/upscaled/frames.upk`. The run is resumable; progress is
  cached under `upscaled/cache/` until the pack is written.
- The game offers the same step itself after the import (and later from the options,
  F10), using the runner bundled next to the app or, in a repository build,
  `tools/upscalers/`.
- In the game, F10 opens the options over the paused game: art mode, scaling,
  fullscreen, volume, importing the data again and making the upscaled art again (both
  end the current game first).
- In the game, F8 toggles between original and upscaled art (saved in the settings).
  Upscaled art is the default whenever a valid pack exists. A frame missing from the
  pack falls back to the original art for that frame only; the rest stays upscaled and
  F8 keeps working. F8 does nothing only when the whole pack is missing or invalid, and
  the game then shows the original art.
- Details: [upscale-pack.md](upscale-pack.md). Licences of the runner and models:
  [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

## Screenshots of the original art

    cargo run --release -p milionar-tool -- screenshot --script <replay.json> --tick <N> --out <PNG>

renders the frames presented at the given ticks of a replay script on the headless host
(original art only). Replay scripts are JSON input scripts, see `crates/engine/src/replay.rs`
and the examples in `crates/engine/tests/replays/`. The output contains game art: do not
commit it. The only game art in the repository is the handful of README screenshots in
`screenshots/`.

## Build and test

    cargo build --workspace
    cargo test --workspace
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    python3 -B -m unittest discover -s packaging
    python3 -B -m unittest discover -s tools/release

## Toolchain

The toolchain is pinned in `rust-toolchain.toml`. Homebrew's cargo on PATH ignores that
file; put rustup's 1.100 toolchain first on PATH for the Windows check:

    PATH="$(dirname "$(rustup which cargo)"):$PATH" cargo check --workspace --target x86_64-pc-windows-msvc

## Licence

GPL-3.0-only, see [LICENSE](../LICENSE).
