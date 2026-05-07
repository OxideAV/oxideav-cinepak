//! Crate-local error type.
//!
//! Mirrors the standalone-friendly pattern used elsewhere in the
//! workspace (e.g. `oxideav-mjpeg`). The `registry` feature wires
//! `From<CinepakError> for oxideav_core::Error` in `crate::registry`.

/// Errors emitted by the Cinepak decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CinepakError {
    /// The bitstream did not parse — header field out of range, chunk
    /// size accounting mismatch, vector chunk truncated, etc.
    InvalidData(String),
    /// A wire-format construct is wire-legal but not implemented by
    /// this decoder.
    Unsupported(String),
    /// Catch-all for context that is neither parse-error nor genuinely
    /// unsupported.
    Other(String),
}

impl CinepakError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

impl core::fmt::Display for CinepakError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "cinepak: invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "cinepak: unsupported: {s}"),
            Self::Other(s) => write!(f, "cinepak: {s}"),
        }
    }
}

impl std::error::Error for CinepakError {}

pub type Result<T> = core::result::Result<T, CinepakError>;
