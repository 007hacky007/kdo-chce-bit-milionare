//! The options overlay (F10 in the game, or the Options button on the wizard's last
//! pages): art mode, scaling, fullscreen, volume, re-import and making the enhanced
//! graphics again.
//!
//! [`Overlay`] is a pure state machine like the wizard; the app applies its
//! [`Effect`]s. It is drawn as a panel over the frozen game frame (or the wizard page).
//! In the game, re-import and rebuild first ask to confirm that the current game ends;
//! the app then shuts the game down (releasing the playing lock and closing the pack)
//! and continues in the setup wizard.

use render::{DrawOp, Font, FontSpec, Frame, Rect, TextLayout, layout_text};

use crate::art::PackKind;
use crate::settings::{ArtMode, ScalingMode};
use crate::setup_ui::widgets::{ACCENT, DIM, PANEL_BG, PANEL_EDGE, TEXT, WARN};
use crate::strings::{Lang, Str, fill, text};

/// Keys the overlay reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Activate,
    /// Esc: close, or leave the confirmation.
    Back,
}

/// The rows, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Art,
    Scaling,
    Fullscreen,
    Volume,
    Reimport,
    Rebuild,
    Close,
}

const ITEMS: [Item; 7] = [
    Item::Art,
    Item::Scaling,
    Item::Fullscreen,
    Item::Volume,
    Item::Reimport,
    Item::Rebuild,
    Item::Close,
];

/// What the app has to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    Art(ArtMode),
    Scaling(ScalingMode),
    Fullscreen(bool),
    Volume(u32),
    /// Import the game data again (in the game: confirmed, the game ends first).
    Reimport,
    /// Make the enhanced graphics again (in the game: confirmed, the game ends first).
    Rebuild,
    Close,
}

/// Where the overlay is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Over a running (paused) game.
    Game,
    /// Over the setup wizard: no game to end.
    Setup,
}

/// The current settings shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Values {
    /// The setting (what is drawn also needs a ready pack).
    pub art: ArtMode,
    pub scaling: ScalingMode,
    pub fullscreen: bool,
    /// 0..=100.
    pub volume: u32,
}

/// Volume steps of the Left/Right keys.
pub const VOLUME_STEP: u32 = 10;

#[derive(Debug, Clone)]
struct Confirm {
    action: Item,
    focus: usize,
}

/// The overlay's state.
#[derive(Debug, Clone)]
pub struct Overlay {
    lang: Lang,
    place: Place,
    pack: PackKind,
    values: Values,
    focus: usize,
    confirm: Option<Confirm>,
    dirty: bool,
}

impl Overlay {
    pub fn new(lang: Lang, place: Place, pack: PackKind, values: Values) -> Overlay {
        Overlay {
            lang,
            place,
            pack,
            values,
            focus: 0,
            confirm: None,
            dirty: true,
        }
    }

    #[cfg(test)]
    pub fn focused(&self) -> Item {
        ITEMS[self.focus]
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn confirming(&self) -> bool {
        self.confirm.is_some()
    }

    /// The upscaled art can be chosen (a valid pack is open).
    pub fn art_enabled(&self) -> bool {
        self.pack == PackKind::Ready
    }

    /// Shows changed settings (fullscreen toggled by F11, the window leaving
    /// fullscreen, ...).
    pub fn set_values(&mut self, values: Values) {
        if values != self.values {
            self.values = values;
            self.dirty = true;
        }
    }

    /// Needs drawing again (cleared by the call).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn key(&mut self, key: Key) -> Effect {
        self.dirty = true;
        if let Some(c) = &mut self.confirm {
            match key {
                Key::Up | Key::Down | Key::Left | Key::Right => c.focus = 1 - c.focus,
                Key::Activate => {
                    let (action, focus) = (c.action, c.focus);
                    self.confirm = None;
                    return if focus == 0 {
                        Self::action_effect(action)
                    } else {
                        Effect::None
                    };
                }
                Key::Back => self.confirm = None,
            }
            return Effect::None;
        }
        let n = ITEMS.len();
        match key {
            Key::Up => self.focus = (self.focus + n - 1) % n,
            Key::Down => self.focus = (self.focus + 1) % n,
            Key::Left => return self.change(false),
            Key::Right => return self.change(true),
            Key::Activate => return self.activate(),
            Key::Back => return Effect::Close,
        }
        Effect::None
    }

