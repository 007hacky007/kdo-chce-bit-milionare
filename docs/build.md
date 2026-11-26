# Building

Everything here runs locally and writes only into the repository (`target/`, `dist/`,
`tools/cache/`, `tools/upscalers/`, all gitignored). Nothing is uploaded.

Overview of a full local release build on an Apple silicon Mac:

    tools/fetch-upscaler.sh                  # pinned Real-ESRGAN runner + model (both platforms)
    packaging/macos/build-app.sh             # dist/Milionar.app
    packaging/macos/build-dmg.sh             # dist/Milionar-<version>-macos-arm64.dmg (+ .sha256)
    packaging/windows/build-tree.sh          # dist/Milionar-windows-x64/ (needs llvm-mingw, below)
    packaging/windows/build-installer.sh     # dist/Milionar-<version>-windows-x64-setup.exe (+ .sha256)

Details of the packaging scripts and the guard that checks every package: `packaging/README.md`.

## Prerequisites

- rustup; the toolchain is pinned in `rust-toolchain.toml` (1.100). Put rustup's `cargo`
  first on `PATH` if Homebrew's is installed.
- CMake (SDL is built from source), Python 3.11 or later (`tomllib`), curl, unzip.
- macOS packages: the Xcode command line tools (`codesign`, `iconutil`, `sips`, `vtool`,
  `plutil`, `lipo`, `hdiutil`).
- Windows packages on macOS: llvm-mingw (below) and wine for the installer, or a Windows
  machine with Inno Setup 6.

## Tests

    cargo test --workspace
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    python3 -B -m unittest discover -s packaging       # guard and installer script
    python3 -B -m unittest discover -s tools/release   # public export and hygiene scan

## macOS (native)

    cargo build --release -p milionar

SDL 3.4.20 is compiled from the vendored source of the `sdl3-src` crate (CMake required)
and linked statically.

## Windows x64 on Windows (native MSVC, development only)

    cargo build --release -p milionar

Target `x86_64-pc-windows-msvc`; needs Visual Studio Build Tools and CMake. The manifest
(`packaging/windows/app.manifest`) is embedded by the MSVC linker. This build is for
development and is never shipped: it has no icon and no version resource (only GNU targets
compile `packaging/windows/app.rc` and the version resource, see `apps/milionar/build.rs`),
and `packaging/windows/build-tree.sh` does not use it. The released Windows packages, local
and in CI (`.github/workflows/release.yml`, on a macOS runner), are made from the llvm-mingw
cross build below.

## Windows x64 cross-built on macOS (llvm-mingw, the shipped build)

Toolchain (pinned):

| Item | Value |
|------|-------|
| Name | llvm-mingw 20261104, UCRT, macOS universal |
| URL | https://github.com/mstorsjo/llvm-mingw/releases/download/20261104/llvm-mingw-20261104-ucrt-macos-universal.tar.xz |
| SHA-256 | `66cbd5178c4a71b0d473e68ede172f8eb7d15d023e67f39201d0a69e21f6bf3c` |
| Size | 125261696 bytes |
| Licence | LLVM parts: Apache-2.0 with LLVM exceptions (`LICENSE.TXT`); statically linked mingw-w64 runtime: `x86_64-w64-mingw32/share/mingw32/COPYING.MinGW-w64-runtime.txt`, to be shipped with a distributed exe |

Steps:

    shasum -a 256 llvm-mingw-20261104-ucrt-macos-universal.tar.xz   # must match the table
    mkdir -p tools/cache
    tar -xJf llvm-mingw-20261104-ucrt-macos-universal.tar.xz -C tools/cache
    rustup target add --toolchain 1.100 x86_64-pc-windows-gnullvm
    tools/build-windows.sh            # extra args go to cargo, e.g. --features test-hooks

`tools/cache/` is gitignored. `LLVM_MINGW=<dir>` points the script at another extracted
copy. Make sure the rustup 1.100 toolchain is the `cargo` on `PATH` (a Homebrew `cargo`
has no Windows standard library).

