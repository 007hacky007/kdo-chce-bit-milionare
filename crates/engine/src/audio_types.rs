//! Audio command types sent from the engine to the host.

use std::fmt;

/// Archive name prefix of a sound (F7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Prefix {
    Sfex,
    Char,
    Musi,
}

impl Prefix {
    pub const fn as_str(self) -> &'static str {
        match self {
            Prefix::Sfex => "SFEX",
            Prefix::Char => "CHAR",
            Prefix::Musi => "MUSI",
        }
    }
}

/// A sound in `Audio1.dat` / `Audio2.dat`, e.g. `SFEX4057`, `CHAR0012q`, `MUSI0010`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SoundId {
    pub prefix: Prefix,
    pub number: u16,
    pub suffix: Option<char>,
}

impl SoundId {
    pub const fn new(prefix: Prefix, number: u16) -> Self {
        Self {
            prefix,
            number,
            suffix: None,
        }
    }

    pub const fn with_suffix(prefix: Prefix, number: u16, suffix: char) -> Self {
        Self {
            prefix,
            number,
            suffix: Some(suffix),
        }
    }
}

impl fmt::Display for SoundId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:04}", self.prefix.as_str(), self.number)?;
        if let Some(c) = self.suffix {
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Mixer channel (F7.3): 1 sfx, 2 voice (queued), 3 music (one looping track).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    Sfx,
    Voice,
    Music,
}

/// Commands the engine sends to the host's audio side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioCmd {
    Play {
        id: SoundId,
        channel: Channel,
        looped: bool,
    },
    /// Append to the voice queue (up to 10 ids); the next clip starts when the current ends.
    QueueVoice(SoundId),
    /// Clear the voice queue and stop channel 2.
    StopVoice,
    StopChannel(Channel),
    /// Stop the clip with this id wherever it plays (`StopSound` 0x40b6d0).
    StopSound(SoundId),
    /// Original volume scale, 100 = 0 dB.
    SetVolume {
        channel: Channel,
        vol: u8,
    },
    PauseChannel(Channel, bool),
    /// Interleaved PCM of a cutscene chunk: samples, rate, channel count.
    CutsceneAudio(Vec<i16>, u32, u16),
    /// Drop all buffered cutscene audio (cutscene ended or was skipped).
    StopCutsceneAudio,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_id_display_matches_archive_names() {
        assert_eq!(SoundId::new(Prefix::Sfex, 4057).to_string(), "SFEX4057");
        assert_eq!(
            SoundId::with_suffix(Prefix::Char, 12, 'q').to_string(),
            "CHAR0012q"
        );
        assert_eq!(SoundId::new(Prefix::Musi, 10).to_string(), "MUSI0010");
    }
}