    fn action_effect(item: Item) -> Effect {
        match item {
            Item::Reimport => Effect::Reimport,
            Item::Rebuild => Effect::Rebuild,
            _ => Effect::None,
        }
    }

    /// Enter: a value moves on, an action runs (in the game after a confirmation).
    fn activate(&mut self) -> Effect {
        match ITEMS[self.focus] {
            Item::Art | Item::Scaling | Item::Fullscreen | Item::Volume => self.change(true),
            item @ (Item::Reimport | Item::Rebuild) => match self.place {
                Place::Setup => Self::action_effect(item),
                Place::Game => {
                    self.confirm = Some(Confirm {
                        action: item,
                        focus: 1,
                    });
                    Effect::None
                }
            },
            Item::Close => Effect::Close,
        }
    }

    /// Left/Right on a value row.
    fn change(&mut self, forward: bool) -> Effect {
        let v = &mut self.values;
        match ITEMS[self.focus] {
            Item::Art => {
                if self.pack != PackKind::Ready {
                    return Effect::None;
                }
                v.art = v.art.other();
                Effect::Art(v.art)
            }
            Item::Scaling => {
                use ScalingMode::*;
                v.scaling = match (v.scaling, forward) {
                    (Fit, true) | (Stretch, false) => Integer,
                    (Integer, true) | (Fit, false) => Stretch,
                    (Stretch, true) | (Integer, false) => Fit,
                };
                Effect::Scaling(v.scaling)
            }
            Item::Fullscreen => {
                v.fullscreen = !v.fullscreen;
                Effect::Fullscreen(v.fullscreen)
            }
            Item::Volume => {
                let next = if forward {
                    (v.volume + VOLUME_STEP).min(100)
                } else {
                    v.volume.saturating_sub(VOLUME_STEP)
                };
                // Snap to the steps (a hand-edited setting may sit between them).
                let next = next / VOLUME_STEP * VOLUME_STEP;
                if next == v.volume {
                    return Effect::None;
                }
                v.volume = next;
                Effect::Volume(next)
            }
            Item::Reimport | Item::Rebuild | Item::Close => Effect::None,
        }
    }

    /// The pointer moved to (x, y) in 640x480.
    pub fn pointer(&mut self, x: i32, y: i32) {
        match &mut self.confirm {
            Some(c) => {
                if let Some(i) = hit(&confirm_rects(), x, y)
                    && i != c.focus
                {
                    c.focus = i;
                    self.dirty = true;
                }
            }
            None => {
                if let Some(i) = hit(&row_rects(), x, y)
                    && i != self.focus
                {
                    self.focus = i;
                    self.dirty = true;
                }
            }
        }
    }

    /// A left click at (x, y): a row or button is focused and activated.
    pub fn click(&mut self, x: i32, y: i32) -> Effect {
        let rects = if self.confirm.is_some() {
            confirm_rects()
        } else {
            row_rects()
        };
        match hit(&rects, x, y) {
            Some(i) => {
                match &mut self.confirm {
                    Some(c) => c.focus = i,
                    None => self.focus = i,
                }
                self.key(Key::Activate)
            }
            None => Effect::None,
        }
    }

