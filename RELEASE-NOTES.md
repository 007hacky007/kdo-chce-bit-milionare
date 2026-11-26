# Release notes

## 0.1.0

First release of the native port of "Kdo chce bít milionáře" (TopCD) for macOS on Apple
silicon and Windows x64. The packages contain no data of the original game: you import
it from your own CD, disc image, `Setup.exe` or installed folder. Player guide (Czech and
English): `docs/install.md`.

### What works

- The whole game, reimplemented in Rust from the original's behaviour: questions and
  lifelines, the money ladder, the beat-up rounds, hotseat play for 1 to 4 players on one
  keyboard, videos, voice lines and endings. Question selection, shuffling, lifeline odds
  and timing follow the original.
- Original bugs that crash, lock up or show garbage are fixed; each fix is listed in
  `docs/original-bugs.md`.
- A setup wizard on first start: import from the CD, an `.iso` image, `Setup.exe` or a
  folder, checked file by file against the TopCD release; a cancelled or failed import
  leaves earlier data untouched.
- Optional enhanced graphics: every frame upscaled 3x on your own computer with
  Real-ESRGAN (about 8 minutes on Apple silicon, about 1.1 GB), with progress, cancel and
  resume. F8 switches between original and enhanced graphics.
- Windowed and fullscreen play with fit, whole-multiple and stretch scaling; options over
  the paused game on F10.
- macOS: a DMG with a drag-to-Applications app (macOS 11 or later, Apple silicon).
- Windows: an installer (per-user by default, Czech and English, Start menu shortcut,
  uninstaller that asks whether to keep the imported data).

### Known limitations

- **The Windows build has not yet been run on a real Windows machine.** It is
  cross-compiled on macOS and checked statically (architecture, GUI subsystem, imported
  DLLs, resources), and the installer is built, but the game itself, the import and the
  upscaling have so far been played and tested on macOS only. Treat the Windows package
  as a preview until that test is done.
- Neither package is signed by a paid certificate. macOS blocks the app on first start
  (right-click > Open, or `xattr -d com.apple.quarantine /Applications/Milionar.app`);
  Windows SmartScreen warns about the installer (More info > Run anyway).
- macOS: Apple silicon only; no Intel build.
- Windows: x64 only; Windows on ARM is not tested.
- Enhanced graphics need a GPU: Metal on macOS, a Vulkan driver on Windows. Without one
  the game runs with the original graphics.
- The Windows upscaler needs Microsoft's `vcomp140.dll`, which the installer ships beside
  it, taken unchanged from the upstream runner zip, with a note on Microsoft's Visual C++
  redistributable terms (`licenses/vcomp140-Microsoft-redistributable.txt`).
- Only the Czech TopCD release of the game is supported.
- No gamepad support.

---

### Česky

První vydání nativní verze hry "Kdo chce bít milionáře" (TopCD) pro macOS s čipem Apple
a 64bitová Windows. Balíčky neobsahují data původní hry, ta si hra jednou naimportuje
z vašeho CD. Návod: `docs/install.md`.

Funguje celá hra včetně hry více hráčů na jedné klávesnici, průvodce importem
a volitelné vylepšené grafiky (trojnásobné rozlišení, přepínání klávesou F8).

Známá omezení: **verze pro Windows zatím nebyla vyzkoušena na skutečném počítači
s Windows**, hra byla dosud hraná a testovaná jen na macOS. Balíčky nejsou podepsané
placeným certifikátem (macOS: pravé tlačítko > Otevřít; Windows: Další informace >
Přesto spustit). Vylepšená grafika potřebuje grafickou kartu (ve Windows s ovladačem
Vulkan). Podporované je jen vydání TopCD.
