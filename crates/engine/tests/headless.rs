use std::sync::Arc;

use engine::{
    Abort, AudioCmd, Channel, Ctx, DrawOp, HeadlessHost, Host, MouseButton, Pacing, Prefix, Quit,
    Rect, ScriptEvent, SoundId, SoundSource, Vk,
};
use formats::wav::Pcm;

fn ten_presents(ctx: &mut Ctx<'_>, count: &mut u32) -> Result<(), Abort> {
    for _ in 0..10 {
        *count += 1;
        ctx.present()?;
    }
    Ok(())
}

#[test]
fn quit_unwinds() {
    let mut host = HeadlessHost::with_script(vec![(3, ScriptEvent::Quit)]);
    let mut ctx = Ctx::new(&mut host, 1);
    let mut count = 0;
    assert_eq!(ten_presents(&mut ctx, &mut count), Err(Abort::Quit));
    assert_eq!(count, 4);
    // Quit is sticky, also against clear_new_game and a later NewGame request.
    ctx.clear_new_game();
    ctx.request_abort(Abort::NewGame);
    assert_eq!(ctx.present(), Err(Abort::Quit));
}

#[test]
fn host_quit_is_sticky_at_host_level() {
    let mut host = HeadlessHost::with_script(vec![(0, ScriptEvent::Quit)]);
    assert_eq!(
        host.present(engine::Frame::new(), Pacing::Tick66),
        Err(Quit)
    );
    assert_eq!(
        host.present(engine::Frame::new(), Pacing::Tick66),
        Err(Quit)
    );
}

#[test]
fn new_game_abort_unwinds_and_clears() {
    let mut host = HeadlessHost::with_script(vec![]);
    let mut ctx = Ctx::new(&mut host, 1);
    ctx.present().unwrap();
    ctx.request_abort(Abort::NewGame);
    // Every enclosing loop sees it until MainMenuLoop clears it.
    assert_eq!(ctx.present(), Err(Abort::NewGame));
    assert_eq!(ctx.present_cutscene(83), Err(Abort::NewGame));
    assert_eq!(ctx.pending_abort(), Some(Abort::NewGame));
    ctx.clear_new_game();
    assert_eq!(ctx.present(), Ok(()));
    drop(ctx);
    // The aborted presents never reached the host.
    assert_eq!(host.ticks(), 2);
}

fn loop_with_question_mark(ctx: &mut Ctx<'_>) -> Result<(), Abort> {
    let quit: Result<(), Quit> = Err(Quit);
    ctx.present()?;
    quit?;
    Ok(())
}

#[test]
fn quit_converts_into_abort() {
    let mut host = HeadlessHost::with_script(vec![]);
    let mut ctx = Ctx::new(&mut host, 1);
    assert_eq!(loop_with_question_mark(&mut ctx), Err(Abort::Quit));
}

#[test]
fn flush_input_drops_pending_and_current_taps() {
    let mut host = HeadlessHost::with_script(vec![
        (0, ScriptEvent::Tap(Vk::A)),
        (0, ScriptEvent::KeyDown(Vk::SHIFT)),
    ]);
    let mut ctx = Ctx::new(&mut host, 1);
    ctx.present().unwrap();
    assert!(ctx.input.held.contains(&Vk::A));
    ctx.flush_input();
    assert_eq!(ctx.input.last_key, None);
    assert!(!ctx.input.held.contains(&Vk::A));
    assert!(ctx.input.held.contains(&Vk::SHIFT));
}

#[test]
fn script_events_arrive_on_their_tick() {
    let mut host = HeadlessHost::with_script(vec![
        (2, ScriptEvent::Tap(Vk::A)),
        (1, ScriptEvent::KeyDown(Vk::SHIFT)),
        (4, ScriptEvent::KeyUp(Vk::SHIFT)),
        (2, ScriptEvent::MouseMove(100, 200)),
        (2, ScriptEvent::MouseDown(MouseButton::Left)),
    ]);
    let mut ctx = Ctx::new(&mut host, 1);
    let mut seen = Vec::new();
    for _ in 0..6 {
        ctx.present().unwrap();
        seen.push(ctx.input.clone());
    }
    assert_eq!(seen[1].last_key, Some(Vk::SHIFT));
    assert!(seen[2].held.contains(&Vk::SHIFT));
    assert_eq!(seen[2].last_key, Some(Vk::A));
    assert!(seen[2].held.contains(&Vk::A) && !seen[2].physically_held.contains(&Vk::A));
    assert_eq!(seen[2].mouse, (100, 200));
    assert_eq!(seen[2].mouse_button, Some(MouseButton::Left));
    assert_eq!(seen[3].last_key, None);
    assert!(!seen[3].is_down(Vk::A));
    assert_eq!(seen[3].mouse, (100, 200));
    assert_eq!(seen[3].mouse_button, None);
    assert!(seen[3].held.contains(&Vk::SHIFT));
    assert!(!seen[4].held.contains(&Vk::SHIFT));
}

