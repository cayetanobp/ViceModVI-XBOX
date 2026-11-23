use std::{fmt, io};

/// Errors reported to the CLI and the viewer. The message is the text shown
/// to the person.
#[derive(Debug)]
pub enum Error {
    /// Malformed, truncated or mismatched data.
    InvalidData(String),
    /// A valid request that this build cannot carry out.
    NotSupported(String),
    /// A bad argument or option.
    Argument(String),
    Io(io::Error),
    Json(serde_json::Error),
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidData(message.into())
    }

    pub fn not_supported(message: impl Into<String>) -> Self {
        Self::NotSupported(message.into())
    }

    pub fn argument(message: impl Into<String>) -> Self {
        Self::Argument(message.into())
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Io(io::Error::other(message.into()))
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(m) | Self::NotSupported(m) | Self::Argument(m) => f.write_str(m),
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "Invalid JSON: {error}"),
            Self::Cancelled => f.write_str("The operation was canceled."),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<image::ImageError> for Error {
    fn from(error: image::ImageError) -> Self {
        Self::InvalidData(error.to_string())
    }
}
