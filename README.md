# Kdo chce bít milionáře - native port

An unofficial native port of the 2000-era Windows game "Kdo chce bít milionáře".

This repository contains no original game data. You need your own TopCD disc or ISO image. The game data is imported from it once into a per-user data directory and read from there; the disc is not needed at runtime.

- Playing: install the macOS DMG or the Windows installer and follow the setup wizard. Player guide in Czech and English, with requirements, upscaling, keys, data and log locations and troubleshooting: [docs/install.md](docs/install.md).
- Building the app, the packages, the licence bundle and the public export: [docs/build.md](docs/build.md) and [packaging/README.md](packaging/README.md).
- What is in this version and its known limitations: [RELEASE-NOTES.md](RELEASE-NOTES.md).

In a development checkout, `cargo run --release -p milionar` starts the game (with the same setup wizard); the `milionar-tool` developer tool below imports and checks the data from the command line.

## Importing the game data

    cargo run --release -p milionar-tool -- import <ISO|Setup.exe|installed folder>
    cargo run --release -p milionar-tool -- verify
    cargo run --release -p milionar-tool -- coverage

- `import` accepts the disc image (`.iso`), the release `Setup.exe` (for example on a mounted disc) or a folder where the original installer put the game. Every file is checked against the release's SHA-256 manifest; a failed or cancelled import leaves any earlier import untouched.
- `verify` re-hashes the imported files against the manifest.
- `coverage` decodes every question, audio entry and Smacker frame and checks the expected counts.

The data directory is

- macOS: `~/Library/Application Support/milionar-port/Milionar`
- Windows: `%APPDATA%\milionar-port\Milionar`

with the game files under `game/`. Pass `--data-dir <DIR>` to use another location.

## Upscaled art

Optionally the game can show the Smacker video frames upscaled 3x with Real-ESRGAN (model `realesr-animevideov3`). The pack is generated on your machine from your imported data; nothing is downloaded with the game.

    tools/fetch-upscaler.sh
    cargo run --release -p milionar-tool -- upscale

- `tools/fetch-upscaler.sh` downloads the pinned runner and models into `tools/upscalers/` (checked against SHA-256 values; never committed). By default it fetches both the macOS and the Windows build (about 97 MB of zips); `tools/fetch-upscaler.sh macos` or `tools/fetch-upscaler.sh windows` fetches one platform. `--runner-dir <DIR>` of `upscale` points at another copy.
- The runner needs a Vulkan GPU on Windows (the driver's `vulkan-1.dll`) and Metal on macOS (it ships MoltenVK). A GPU is required; there is no CPU mode.
- Time and disk: about 8 minutes on Apple Silicon for all 3235 frames, and a pack of about 1.1 GB at `<data-dir>/upscaled/frames.upk`. The run is resumable; progress is cached under `upscaled/cache/` until the pack is written.
- The game offers the same step itself after the import (and later from the options, F10), using the runner bundled next to the app or, in a repository build, `tools/upscalers/`.
- In the game, F10 opens the options over the paused game: art mode, scaling, fullscreen, volume, importing the data again and making the upscaled art again (both end the current game first).
- In the game, F8 toggles between original and upscaled art (saved in the settings). Upscaled art is the default whenever a valid pack exists. A frame missing from the pack falls back to the original art for that frame only; the rest stays upscaled and F8 keeps working. F8 does nothing only when the whole pack is missing or invalid, and the game then shows the original art.
- Details: `docs/upscale-pack.md`. Licences of the runner and models: `THIRD_PARTY_NOTICES.md`.

## Build and test

    cargo build --workspace
    cargo test --workspace
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    python3 -B -m unittest discover -s packaging
    python3 -B -m unittest discover -s tools/release

Licensed under GPL-3.0-only, see LICENSE.

## Toolchain

The toolchain is pinned in rust-toolchain.toml. Homebrew's cargo on PATH ignores that file; put rustup's 1.100 toolchain first on PATH for the Windows check:

    PATH="$(dirname "$(rustup which cargo)"):$PATH" cargo check --workspace --target x86_64-pc-windows-msvc