#[test]
fn shift_f12_quits() {
    let mut host = HeadlessHost::with_script(vec![
        (0, ScriptEvent::KeyDown(Vk::SHIFT)),
        (1, ScriptEvent::Tap(Vk::F12)),
    ]);
    let mut ctx = Ctx::new(&mut host, 1);
    assert_eq!(ctx.present(), Ok(()));
    assert_eq!(ctx.present(), Err(Abort::Quit));
}

#[test]
fn records_frame_hashes_and_time() {
    let mut host = HeadlessHost::with_script(vec![]);
    {
        let mut ctx = Ctx::new(&mut host, 1);
        ctx.draw(DrawOp::Fill {
            rect: Rect::new(0, 0, 640, 480),
            color: [0, 0, 0],
        });
        ctx.present().unwrap();
        ctx.present().unwrap();
        ctx.present_cutscene(83).unwrap();
        ctx.present_cutscene(10).unwrap();
        assert_eq!(ctx.now_ms(), 66 + 66 + 83 + 66);
        assert!(ctx.frame.ops.is_empty());
    }
    let h = host.frame_hashes();
    assert_eq!(h.len(), 4);
    assert_ne!(h[0], h[1]);
    assert_eq!(h[1], engine::Frame::new().stable_hash());
    assert_eq!(
        host.pacings(),
        [
            Pacing::Tick66,
            Pacing::Tick66,
            Pacing::Cutscene { min_ms: 83 },
            Pacing::Cutscene { min_ms: 10 }
        ]
    );
    assert_eq!(host.ticks(), 4);
}

fn voice(n: u16) -> SoundId {
    SoundId::new(Prefix::Char, n)
}

/// Every clip is silent PCM at 22050 Hz mono; `f` gives its length in ms.
fn ms_source(f: impl Fn(SoundId) -> u32 + Send + 'static) -> Box<dyn SoundSource + Send> {
    Box::new(move |id: SoundId| {
        Some(Arc::new(Pcm {
            sample_rate: 22_050,
            channels: 1,
            bits_per_sample: 16,
            samples: vec![0; (f(id) * 22_050 / 1000) as usize],
        }))
    })
}

#[test]
fn voice_busy_follows_clip_durations() {
    // Clip n lasts n ms: 100 ms -> 2 ticks, 132 ms -> 2 ticks, 133 ms -> 3 ticks.
    let mut host = HeadlessHost::new(vec![], ms_source(|id| u32::from(id.number)));
    let mut ctx = Ctx::new(&mut host, 1);
    assert!(!ctx.voice_busy());
    ctx.audio(AudioCmd::QueueVoice(voice(100)));
    ctx.audio(AudioCmd::QueueVoice(voice(133)));
    let mut busy_ticks = 0;
    while ctx.voice_busy() {
        ctx.present().unwrap();
        ctx.pump_voice();
        busy_ticks += 1;
        assert!(busy_ticks < 100);
    }
    // The pump runs once per tick (after each present, as the screens call it) and starts the next clip only when it sees the previous
    // ended: 100 ms takes 2 ticks, then 133 ms takes 3 (the original's VoiceQueuePump).
    assert_eq!(busy_ticks, 5);

    ctx.audio(AudioCmd::QueueVoice(voice(1000)));
    assert!(ctx.voice_busy());
    ctx.present().unwrap();
    ctx.pump_voice();
    ctx.audio(AudioCmd::StopVoice);
    assert!(!ctx.voice_busy());

    // Pausing the voice channel freezes the remaining time.
    ctx.audio(AudioCmd::QueueVoice(voice(100)));
    ctx.present().unwrap();
    ctx.pump_voice();
    ctx.audio(AudioCmd::PauseChannel(Channel::Voice, true));
    for _ in 0..5 {
        ctx.present().unwrap();
        ctx.pump_voice();
        assert!(ctx.voice_busy());
    }
    ctx.audio(AudioCmd::PauseChannel(Channel::Voice, false));
    ctx.present().unwrap();
    ctx.pump_voice();
    assert!(!ctx.voice_busy());
    assert_eq!(ctx.voice_envelope(), 0);

    // A sound played directly on the voice channel also keeps it busy.
    ctx.audio(AudioCmd::Play {
        id: voice(132),
        channel: Channel::Voice,
        looped: false,
    });
    ctx.present().unwrap();
    ctx.pump_voice();
    assert!(ctx.voice_busy());
    ctx.present().unwrap();
    ctx.pump_voice();
    assert!(!ctx.voice_busy());
}

#[test]
fn sfx_and_music_do_not_make_voice_busy_and_are_logged() {
    let mut host = HeadlessHost::new(vec![], ms_source(|_| 5000));
    {
        let mut ctx = Ctx::new(&mut host, 1);
        ctx.present().unwrap();
        ctx.audio(AudioCmd::Play {
            id: SoundId::new(Prefix::Sfex, 4057),
            channel: Channel::Sfx,
            looped: false,
        });
        ctx.audio(AudioCmd::Play {
            id: SoundId::new(Prefix::Musi, 10),
            channel: Channel::Music,
            looped: true,
        });
        assert!(!ctx.voice_busy());
    }
    let log = host.audio_log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].0, 1);
    assert!(matches!(
        log[1].1,
        AudioCmd::Play {
            channel: Channel::Music,
            ..
        }
    ));
    assert!(!host.voice_busy());
}