    fn label(&self, item: Item) -> &'static str {
        let s = match item {
            Item::Art => Str::OptArt,
            Item::Scaling => Str::OptScaling,
            Item::Fullscreen => Str::OptFullscreen,
            Item::Volume => Str::OptVolume,
            Item::Reimport => Str::OptReimport,
            Item::Rebuild if self.pack == PackKind::Missing => Str::OptBuild,
            Item::Rebuild => Str::OptRebuild,
            Item::Close => Str::OptClose,
        };
        text(self.lang, s)
    }

    fn value(&self, item: Item) -> Option<String> {
        let t = |s| text(self.lang, s);
        let v = &self.values;
        let s = match item {
            Item::Art if self.pack != PackKind::Ready => {
                return Some(t(Str::OptArtOriginal).into());
            }
            Item::Art => match v.art {
                ArtMode::Upscaled => t(Str::OptArtUpscaled),
                ArtMode::Original => t(Str::OptArtOriginal),
            },
            Item::Scaling => match v.scaling {
                ScalingMode::Fit => t(Str::OptScaleFit),
                ScalingMode::Integer => t(Str::OptScaleInteger),
                ScalingMode::Stretch => t(Str::OptScaleStretch),
            },
            Item::Fullscreen => {
                if v.fullscreen {
                    t(Str::OptOn)
                } else {
                    t(Str::OptOff)
                }
            }
            Item::Volume => {
                return Some(format!(
                    "<  {}  >",
                    fill(t(Str::OptPercent), &[("n", &v.volume.to_string())])
                ));
            }
            Item::Reimport | Item::Rebuild | Item::Close => return None,
        };
        Some(format!("<  {s}  >"))
    }

    /// What the focused row does (and why the art row is disabled).
    fn explanation(&self, item: Item) -> (Str, [u8; 3]) {
        match item {
            Item::Art => match self.pack {
                PackKind::Ready => (Str::OptArtHelp, DIM),
                PackKind::Missing => (Str::OptArtMissing, WARN),
                PackKind::Stale => (Str::OptArtStale, WARN),
                PackKind::Unusable => (Str::OptArtUnusable, WARN),
            },
            Item::Scaling => (
                match self.values.scaling {
                    ScalingMode::Fit => Str::OptScaleFitHelp,
                    ScalingMode::Integer => Str::OptScaleIntegerHelp,
                    ScalingMode::Stretch => Str::OptScaleStretchHelp,
                },
                DIM,
            ),
            Item::Fullscreen => (Str::OptFullscreenHelp, DIM),
            Item::Volume => (Str::OptVolumeHelp, DIM),
            Item::Reimport => (
                match self.place {
                    Place::Game => Str::OptReimportHelpGame,
                    Place::Setup => Str::OptReimportHelp,
                },
                DIM,
            ),
            Item::Rebuild => (
                match self.place {
                    Place::Game => Str::OptRebuildHelpGame,
                    Place::Setup => Str::OptRebuildHelp,
                },
                if self.pack == PackKind::Stale {
                    ACCENT
                } else {
                    DIM
                },
            ),
            Item::Close => (
                match self.place {
                    Place::Game => Str::OptCloseHelpGame,
                    Place::Setup => Str::OptCloseHelp,
                },
                DIM,
            ),
        }
    }

    /// The overlay as display-list ops in 640x480, to draw over the frame underneath.
    pub fn frame(&self, font: &Font) -> Frame {
        let t = |s| text(self.lang, s);
        let mut f = Frame::new();
        f.push(DrawOp::Fill {
            rect: PANEL,
            color: PANEL_EDGE,
        });
        f.push(DrawOp::Fill {
            rect: Rect::new(
                PANEL.left + 1,
                PANEL.top + 1,
                PANEL.right - 1,
                PANEL.bottom - 1,
            ),
            color: PANEL_BG,
        });
        let title = match &self.confirm {
            Some(_) => t(Str::OptConfirmTitle),
            None => t(Str::OptTitle),
        };
        f.push(text_op(
            title,
            TITLE_FONT,
            ACCENT,
            Rect::new(LEFT, 42, RIGHT, 74),
        ));
        f.push(DrawOp::Fill {
            rect: Rect::new(LEFT, 76, RIGHT, 78),
            color: ACCENT,
        });
        if self.place == Place::Game {
            f.push(text_op(
                t(Str::OptPaused),
                SMALL_FONT,
                DIM,
                Rect::new(LEFT, 82, RIGHT, 100),
            ));
        }
        match &self.confirm {
            Some(c) => self.confirm_ops(&mut f, font, c),
            None => self.rows_ops(&mut f, font),
        }
        f
    }

    fn rows_ops(&self, f: &mut Frame, font: &Font) {
        let rows = row_rects();
        for (i, (item, r)) in ITEMS.iter().zip(&rows).enumerate() {
            let focused = i == self.focus;
            let disabled = *item == Item::Art && !self.art_enabled();
            push_button(f, *r, focused);
            let color = if focused {
                FOCUS_TEXT
            } else if disabled {
                DIM
            } else {
                TEXT
            };
            let top = r.top + (r.height() - cell(font, BODY_FONT)) / 2;
            match self.value(*item) {
                Some(v) => {
                    f.push(text_op(
                        self.label(*item),
                        BODY_FONT,
                        color,
                        Rect::new(r.left + 12, top, r.right - VALUE_W - 8, r.bottom),
                    ));
                    f.push(DrawOp::Text {
                        text: v,
                        font: BODY_FONT,
                        color,
                        rect: Rect::new(r.right - VALUE_W, r.top, r.right - 8, r.bottom),
                        layout: TextLayout::SingleLineCentered,
                    });
                }
                None => f.push(text_op(
                    self.label(*item),
                    BODY_FONT,
                    color,
                    Rect::new(r.left + 12, top, r.right - 8, r.bottom),
                )),
            }
        }
        let (s, color) = self.explanation(ITEMS[self.focus]);
        let top = rows.last().map_or(HELP_TOP, |r| r.bottom + 10);
        let help = text(self.lang, s);
        let bottom = top + lines_height(font, help, SMALL_FONT, RIGHT - LEFT);
        f.push(text_op(
            help,
            SMALL_FONT,
            color,
            Rect::new(LEFT, top, RIGHT, bottom),
        ));
        f.push(DrawOp::Text {
            text: text(self.lang, Str::OptHint).into(),
            font: SMALL_FONT,
            color: DIM,
            rect: Rect::new(LEFT, HINT_TOP, RIGHT, HINT_TOP + 18),
            layout: TextLayout::SingleLineCentered,
        });
    }

    fn confirm_ops(&self, f: &mut Frame, font: &Font, c: &Confirm) {
        let t = |s| text(self.lang, s);
        let body = match c.action {
            Item::Reimport => t(Str::OptConfirmReimport),
            _ => t(Str::OptConfirmRebuild),
        };
        let mut y = 108;
        for (s, color) in [(body, TEXT), (t(Str::OptConfirmEnds), WARN)] {
            let bottom = y + lines_height(font, s, BODY_FONT, RIGHT - LEFT);
            f.push(text_op(
                s,
                BODY_FONT,
                color,
                Rect::new(LEFT, y, RIGHT, bottom),
            ));
            y = bottom + 10;
        }
        let labels = [t(Str::OptConfirmYes), t(Str::BtnBack)];
        for (i, (r, label)) in confirm_rects().iter().zip(labels).enumerate() {
            let focused = i == c.focus;
            push_button(f, *r, focused);
            f.push(DrawOp::Text {
                text: label.into(),
                font: BODY_FONT,
                color: if focused { FOCUS_TEXT } else { TEXT },
                rect: *r,
                layout: TextLayout::SingleLineCentered,
            });
        }
        f.push(DrawOp::Text {
            text: t(Str::HintBack).into(),
            font: SMALL_FONT,
            color: DIM,
            rect: Rect::new(LEFT, HINT_TOP, RIGHT, HINT_TOP + 18),
            layout: TextLayout::SingleLineCentered,
        });
    }
}

