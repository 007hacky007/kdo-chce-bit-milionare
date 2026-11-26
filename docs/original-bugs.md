# Original bugs and quirks: decisions

The bug rule of this port: fix crashes, softlocks, save corruption and obvious glitches; keep question selection, shuffling, lifeline odds, RNG behaviour and timing exactly as shipped. Item numbers F11.n refer to section 11 of `analysis/findings.md`; addresses are in the original executable. Pause voice suspension, the explicit voice pump and the GDI realised font size are faithful reproductions of original behaviour, not fixes, and are not listed here.

Summary: FIX F11.3, F11.4, F11.8 (input latch), bio screen; DROP F11.6, F11.13; N/A F11.5; REPLACED F11.14; everything else KEEP.

## F11.1 Last question of each database unreachable

- Original: the order arrays hold 230 / 300 / 270 ids while the files hold 231 / 301 / 271 records, so the last record of each file is never asked.
- Cause: array sizes in the selection routine at 0x405420.
- Decision: KEEP (question selection must stay exact).
- Implemented: `crates/engine/src/game/select.rs` (`ORDER_LEN`).
- Test: `selection_order_and_sav` in `crates/engine/src/game/tests.rs` (asserts record ids never exceed the order length).

## F11.2 Millionaire 5 third weapon reuses art

- Original: weapon sprite entries 15 and 16 both load the same FistM5B file; no third file ships. File order per millionaire is irregular.
- Cause: sprite table at 0x41db54 / 0x41db9c.
- Decision: KEEP (art as shipped).
- Implemented: `crates/engine/src/beatup/actors.rs` (`SPRITE_FILES`).
- Test: none (data table).

## F11.3 Random timeout beater can be an eliminated player

- Original: when the answer time runs out in multiplayer, the random beater is re-rolled only while it equals the answerer; the second condition compares a computed address with NULL and is always false. A player with no millionaire or with money 0 can be drawn.
- Cause: 0x407800 (inferred effect).
- Decision: FIX. The re-roll also rejects players without a millionaire or with money below 1. Draws are otherwise identical (same RNG calls while the first pick is valid). If no valid candidate exists, no beater is drawn.
- Implemented: `crates/engine/src/game/phases.rs` (`answer_timer_tick`).
- Test: `timeout_beater_skips_eliminated_players` in `crates/engine/src/game/tests.rs`.

## F11.4 .sav files trusted blindly

- Original: truncated files and out-of-range ids leave the previous question in the buffer (a repeat) or index past the arrays; the cursor is not range checked.
- Cause: 0x4058b0, 0x405950.
- Decision: FIX. A save with the wrong length, an id out of range or a cursor out of range is rejected and the order is reshuffled. Saves are also written atomically.
- Implemented: `crates/engine/src/game/saves.rs` (`read_sav`, `write_sav`).
- Tests: `roundtrip_and_wrong_length`, `out_of_range_ids_rejected`, `cursor_out_of_range_rejected`, `save_written_atomically` in `saves.rs`; `truncated_sav_reshuffles` and `degenerate_dbs_do_not_panic` in `crates/engine/src/game/tests.rs`.

## F11.5 Field copy overflow

- Original: the field copy writes a NUL one byte past each 64 / 256 byte field and reads `dst[-1]` for an empty field.
- Cause: 0x40c6c0.
- Decision: N/A in Rust (fields are `String`s, no fixed buffers).
- Implemented: not applicable.
- Test: none.

## F11.6 Slow rescans and loose-WAV probe

- Original: every question rescans the database files from the start and probes loose WAV files over all 800 records. Slow but harmless.
- Cause: 0x405420.
- Decision: DROP (no behaviour change; the port reads the records it needs and has no loose-WAV probe).
- Implemented: `crates/engine/src/game/select.rs`.
- Test: none.

## F11.7 Repeat avoidance only against the previous question

- Original: a question is skipped only when its voice id equals that of the immediately previous question.
- Cause: selection loop (see F11.1).
- Decision: KEEP.
- Implemented: `crates/engine/src/game/select.rs` (`pick_question`).
- Test: `repeat_voice_avoided` in `crates/engine/src/game/tests.rs`.

## F11.8 Hits register only on the exact tick

- Original: a hit registers only on the tick where the fist shows the listed frame. The original polls key state once per tick, so a key tap shorter than one tick can be lost.
- Cause: fist update at 0x401730.
- Decision: KEEP the 66 ms tick logic; FIX lost taps: the port latches a key or mouse press that goes down and up between two ticks so the next tick sees it once.
- Implemented: tick logic in `crates/engine/src/beatup/fist.rs`; latching in `crates/engine/src/input.rs` (`InputAccumulator`).
- Tests: `hit_only_on_listed_frame` in `crates/engine/src/beatup/tests.rs`; `tap_between_ticks_is_seen_once`, `flush_drops_latched_taps_but_keeps_physical_keys`, `arrow_dir_sees_latched_tap` in `crates/engine/src/input.rs`; `flush_input_drops_pending_and_current_taps` in `crates/engine/tests/headless.rs`.