Output: `target/x86_64-pc-windows-gnullvm/release/milionar.exe`, a GUI-subsystem exe with
the manifest embedded as resource 1/RT_MANIFEST (compiled from `packaging/windows/app.rc`
with llvm-mingw's windres by `apps/milionar/build.rs`). It imports only system DLLs
(KERNEL32, USER32, GDI32, ADVAPI32, SHELL32, OLE32, OLEAUT32, COMBASE, IMM32, SETUPAPI,
VERSION, WINMM, NTDLL, bcryptprimitives and the `api-ms-win-crt-*` / `api-ms-win-core-*`
API sets of the Universal CRT, present on Windows 10 and later); libunwind and the C
runtime pieces of llvm-mingw are linked statically, so no extra DLLs ship beside it.
Check with:

    tools/cache/llvm-mingw-20261104-ucrt-macos-universal/bin/llvm-objdump -p \
        target/x86_64-pc-windows-gnullvm/release/milionar.exe | grep "DLL Name"

## Upscaler runner

    tools/fetch-upscaler.sh [macos|windows]

Downloads the pinned Real-ESRGAN ncnn Vulkan runner and the `realesr-animevideov3` model
into `tools/upscalers/`, checked against `tools/upscalers/upscaler.lock.json`. The
packaging scripts run it themselves when a pinned file is missing.

## Release packages

- macOS app and DMG: `packaging/macos/build-app.sh`, then `packaging/macos/build-dmg.sh`.
  Ad hoc signed, not notarised.
- Windows install tree: `packaging/windows/build-tree.sh` (cross build with llvm-mingw as
  above; the exe in the tree is the one the installer ships).
- Windows installer: `packaging/windows/build-installer.sh`. On macOS it installs the
  pinned Inno Setup (`tools/upscalers/innosetup.lock.json`) into a private wine prefix
  under `target/package/` and runs ISCC there; always use the script, never `wine`
  directly (it isolates `HOME` and turns off winemenubuilder, so nothing is written into
  the real home). On Windows (Git Bash) set `ISCC=<path to ISCC.exe>` instead.

See `packaging/README.md` for the layout, the guard and the installer's behaviour.

## Licences of the Rust crates

`packaging/licenses/rust-crates.html` lists every crates.io crate built into the app with
its licence text. It is committed and shipped in both packages. It is generated by
cargo-about, pinned to **0.9.3**:

    cargo install cargo-about --locked --features cli --version 0.9.3 \
        --root tools/cache/cargo-about-0.9.3
    cargo fetch --locked
    tools/release/gen-licenses.sh           # regenerate (commit the result)
    tools/release/gen-licenses.sh --check   # fail if it is out of date

Configuration: `about.toml` (accepted licences, the two shipped targets, crate-root licence
files only) and `about.hbs` (the page). It runs offline: licence texts come only from the
crate sources in the cargo registry. Regenerate it whenever `Cargo.lock` changes. SDL's own
licence and the notices of the parts it builds in are in
`packaging/licenses/SDL3-zlib.txt` (copied from the `sdl3-src` 3.4.20 crate; sources and
hashes in its header).

## Public export

The development repository carries internal notes and history that are not published.
A public source tree of one commit is made with:

    tools/release/export-public.sh <out_dir> [--commit <rev>] [--with-analysis] [--init-git]

- `git archive` of the commit (default `HEAD`; uncommitted changes are never included)
  without the paths listed in the gitignored `tools/release/export-exclude.local` (one git
  pathspec per line, `#` comments; the export refuses to run when the file is missing or
  lists no path) and, unless `--with-analysis` is given, without `analysis/`;
- `ENGINE_REVISION` with the source commit hash;
- a check that the archive holds none of the excluded paths, and a warning for a listed
  path that exists in the checkout untracked but is not ignored by git;
- a hygiene scan (`tools/release/hygiene.py`) that fails on the header phrases of the
  maintainer's internal planning documents (a second line of defence behind the exclude
  list), on home paths (`/Users/<name>`,
  `/home/<name>`, `C:\Users\<name>` other than the test placeholders), Unicode dashes, the
  original game's file names and extensions, and strings of the build machine and its user
  (the same list as the packaging guard, including `MILIONAR_GUARD_FORBID` and the
  gitignored `packaging/.guard-forbid.local`); on failure the output is removed;
- with `--init-git`, a new repository on `master` with a single commit by the public
  pseudonym (global git configuration ignored, signing off). Nothing is pushed.

In a fresh clone of the development repository, recreate the local files first: the
exclude list (the maintainer keeps its contents) and, for untracked internal folders in
the checkout, their ignore lines in `.git/info/exclude`, one per folder in the form
`/<folder>/` (the export warns about a listed folder that git does not ignore).

The exported tree builds and tests on its own (`cargo build --workspace --locked`,
`cargo test --workspace`).

## Release workflow

`.github/workflows/release.yml` runs on a `v*` tag or by hand. It builds the DMG on
`macos-15`, cross-builds the Windows install tree on `macos-15` with the llvm-mingw pinned
above, then compiles the installer on `windows-latest` with Inno Setup 6 (the preinstalled
one, else the pinned version), and keeps the DMG, the setup exe and their `.sha256` files
as workflow artifacts. It checks that `rust-crates.html` is up to date first. It does not
create a release or publish anything; actions are pinned by commit.
