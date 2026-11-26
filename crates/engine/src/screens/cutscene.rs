//! `PlayFullscreenSmk` 0x40ec80 / 0x40eb80 (F6.3): fullscreen Smacker playback.
//!
//! Input is flushed first. Each frame is decoded, its audio (track 0) queued, and the frame
//! presented on an absolute schedule at the file's own rate (every shipped cutscene is
//! 83.33 ms per frame; presents never go below 66 ms). After each frame the messages are
//! pumped: a left or right click, ESC or Space skips the rest unless `flags` has
//! [`NOT_SKIPPABLE`]; a closed window aborts. No looping: the cutscene ends after its last
//! frame. Buffered cutscene audio is dropped when it ends or is skipped (`SmackClose`).

use render::DrawOp;

use crate::audio_types::AudioCmd;
use crate::ctx::{Abort, Ctx};
use crate::input::Vk;
use crate::state::Game;

/// Flag 8: clicks and keys do not skip (End_01..05).
pub const NOT_SKIPPABLE: u8 = 8;

/// Plays `name`. Returns `Ok(true)` when skipped, `Ok(false)` when it ran to the end or
/// could not be opened (the original shows nothing and goes on).
pub fn play(ctx: &mut Ctx, game: &mut Game, name: &str, flags: u8) -> Result<bool, Abort> {
    // The cutscene draws into its own canvas; nothing composed by the caller shows.
    ctx.frame.ops.clear();
    ctx.flush_input();
    let mut stream = match game.assets.cutscene(name) {
        Ok(s) => s,
        Err(e) => {
            game.note_error(e);
            return Ok(false);
        }
    };
    ctx.flush_input();
    let h = stream.header().clone();
    let audio = h.audio[0].filter(|a| a.sample_rate > 0);
    let id = game.assets.open_stream(name, h.width, h.height);
    game.assets.note_blit(id, false);
    let start = ctx.now_ms();
    let frame_us = u64::from(h.frame_duration_us);
    let mut result = Ok(false);
    for n in 1..=h.frames {
        let frame = match stream.next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                game.note_error(e);
                break;
            }
        };
        if let Err(e) = game
            .assets
            .set_stream_frame(id, n, frame.pixels, frame.palette)
        {
            game.note_error(e);
            break;
        }
        if let Some(a) = audio {
            for c in frame.audio.iter().filter(|c| c.track == 0) {
                ctx.audio(AudioCmd::CutsceneAudio(
                    c.samples.clone(),
                    a.sample_rate,
                    a.channels,
                ));
            }
        }
        ctx.draw(DrawOp::Smk {
            asset: id,
            frame: n,
            x: 0,
            y: 0,
            transparent: false,
        });
        let due = start + (u64::from(n) * frame_us + 500) / 1000;
        let min_ms = due.saturating_sub(ctx.now_ms()).min(u64::from(u32::MAX)) as u32;
        if let Err(a) = ctx.present_cutscene(min_ms) {
            result = Err(a);
            break;
        }
        let p = ctx.pump();
        let key_skip = matches!(ctx.last_key(), Some(Vk::ESCAPE | Vk::SPACE));
        if flags & NOT_SKIPPABLE == 0 && (p.mouse.is_some() || key_skip) {
            result = Ok(true);
            break;
        }
    }
    ctx.audio(AudioCmd::StopCutsceneAudio);
    game.assets.release(id);
    result
}
