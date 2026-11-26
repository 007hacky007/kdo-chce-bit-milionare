# Upscale pack format

The upscaled art mode stores every Smacker frame upscaled 3x by Real-ESRGAN
in one file per user, `<data_dir>/upscaled/frames.upk`. The pack is generated
on the user's machine from their own game import and never ships. Code:
`crates/upscale` (`key.rs`, `pack.rs`, `cache.rs`, `alpha.rs`, `runner.rs`,
`pipeline.rs`).

## Design

- One indexed container, not one file per Smacker file. Identical frames in
  different files (the four `Ans*Selc.smk`, repeated frames inside one file)
  share a single payload, readers open one file, and the whole pack is
  replaced atomically in one rename.
- Entries are keyed by (file, 1-based frame) and carry the content hash of
  the source frame. A reader returns an image only when the stored hash
  equals the hash of the frame the game is about to draw, so a pack from
  another import or an older pipeline can never show a wrong frame; the
  caller falls back to the original pixels instead.
- The header records the SHA-256 of `<data_dir>/game/import.json` (the source
  manifest hash). `PackReader::open` refuses a pack whose manifest hash
  differs from the current import.
- Writers stage into a temporary file in the target directory
  (`.frames.upk.XXXXXX.tmp`), write payloads, then the index, then the
  header, `fsync`, and rename over the target (on Unix the directory is
  synced too). An interrupted writer leaves the previous pack untouched and
  its temporary file is removed when the writer is dropped; if the process
  dies instead, the next `PackWriter::create` for the same path deletes the
  stale `.frames.upk.*.tmp` files (one writer per pack at a time). The
  pipeline enforces that with an exclusive lock on
  `<data_dir>/upscaled/.upscale.lock`, held for the whole run including
  the sweeps; a second run while it is held fails with `Busy` and touches
  nothing. It is separate from setup's `.import.lock`.
- On Windows a rename cannot replace a file another process holds open,
  and the game keeps the pack open while it runs. The game therefore
  releases the pack before a rebuild promotes the new one and opens it
  again afterwards.
- Resume goes through the frame cache (below): every encoded frame is
  stored there first, and the pack is assembled from the cache at the end.
  Cancelling at any point loses at most the frames in flight.
- Readers validate every field, bound every length by the file size, check
  an index checksum and a per-payload checksum, and return errors for
  malformed input; they never panic. A panic inside the JPEG decoder is
  caught (`catch_unwind`) and reported as an invalid payload.
- The header records the pipeline version (fill, mask and blit rule) and
  the index records per file the blit mode its frames were made for
  (opaque or transparent). The pipeline rebuilds a pack whose pipeline
  version, blit modes, runner params, keys or hashes differ from the
  current run; the game refuses a pack from another pipeline version and
  falls back per draw when the draw's blit differs from the file's mode.
- `PackReader::open` checks the format version and the source manifest
  hash only. It does not compare the pipeline version, the runner and
  model hashes, model name, scale, tile, TTA or GPU; callers do
  (`render::UpscaledPack::open` checks pipeline version and scale, the
  pipeline compares the whole header).

## Content hash

`content_hash(pixels, palette, width, height)` is the first 16 bytes of
SHA-256 over:

| Bytes | Field |
|-------|-------|
| 16 | domain prefix `MILUPK-FRAME` followed by `00 00 00 01` |
| 4 | width, u32 LE |
| 4 | height, u32 LE |
| 768 | the 256 palette entries, R, G, B per entry |
| width * height | palette indices, rows top to bottom |

The palette is the frame's full 256-entry palette (8-bit components, as the
Smacker decoder returns them). Index 0 is transparent (`analysis/findings.md`
6.1); it is part of the hashed indices like any other value.

## File layout

All integers are little-endian. Offsets are absolute file offsets.

```
+------------------+  0
| header (256 B)   |
+------------------+  256
| payloads         |  encoded frames, back to back, any order
+------------------+  index_offset
| index            |  index_len bytes, ends exactly at end of file
+------------------+
```

### Header (256 bytes)

