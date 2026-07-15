use thiserror::Error;

/// The single error type for the `grs-lib` engine.
#[derive(Debug, Error)]
pub enum GrsError {
    #[error("no .grs/ repository found here")]
    NotInitialized,

    #[error("grs is already initialized in this directory")]
    AlreadyInitialized,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),

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

    #[error("session name \"{0}\" is already in use")]
    NameInUse(String),

    #[error("session {0} is open; close it (e.g. quit the TUI) before deleting")]
    SessionOpen(crate::ulid::SessionId),

    #[error("a TUI is already running for this project (lock: {0})")]
    AlreadyRunning(String),

    #[error("invalid session name: {0}")]
    InvalidName(String),
}

pub type Result<T, E = GrsError> = std::result::Result<T, E>;
