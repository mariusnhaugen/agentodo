use std::path::PathBuf;

/// Errors produced by the core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no todo found with id {0}")]
    NotFound(u64),

    #[error("could not determine a data directory for the store")]
    NoDataDir,

    #[error("failed to access store at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse store at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to serialize store: {0}")]
    Serialize(#[source] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
