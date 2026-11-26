//! Game engine: the original's per-screen loops as imperative Rust on a game thread.
//!
//! Every loop composes `ctx.frame` and calls `ctx.present()?`, which hands the display
//! list to a [`Host`] and blocks until the next 66 ms tick. A closed window surfaces as
//! [`Quit`] and unwinds every loop through `?` (F1.2 universal abort).
#![forbid(unsafe_code)]

pub mod anim;
pub mod assets;
pub mod audio;
pub mod audio_types;
pub mod beatup;
#[rustfmt::skip]
pub mod button_tables;
pub mod buttons;
pub mod ctx;
pub mod game;
pub mod headless;
pub mod host;
pub mod input;
pub mod replay;
pub mod rng;
pub mod screens;
pub mod sound_events;
#[rustfmt::skip]
pub mod sound_tables;
pub mod state;

pub use assets::{AssetError, Assets, CutsceneStream, FrameSnapshot, FrameView, SharedFrames};
pub use audio::{
    Clip, ClipCache, Mixer, SilentSource, SoundSource, command_sound, compute_envelope, volume_gain,
};
pub use audio_types::{AudioCmd, Channel, Prefix, SoundId};
pub use ctx::{Abort, Ctx, DebugState, Pumped};
pub use game::{BeatUpOutcome, Round, Session};
pub use headless::{HeadlessHost, Responder, ScriptEvent};
pub use host::{Host, Pacing, Quit, TICK_MS};
pub use input::{InputAccumulator, InputState, MouseButton, Vk, read_arrow_dir};
pub use render::{AssetId, DrawOp, FontSpec, Frame, Rect, TextLayout};
pub use rng::Lcg;
pub use screens::run;
pub use sound_events::{SoundPools, sound_event, sound_event3};
pub use state::{Game, Player, Sprite, with_sprites};