| Offset | Size | Field | Rule |
|--------|------|-------|------|
| 0 | 8 | magic | ASCII `MILUPACK` |
| 8 | 4 | format version, u32 | must be 2 (version 1 had no pipeline version and no blit modes) |
| 12 | 4 | header length, u32 | must be 256 |
| 16 | 32 | runner SHA-256 | `realesrgan-ncnn-vulkan` binary used |
| 48 | 32 | model `.param` SHA-256 | |
| 80 | 32 | model `.bin` SHA-256 | |
| 112 | 32 | source manifest SHA-256 | of `game/import.json`; must match the reader's |
| 144 | 32 | model name | printable ASCII, 1..=31 bytes, NUL padded (all bytes after the first NUL are 0) |
| 176 | 4 | scale, u32 | 3 |
| 180 | 4 | tile, u32 | 128 |
| 184 | 4 | TTA, u32 | 0 or 1 (always 0) |
| 188 | 4 | GPU index, i32 | 0 |
| 192 | 8 | index_offset, u64 | >= 256 |
| 200 | 8 | index_len, u64 | <= 64 MiB; index_offset + index_len = file length |
| 208 | 32 | index SHA-256 | of the index bytes |
| 240 | 4 | file count, u32 | number of file records in the index |
| 244 | 4 | entry count, u32 | total entries over all file records |
| 248 | 4 | pipeline version, u32 | `upscale::pipeline::PIPELINE_VERSION` of the writer |
| 252 | 4 | reserved | must be 0 |

Readers check, in order: length >= 256, magic, version, header length,
model name NUL padding, TTA flag, reserved bytes, model name characters,
manifest hash, index bounds, index checksum, then the index itself.

### Index

`file count` file records, sorted strictly ascending by name bytes, then
nothing (trailing bytes are an error).

File record:

