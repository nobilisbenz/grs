use thiserror::Error;

/// The single error type for the `grs-lib` engine.
///
/// Variants carry enough information for the CLI's `CommandError` to attach
/// user-facing hints (see `grs/src/command_error.rs`). The CLI maps each
/// variant to a kind + hint in exactly one place.
#[derive(Debug, Error)]
pub enum GrsError {
    #[error("no .grs/ repository found here")]
    NotInitialized,

    #[error("grs is already initialized in this directory")]
    AlreadyInitialized,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("snap io error: {0}")]
    SnapIo(std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("ignore error: {0}")]
    Ignore(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("ambiguous id \"{0}\" matched multiple sessions")]
    AmbiguousId(String),

    #[error("storage version {0} not supported by this grs build")]
    UnsupportedVersion(u32),

    #[error("session {0} is open; close it (e.g. `grs new`) before deleting")]
    SessionOpen(crate::ulid::SessionId),
}

pub type Result<T, E = GrsError> = std::result::Result<T, E>;