/// The overlay panel (centred, leaving the game visible around it).
pub const PANEL: Rect = Rect::new(64, 28, 576, 452);
const LEFT: i32 = PANEL.left + 24;
const RIGHT: i32 = PANEL.right - 24;
const ROW_TOP: i32 = 106;
const ROW_H: i32 = 30;
const ROW_GAP: i32 = 4;
const VALUE_W: i32 = 236;
const HELP_TOP: i32 = 350;
const HINT_TOP: i32 = PANEL.bottom - 26;
const FOCUS_TEXT: [u8; 3] = [20, 20, 28];
const BUTTON_BG: [u8; 3] = [46, 48, 68];
const TITLE_FONT: FontSpec = FontSpec {
    height: 28,
    width: 0,
};
const BODY_FONT: FontSpec = FontSpec {
    height: 18,
    width: 0,
};
const SMALL_FONT: FontSpec = FontSpec {
    height: 15,
    width: 0,
};

/// One rect per [`Item`], top to bottom.
pub fn row_rects() -> Vec<Rect> {
    (0..ITEMS.len() as i32)
        .map(|i| {
            let top = ROW_TOP + i * (ROW_H + ROW_GAP);
            Rect::new(LEFT, top, RIGHT, top + ROW_H)
        })
        .collect()
}

/// The confirmation's two buttons (confirm, back).
pub fn confirm_rects() -> Vec<Rect> {
    let w = 400;
    let left = (PANEL.left + PANEL.right - w) / 2;
    (0..2)
        .map(|i| {
            let bottom = HINT_TOP - 12 - (1 - i) * (ROW_H + 8);
            Rect::new(left, bottom - ROW_H, left + w, bottom)
        })
        .collect()
}

fn hit(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects.iter().position(|r| r.contains(x, y))
}

