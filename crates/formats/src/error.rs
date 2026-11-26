use thiserror::Error;

/// Errors produced while decoding original game data.
#[derive(Debug, Error)]
pub enum FormatError {
    #[error("truncated {what} at offset {offset}")]
    Truncated { what: &'static str, offset: u64 },
    #[error("invalid {what}: {detail}")]
    Invalid { what: &'static str, detail: String },
    /// The underlying reader failed (not end of data), e.g. a disc read error.
    #[error("read error in {what} at offset {offset}: {source}")]
    Io {
        what: &'static str,
        offset: u64,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, FormatError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_names_offset() {
        let s = FormatError::Truncated {
            what: "smacker header",
            offset: 12,
        }
        .to_string();
        assert!(s.contains("smacker header"));
        assert!(s.contains("12"));
    }
}