| Size | Field | Rule |
|------|-------|------|
| 2 | name length, u16 | |
| n | name | lower-case path relative to `game/` with `/` separators, 1..=255 bytes of printable ASCII without upper case, `\` or `:`; no empty, `.` or `..` segment |
| 1 | blit mode, u8 | 0 = opaque, 1 = transparent (index 0 skipped): the blit the file's frames were made for |
| 4 | entry count, u32 | >= 1 |
| 52 * count | entries | sorted strictly ascending by frame |

Entry (52 bytes):

| Offset | Size | Field | Rule |
|--------|------|-------|------|
| 0 | 4 | frame, u32 | 1-based, strictly increasing within the file |
| 4 | 16 | content hash | of the source frame, see above |
| 20 | 4 | width, u32 | upscaled width (source width * 3), 1..=2048 |
| 24 | 4 | height, u32 | upscaled height, 1..=2048 |
| 28 | 1 | codec, u8 | 0 = raw RGBA, 1 = JPEG + RLE alpha |
| 29 | 3 | reserved | must be 0 |
| 32 | 8 | payload offset, u64 | >= 256 |
| 40 | 4 | payload length, u32 | offset + length <= index_offset; raw: exactly width * height * 4 |
| 44 | 8 | payload checksum | first 8 bytes of SHA-256 of the payload |

Several entries may point at the same payload (identical source frames
drawn with the same blit mode; one content hash made for both modes gets
two payloads).
The sum of all file records' entry counts must equal the header's entry
count.

### Payload codecs

Every decoded frame is `width * height` RGBA pixels, rows top to bottom.
Alpha is binary: transparent pixels decode to `00 00 00 00`, visible pixels
have alpha 255. Alpha is the source palette mask (index 0 = transparent)
upscaled by nearest neighbour.

Codec 0, raw: `width * height * 4` bytes of RGBA. Alpha must be 0 or 255.
Used by tests and as a lossless fallback.

Codec 1, JPEG + RLE alpha (the default):

| Size | Field |
|------|-------|
| 4 | jpeg_len, u32 |
| jpeg_len | baseline JPEG of the RGB channels, 3 components, 4:4:4, standard Huffman tables, quality 95; its size must equal the entry's width and height |
| rest | alpha runs |

Alpha runs are unsigned LEB128 varints (7 bits per byte, low bits first,
high bit set on all but the last byte, at most 5 bytes, value <= u32::MAX).
Runs alternate transparent, visible, transparent, ... starting with
transparent (the first run may be 0, no other run may be 0), cover pixels
in row-major order, and must sum to exactly `width * height` with no bytes
left over.

The JPEG encodes the RGB under transparent pixels too. The pipeline leaves
the runner's output there (it upscaled an input whose hidden pixels were
filled from visible neighbours), so the DCT sees no hard edge at the mask
boundary and does not ring into visible pixels.

Optimized Huffman tables are not used: zune-jpeg 0.5.15 mis-decodes some
images that jpeg-encoder 0.7.1 writes with optimized tables (libjpeg decodes
the same files correctly). On the pilot frames they would save about 6 %.

## Frame cache

`<data_dir>/upscaled/cache/<content hash as 32 lower-case hex digits>.bin`,
one record per unique source frame (`FrameCache` in `crates/upscale`):

| Offset | Size | Field | Rule |
|--------|------|-------|------|
| 0 | 8 | magic | ASCII `MILUCACH` |
| 8 | 4 | version, u32 | 2 |
| 12 | 16 | content hash | must equal the hash in the file name |
| 28 | 4 | width, u32 | 1..=2048 |
| 32 | 4 | height, u32 | 1..=2048 |
| 36 | 1 | codec, u8 | as in the pack |
| 37 | 1 | blit mode, u8 | as in the pack index; a lookup for the other mode is a miss |
| 38 | 2 | reserved | must be 0 |
| 40 | 4 | payload length, u32 | file length = 76 + payload length |
| 44 | 32 | checksum | SHA-256 over bytes 0..44 and the payload |
| 76 | n | payload | encoded frame, same format as a pack payload |

`put` writes a record to `.XXXXXX.tmp` in the cache directory, syncs it
and renames it to its final name, so a record is either complete or
absent. `get` returns `None` for a missing or invalid record and deletes an
invalid one so the frame is redone; `contains` checks the header and the
file length only. `sweep_temp` deletes orphan `.*.tmp` files and should run
once when a pipeline run starts (one writer at a time).

The cache directory also holds `stamp.txt`: the pipeline version, codec
id, JPEG quality and the runner params (hashes, model, scale, tile, TTA,
gpu). When the stamp differs from the current run, all records are deleted
before use, so frames from another runner, model or pipeline are never
mixed into a pack.

## Pipeline

`upscale::pipeline::run` (driven by `milionar-tool upscale` and by the
game's setup wizard and options) builds the pack:

1. Scan every `.smk` under `game/` (80 files, 3235 frames, cutscenes
   included), key each frame by (lower-case file, 1-based frame) and its
   content hash. Identical frames are one unique frame (2453 in the
   corpus; the four `Ans*Selc.smk` are one set).
2. If the existing pack has this import's manifest hash, the same
   `RunnerParams` and exactly these keys with these hashes, stop: up to
   date. Otherwise open the cache (stamp check, orphan temp files swept,
   stale `.work-*` directories removed).
3. Unique frames already in the cache are skipped. If a pack already
   exists (for example one made before a re-import, so its manifest hash
   no longer matches) and has the same `RunnerParams` and pipeline
   version, its payloads are copied into the cache for every pending
   frame with the same content hash and blit mode ("seeded"; the payload
   checksum is verified, a bad payload is simply redone). Re-importing the
   same disc therefore rebuilds without the runner (about a minute and a
   half on Apple Silicon, mostly writing the cache and the pack). The others are decoded
   again file by file and sent to the runner in batches of 64: each frame
   becomes an RGB PNG (see Transparency), the runner upscales the batch
   directory, each output is checked to be exactly 3x the source size,
   masked, encoded (JPEG q95 + RLE alpha) on all cores and stored with
   `FrameCache::put`.
4. The pack is assembled from the cache in key order with
   `PackWriter::add_cached` and promoted by rename. A cache record that fails
   its checksum when read here is deleted and that frame is redone (up to
   two more rounds).
5. After a successful promote the cache directory is deleted, unless
   `Options::keep_cache` (`--keep-cache`) is set; keeping it makes a rebuild
   after a pack loss or a changed import cheap.

Progress comes in three phases, each with its own done/total that only
grows: `Scan` (files), `Upscale` (unique frames: cached, seeded or done
now; reported after every batch and every 64 seeded frames) and
`Assemble` (pack entries, every 64). Each report has the time since the
start of the run and an estimate for the rest of the phase from this
run's rate in it (the seeding copy does not count for the upscale
estimate). A record that fails its checksum during assembly sends the run
back to `Upscale` for that frame; only then do phases repeat.

Cancel (an `AtomicBool`) is checked between frames while decoding, before
every runner call and every 50 ms while the runner runs (the subprocess is
killed). Cancelling or killing the process at any point loses at most the
batch in flight; the next run resumes from the cache. The previous pack
stays valid until the new one is renamed over it.

### Transparency

The engine draws a file either opaque or skipping palette index 0, chosen
per load site and never mixed for one file. `upscale::blit_mode` encodes
that usage: `cuts/*` and the backgrounds and full-screen animations loaded
opaque (`beat/bs_bg`, `menus/beatkey`, `bios`, `keys2-4`, `logostill`,
`np`, `opening`, `pm`, `pm_end`, `quesansw/mm_bg`, `qs_bg`) are
`Opaque`; every other file is `Transparent`.

- Opaque: the runner sees the true palette colours (index 0 is the black
  of the picture, up to 74 % of a cutscene frame) and alpha is 255
  everywhere.
- Transparent: index-0 pixels take the colour of the nearest visible pixel
  (breadth-first, 4-neighbours) before upscaling, so no dark fringe bleeds
  into the edges, and alpha is the index-0 mask scaled 3x by nearest
  neighbour (binary). The runner's RGB stays under transparent pixels.
  A frame without index 0 comes out fully opaque.

The runtime must use an entry only for the blit it was made for: on a
draw whose `transparent` flag differs from `blit_mode(file)` it falls back
to the original pixels. If one content hash occurs in both kinds of file
the opaque version is stored and the transparent keys are left out of the
pack (counted in `Report::conflicts`; the corpus has none).

### Runner

`Runner::locate(dir)` checks `dir/realesrgan-ncnn-vulkan[.exe]` and
`dir/models/realesr-animevideov3-x3.{param,bin}` against the SHA-256 pins
(`tools/upscalers/upscaler.lock.json`, fetched by `tools/fetch-upscaler.sh`)
and runs `-i <in> -o <out> -n realesr-animevideov3 -s 3 -t 128 -g 0 -f png
-m <dir>/models`. Failures map to `UpscaleError`:

| Case | Error |
|------|-------|
| stderr contains `invalid gpu device`, `vkCreateInstance failed` and similar (any non-zero exit), or on Windows exit 0xC0000135 (DLL missing) | `NoVulkan` |
| killed by a signal, or a Windows exception exit | `RunnerCrashed` |
| other non-zero exit | `RunnerFailed` |
| exit 0 but an input has no output (the runner exits 0 when it cannot decode or encode an image) | `MissingOutput` |
| output not a PNG or not 3x the source size | `BadOutput` |

The runner is tied to the calling process. On Unix it runs in its own
process group and a cancel kills the whole group. On Windows it is started
with `CREATE_NO_WINDOW` (no console window) and assigned to a Job Object
with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so it dies with the app even
when the app is killed; a cancel terminates the job.

All messages carry the tail of the runner's stderr. `NoVulkan` and
`RunnerCrashed` say that the game still runs with the original art.
`FakeRunner` (nearest-neighbour 3x in Rust, `--fake-runner`) stands in for
tests and CI.

`Runner::probe` upscales one 4x4 image and reads the device ncnn prints
for GPU 0 (`[0 Apple M1 Pro]  queueC=...`). A device named llvmpipe,
lavapipe or SwiftShader is a software rasteriser on the CPU
(`GpuProbe::cpu_only`); the game warns that upscaling would take hours and
lets the player choose. The probe also fails early with `NoVulkan`.

`milionar-tool upscale` takes the runner from `--runner-dir <DIR>`. Without
it the tool, like the game, uses `runner::find_runner_dir`, which looks
next to the running executable, never in the working directory:
`upscaler/` beside it (the Windows install tree), `../Resources/upscaler/`
(a macOS app bundle), `upscalers/<runner>` beside it (a copied tool), then
`tools/upscalers/<runner>` of the repository it was built in
(`target/<profile>/` or `target/<triple>/<profile>/`, never above a
directory holding `.git`). `<runner>` is `realesrgan-20220424`
(`-windows` on Windows). When none holds the runner the tool stops with
the list of places it looked.

The game checks for free space before it starts and reports `NoSpace`
otherwise: 3.5 GB (`runner::FREE_SPACE_NEEDED`: cache, staging pack and
old pack side by side) less the cache records this runner can reuse, and
nothing when the pack is already current (`pipeline::free_space_needed`).
It never runs the pipeline while a game is open: a rebuild from the
options ends the game first, so the pack file is closed before the new
one is renamed over it (Windows refuses to replace an open file), and the
job holds `<data_dir>/.playing.lock` exclusively for its whole run
(`setup::PlayingLock::acquire_exclusive`). A game in another window makes
it stop at once with a "quit the other game, then try again" message, and
no game or import can start in another window while it runs. The hidden
`--cancel-after N` (cancel once N frames are done, like an interrupt) and
`--batch-size N` options exist for the CLI tests.

## Runtime

`render::UpscaledPack::open(<data_dir>/upscaled, manifest hash,
expectation)` opens the pack and refuses it when the manifest hash, the
pipeline version or the scale (3) differ. The game passes
`PackExpectation::pinned()`, which also requires the pinned runner build
(macOS or Windows), the pinned model and the fixed runner parameters, so
a `--fake-runner` pack is never shown to players.
Frames are decoded lazily on the compositing thread and kept in an LRU
bounded by bytes of decoded RGBA (256 MB by default), keyed by payload so
identical frames are decoded once. A payload that fails its checksum or
does not decode is remembered and never retried. Frames of cutscene
stream slots (`Cuts/*.smk`, `Menus/Opening.smk`; marked by the engine in
the frame identity) are shown once, so they never enter the LRU: each is
held only for the compose that draws it (outside the budget, one
1920x1440 frame in practice) and dropped at the next prefetch, so a
cutscene does not push the screen backgrounds out.

The engine's frame snapshot names every draw by pack key (the file's
path relative to `game/`, lower case), 1-based frame number and content
hash (hashed once per decoded frame on the game thread). At s = 3 with a
pack set, the compositor draws an entry only when key, hash, blit mode
and size (exactly 3x the source) all match; otherwise it draws the
original palette frame by nearest neighbour, counts a miss and reports
the file once through its miss log. Before each compose it decodes the
frames the display list needs that are not in memory yet, in parallel on
at most `available_parallelism` threads.

### In the game

- `settings.json` gains `"art_mode": "original" | "upscaled"` (read
  leniently like the other fields). The default is `upscaled`: the game
  uses the upscaled art whenever a valid pack is present and the original
  art otherwise. A missing or invalid pack does not change the setting, so
  the upscaled art comes back once the pack has been generated.
- At startup (after the import check) the app opens
  `<data_dir>/upscaled/frames.upk` with the SHA-256 of `game/import.json`
  and `PackExpectation::pinned()`. Any error is logged once to
  `milionar.log` and the game runs with the original art.
- F8 switches the art on the main thread. It takes effect at the next
  compose, so game state, timing and the display list are the same in both
  modes. The setting is saved like the window size, 0.5 s after the last
  change. No on-screen UI is added; the switch and the compose times of
  the mode it leaves are logged. Without a valid pack F8 does nothing and
  is logged once.
- Upscaled art composes at s = 3 (1920x1440) whatever the window size,
  then uses the usual letterbox scaling (fit, integer or stretch). The
  texture is drawn with nearest filtering only when it lands at exactly
  1920x1440; otherwise it is filtered linearly, in integer scaling mode
  too. Original art keeps the usual choice of s, with nearest filtering
  at an exact size or in integer mode, and linear otherwise.
- Fallbacks (missing or stale entries) are logged to `milionar.log`, once
  per file, through the compositor's miss log.

## Codec pilot

100 representative unique frames were exported from the imported game
(30 full-screen backgrounds without transparency, 30 overlays with
transparency, 40 cutscene frames from `Cuts/*` and `Menus/Opening.smk`),
sampled evenly over each class, with hidden pixels filled from the nearest
visible neighbour, and upscaled with the pinned runner
(`realesrgan-ncnn-vulkan` 20220424 macOS, `-n realesr-animevideov3 -s 3
-t 128 -g 0 -f png`) on an Apple M1 Pro: 18 s for the 100 frames.

The game has 80 Smacker files and 3235 frames, 2453 of them unique by
content hash (backgrounds 591, overlays 1074, cutscenes 788). Totals are
extrapolated per class by bytes per source pixel over the unique frames
of that class; the "no dedupe" column stores all 3235 frames. Decode times
are for the 76 sampled frames that are 1920x1440, best of 3, release build,
on a loaded machine (load average 5 to 7).

| Codec | Background B/frame | Overlay B/frame | Cutscene B/frame | Decode ms (median / max) | Total, unique | Total, no dedupe |
|-------|-----------:|-----------:|-----------:|------------:|------:|------:|
| PNG fast (lossless) | 2385931 | 921150 | 3055809 | 20.5 / 25.8 | 4.77 GB | 6.36 GB |
| PNG high (lossless) | 2117074 | 875948 | 3105452 | 19.2 / 29.2 | 4.61 GB | 6.13 GB |
| WebP lossless (image-webp) | 2205561 | 882792 | 3100277 | 33.4 / 43.0 | 4.66 GB | 6.20 GB |
| QOI (lossless) | 2511680 | 1021551 | 3573386 | 11.9 / 13.3 | 5.36 GB | 7.13 GB |
| zstd 9 raw RGBA (lossless) | 2787167 | 949116 | 4044307 | 10.2 / 12.9 | 5.81 GB | 7.72 GB |
| zstd 19 raw RGBA (lossless) | 2423378 | 854350 | 3738799 | 12.7 / 22.1 | 5.26 GB | 6.98 GB |
| JPEG q85 + RLE alpha | 145057 | 162028 | 417113 | 7.3 / 12.2 | 0.59 GB | 0.84 GB |
| JPEG q90 + RLE alpha | 195653 | 197976 | 531477 | 8.8 / 14.4 | 0.74 GB | 1.07 GB |
| JPEG q92 + RLE alpha | 229559 | 221730 | 599969 | 9.8 / 15.9 | 0.84 GB | 1.21 GB |
| **JPEG q95 + RLE alpha** | 356925 | 309952 | 838667 | 12.6 / 18.9 | **1.20 GB** | 1.73 GB |

Quality of the JPEG variants against the lossless runner output, visible
pixels only (PSNR over all sampled pixels / worst frame / max channel error):
q85 40.3 dB / 32.8 dB / 65, q90 41.3 / 34.6 / 45, q92 41.9 / 35.5 / 42,
q95 43.4 / 38.4 / 28. The RLE alpha averages 417, 4181 and 24484 bytes per
frame for the three classes; it is included in the JPEG sizes above.

Lossless does not fit: the AI output has fine texture everywhere and the
best lossless codec needs about 1.1 bytes per output pixel, 4.6 GB for the
unique frames against the 1.5 GB target. JPEG q95 4:4:4 is the chosen
codec: 1.20 GB for the unique frames, within the target, 43.4 dB on
average and 38.4 dB on the worst frame. Its decode is about 12.6 ms median
but reached 18.9 ms at most in the pilot, above the 15 ms target; this is
accepted because the runtime decodes each frame once and keeps it
in an LRU cache, so a slow decode costs one tick at most when a frame first
appears, well inside the 66 ms tick. q90 (0.74 GB, about 9 ms) is the
fallback if decode time turns out to matter. The quality is a writer
constant (`JPEG_QUALITY`); readers do not depend on it.

End to end through the crate (`PackWriter` then `PackReader::read_frame`,
which reads the payload, checks its checksum, decodes the JPEG and applies
the alpha runs), at q95: the 100-frame pilot pack is 53559917 bytes
(535599 bytes per frame, a cutscene-heavy sample); reading a 1920x1440
frame takes 15.0 ms median, 22.2 ms p95, 23.6 ms max on the same loaded
machine (load average 4 to 6; payload read and checksum about 1.7 ms of
that, JPEG decode and alpha 13.2 ms median). At q90 the same run measured
10.4 ms median, 16.1 ms p95 and a 33074838-byte pack. Encoding takes about
20 ms per full-screen frame.

Pilot inputs and outputs were kept outside the repository; no game-derived
image or pack is committed.