## F11.9 Lip-sync envelope is noise

- Original: for 16-bit audio the envelope averages raw bytes, so mouth motion is effectively noise.
- Cause: 0x40ce30.
- Decision: KEEP.
- Implemented: `crates/engine/src/audio.rs` (`compute_envelope`).
- Tests: `envelope_matches_original_formula`, `envelope_is_shifted_one_byte_with_zero_tail` in `crates/engine/src/audio.rs`.

## F11.10 Win_Conf never blitted

- Original: the Win_Conf animation is loaded and advanced but never drawn on the win screen.
- Cause: 0x4060f0 (the reference run did not check it).
- Decision: KEEP (as shipped).
- Implemented: `crates/engine/src/game/resolve.rs`.
- Test: none.

## F11.11 Answer time 89 vs 164 ticks

- Original: after a buzz the answer time is 89 ticks if the buzz interrupted the question being read and 164 ticks otherwise. Looks intentional (a penalty for interrupting).
- Cause: 0x407910.
- Decision: KEEP.
- Implemented: `crates/engine/src/game/phases.rs` (`buzz_in`).
- Test: `answer_timer_89_vs_164` in `crates/engine/src/game/tests.rs`.

## F11.12 Overlapping special-round schedule arrays

- Original: the special-round schedule arrays overlap in memory.
- Cause: 0x405190. The effective behaviour is documented in F5.5.
- Decision: KEEP the effective behaviour.
- Implemented: `crates/engine/src/beatup/schedule.rs` (`special_schedule`).
- Test: none directly; the schedule is exercised by the replay tests in `crates/engine/tests/replays.rs`.

## F11.13 /quesonly

- Original: the developer switch asks for per-answer CHAR clips that do not exist.
- Cause: sound event 0x32.
- Decision: DROP (dev mode, not ported).
- Implemented: not applicable (event listed in `crates/engine/src/sound_events.rs` for completeness).
- Test: none.

## F11.14 Mandatory display mode change

- Original: the 640x480x16 mode change is mandatory even with `/window`; the game exits when it cannot be set. This is the main compatibility problem on modern Windows.
- Cause: 0x40ee80.
- Decision: REPLACED by a scaled window that draws the 640x480 picture at an integer scale.
- Implemented: `apps/milionar/src/layout.rs`, `apps/milionar/src/host.rs`.
- Tests: `scale_choice`, `mouse_mapping_fit_mode`, `hidpi_window_maps_like_its_pixel_size` and related tests in `apps/milionar/src/layout.rs`.

## F11.15 Event 0x52 for millionaire 4 uses the 0x54 pool

- Original: event 0x52 indexes a four-entry pool array with millionaire 0..4, so millionaire 4 draws its KO line from the event 0x54 pool and shares its position.
- Cause: pool array 0x421b80 (F7.4).
- Decision: KEEP (audio as shipped).
- Implemented: `crates/engine/src/sound_events.rs` and `crates/engine/src/sound_tables.rs` (`PA_421B80`).
- Test: none specific; covered by `pools_start_in_exe_order` and `every_event_resolves_existing_sounds` in `crates/engine/src/sound_events/tests.rs`.

## F11.16 Weapon 14 has no announcement line

- Original: weapon 14 (millionaire 4, third weapon) has voice id 0 in the weapon table, so event 0x55 announces no weapon line.
- Cause: table 0x421e7c.
- Decision: KEEP.
- Implemented: `crates/engine/src/sound_tables.rs`, `crates/engine/src/sound_events.rs` (event 0x55).
- Test: none specific; covered by `every_event_resolves_existing_sounds`.

## F11.17 Five missing sound ids

- Original: five sound ids reachable from sound events do not exist in the archives (SFEX4119, CHAR0431, CHAR0444, CHAR0463, CHAR0491); they play as silence.
- Cause: archive contents (F7.4).
- Decision: KEEP. A missing clip is a silent, zero-length voice clip, so the voice wait ends immediately.
- Implemented: `crates/engine/src/audio.rs` (missing clip placeholder).
- Tests: `missing_sound_is_a_zero_length_voice_clip` in `crates/engine/src/audio.rs`; `every_event_resolves_existing_sounds` in `crates/engine/src/sound_events/tests.rs` (reports the missing ids when the corpus is present).

## Extra: bio screen drawn as a partial frame

- Original: in the Windows 11 reference run the bio screen showed the label boxes and background missing, on black, with block debris on the right column.
- Cause (inferred from the reference run): the original renders the Bios animation frame as a delta over a black buffer, going to the frame without decoding the frames before it, so only the changed blocks are drawn.
- Decision: FIX (obvious visual glitch). The port shows the complete frame.
- Implemented: `crates/engine/src/screens/pick_millionaire.rs` (bio screen drawing).
- Test: none (visual; compared against the reference screenshot of the original).