fn push_button(f: &mut Frame, r: Rect, focused: bool) {
    if focused {
        f.push(DrawOp::Fill {
            rect: r,
            color: ACCENT,
        });
    } else {
        f.push(DrawOp::Fill {
            rect: r,
            color: PANEL_EDGE,
        });
        f.push(DrawOp::Fill {
            rect: Rect::new(r.left + 1, r.top + 1, r.right - 1, r.bottom - 1),
            color: BUTTON_BG,
        });
    }
}

fn text_op(s: &str, font: FontSpec, color: [u8; 3], rect: Rect) -> DrawOp {
    DrawOp::Text {
        text: s.to_string(),
        font,
        color,
        rect,
        layout: TextLayout::WrappedLeft,
    }
}

fn cell(font: &Font, spec: FontSpec) -> i32 {
    layout_text(
        font,
        "Ag",
        spec,
        Rect::new(0, 0, 600, 100),
        TextLayout::WrappedLeft,
    )
    .scale
    .cell_height()
}

fn lines_height(font: &Font, s: &str, spec: FontSpec, width: i32) -> i32 {
    let laid = layout_text(
        font,
        s,
        spec,
        Rect::new(0, 0, width, i32::MAX / 2),
        TextLayout::WrappedLeft,
    );
    laid.lines.len() as i32 * laid.scale.cell_height()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            art: ArtMode::Upscaled,
            scaling: ScalingMode::Fit,
            fullscreen: false,
            volume: 100,
        }
    }

    fn center(r: Rect) -> (i32, i32) {
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    #[test]
    fn options_state_machine() {
        let mut o = Overlay::new(Lang::Cs, Place::Game, PackKind::Ready, values());
        assert_eq!(o.focused(), Item::Art);
        // Art switches only with a ready pack.
        assert_eq!(o.key(Key::Right), Effect::Art(ArtMode::Original));
        assert_eq!(o.key(Key::Activate), Effect::Art(ArtMode::Upscaled));
        // Scaling cycles both ways.
        assert_eq!(o.key(Key::Down), Effect::None);
        assert_eq!(o.key(Key::Right), Effect::Scaling(ScalingMode::Integer));
        assert_eq!(o.key(Key::Right), Effect::Scaling(ScalingMode::Stretch));
        assert_eq!(o.key(Key::Right), Effect::Scaling(ScalingMode::Fit));
        assert_eq!(o.key(Key::Left), Effect::Scaling(ScalingMode::Stretch));
        o.key(Key::Down);
        assert_eq!(o.key(Key::Activate), Effect::Fullscreen(true));
        assert_eq!(o.key(Key::Left), Effect::Fullscreen(false));
        // Volume in steps, clamped.
        o.key(Key::Down);
        assert_eq!(o.focused(), Item::Volume);
        assert_eq!(o.key(Key::Right), Effect::None, "already at 100");
        assert_eq!(o.key(Key::Left), Effect::Volume(90));
        for _ in 0..12 {
            o.key(Key::Left);
        }
        assert_eq!(o.key(Key::Left), Effect::None, "stays at 0");
        assert_eq!(o.key(Key::Right), Effect::Volume(10));
        // Re-import in the game: confirm first; Back keeps the game.
        o.key(Key::Down);
        assert_eq!(o.focused(), Item::Reimport);
        assert_eq!(o.key(Key::Left), Effect::None);
        assert_eq!(o.key(Key::Activate), Effect::None);
        assert!(o.confirming());
        assert_eq!(o.key(Key::Activate), Effect::None, "focus starts on Back");
        assert!(!o.confirming());
        o.key(Key::Activate);
        assert_eq!(o.key(Key::Back), Effect::None);
        assert!(
            !o.confirming(),
            "Esc leaves the confirmation, not the overlay"
        );
        o.key(Key::Activate);
        o.key(Key::Up);
        assert_eq!(o.key(Key::Activate), Effect::Reimport);
        // Rebuild, confirmed.
        o.key(Key::Down);
        assert_eq!(o.focused(), Item::Rebuild);
        o.key(Key::Activate);
        o.key(Key::Down);
        assert_eq!(o.key(Key::Activate), Effect::Rebuild);
        // Close, wrap-around, Esc.
        o.key(Key::Down);
        assert_eq!(o.key(Key::Activate), Effect::Close);
        o.key(Key::Down);
        assert_eq!(o.focused(), Item::Art, "focus wraps");
        o.key(Key::Up);
        assert_eq!(o.focused(), Item::Close);
        assert_eq!(o.key(Key::Back), Effect::Close);

        // Without a ready pack the art row explains why and changes nothing.
        for kind in [PackKind::Missing, PackKind::Stale, PackKind::Unusable] {
            let mut o = Overlay::new(Lang::En, Place::Game, kind, values());
            assert!(!o.art_enabled());
            assert_eq!(o.key(Key::Right), Effect::None);
            assert_eq!(o.key(Key::Activate), Effect::None);
            assert_eq!(o.explanation(Item::Art).1, WARN);
        }
        // The stale pack: the rebuild row is highlighted in its help.
        let o = Overlay::new(Lang::Cs, Place::Game, PackKind::Stale, values());
        assert_eq!(o.explanation(Item::Rebuild).1, ACCENT);

        // Over the wizard there is no game to end: no confirmation.
        let mut o = Overlay::new(Lang::Cs, Place::Setup, PackKind::Missing, values());
        for _ in 0..4 {
            o.key(Key::Down);
        }
        assert_eq!(o.key(Key::Activate), Effect::Reimport);
        o.key(Key::Down);
        assert_eq!(o.key(Key::Activate), Effect::Rebuild);

        // Mouse: hover focuses, a click activates.
        let mut o = Overlay::new(Lang::Cs, Place::Game, PackKind::Ready, values());
        let rows = row_rects();
        let (x, y) = center(rows[2]);
        o.pointer(x, y);
        assert_eq!(o.focused(), Item::Fullscreen);
        assert_eq!(o.click(x, y), Effect::Fullscreen(true));
        assert_eq!(o.click(2, 2), Effect::None);
        let (x, y) = center(rows[4]);
        assert_eq!(o.click(x, y), Effect::None);
        assert!(o.confirming());
        let (x, y) = center(confirm_rects()[0]);
        assert_eq!(o.click(x, y), Effect::Reimport);
        // Values changed elsewhere (F11) are shown.
        let mut o = Overlay::new(Lang::Cs, Place::Game, PackKind::Ready, values());
        o.take_dirty();
        o.set_values(values());
        assert!(!o.take_dirty());
        o.set_values(Values {
            fullscreen: true,
            ..values()
        });
        assert!(o.take_dirty());
        assert_eq!(o.value(Item::Fullscreen).unwrap(), "<  Zapnuto  >");
    }

    /// Every state in both languages: every text inside its rect and the panel, the
    /// help never runs into the hint.
    #[test]
    fn overlay_renders_inside_its_panel() {
        let font = Font::bundled();
        let mut states = Vec::new();
        for lang in [Lang::Cs, Lang::En] {
            for place in [Place::Game, Place::Setup] {
                for pack in [
                    PackKind::Ready,
                    PackKind::Missing,
                    PackKind::Stale,
                    PackKind::Unusable,
                ] {
                    for scaling in [ScalingMode::Fit, ScalingMode::Integer, ScalingMode::Stretch] {
                        let v = Values {
                            scaling,
                            ..values()
                        };
                        for (focus, item) in ITEMS.iter().enumerate() {
                            let mut o = Overlay::new(lang, place, pack, v);
                            o.focus = focus;
                            states.push(o.clone());
                            if matches!(item, Item::Reimport | Item::Rebuild)
                                && place == Place::Game
                            {
                                o.key(Key::Activate);
                                states.push(o);
                            }
                        }
                    }
                }
            }
        }
        for o in &states {
            let frame = o.frame(&font);
            for op in &frame.ops {
                let DrawOp::Text {
                    text,
                    font: spec,
                    rect,
                    layout,
                    ..
                } = op
                else {
                    continue;
                };
                let laid = layout_text(&font, text, *spec, *rect, *layout);
                let c = laid.scale.cell_height();
                assert!(!laid.lines.is_empty(), "{text}");
                for l in &laid.lines {
                    let inside = |r: &Rect| {
                        l.x >= r.left
                            && l.x + l.width <= r.right
                            && l.y >= r.top
                            && l.y + c <= r.bottom
                    };
                    assert!(
                        inside(rect),
                        "{:?} {:?}: {:?} outside {rect:?}",
                        o.lang,
                        o.focus,
                        l.text
                    );
                    assert!(
                        inside(&PANEL),
                        "{:?}: {:?} outside the panel",
                        o.lang,
                        l.text
                    );
                    if *layout == TextLayout::WrappedLeft {
                        assert!(
                            l.y + c <= HINT_TOP,
                            "{:?}: {:?} over the hint",
                            o.lang,
                            l.text
                        );
                    }
                }
            }
        }
    }
}
