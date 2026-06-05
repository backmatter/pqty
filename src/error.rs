use std::io;
use std::path::PathBuf;

/// Error returned by pqty library operations.
#[derive(Debug)]
pub enum PqtyError {
    /// A filesystem or stream operation failed.
    Io {
        /// Path associated with the failed operation.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A JSON artifact could not be serialized or deserialized.
    Json(serde_json::Error),
    /// A TOML configuration document could not be parsed.
    Toml {
        /// Path of the invalid configuration document.
        path: PathBuf,
        /// Underlying TOML decoding error.
        source: Box<toml::de::Error>,
    },
    /// Input, protocol, integrity, or command usage was invalid.
    Usage(String),
}

impl std::fmt::Display for PqtyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Toml { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Usage(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for PqtyError {}

impl From<serde_json::Error> for PqtyError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
