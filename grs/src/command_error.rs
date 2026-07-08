//! `CommandError` — one error type with kind + hints, mirroring jj's pattern.
//! `From<GrsError>` maps each engine variant to a kind + maybe a hint.

use grs_lib::error::GrsError;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandErrorKind {
    User,
    Config,
    Cli,
    BrokenPipe,
    Internal,
}

pub struct CommandError {
    pub kind: CommandErrorKind,
    pub error: Arc<dyn std::error::Error + Send + Sync>,
    pub hints: Vec<String>,
}

impl CommandError {
    pub fn new(kind: CommandErrorKind, error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            error: Arc::from(Box::new(error) as Box<dyn std::error::Error + Send + Sync>),
            hints: Vec::new(),
        }
    }

    pub fn user_error(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        Self::new(CommandErrorKind::User, SimpleError(msg))
    }

    pub fn user_error_with_message(
        msg: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::new(CommandErrorKind::User, WithMessage(msg.into(), Box::new(source)))
    }

    pub fn internal_error(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::new(CommandErrorKind::Internal, source)
    }

    pub fn cli_error(msg: impl Into<String>) -> Self {
        Self::new(CommandErrorKind::Cli, SimpleError(msg.into()))
    }

    pub fn hinted(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }

    /// Print the error + hints to stderr (called by `CliRunner`).
    pub fn print(&self, ui: &mut crate::ui::Ui) {
        let _ = ui;
        match self.kind {
            CommandErrorKind::BrokenPipe => {}
            _ => {
                eprintln!("Error: {}", self.error);
                for hint in &self.hints {
                    eprintln!("Hint: {hint}");
                }
            }
        }
    }

    /// Exit code for this kind (jj's convention).
    pub fn exit_code(&self) -> u8 {
        match self.kind {
            CommandErrorKind::User | CommandErrorKind::Config => 1,
            CommandErrorKind::Cli => 2,
            CommandErrorKind::BrokenPipe => 0,
            CommandErrorKind::Internal => 255,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::fmt::Debug for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandError")
            .field("kind", &self.kind)
            .field("error", &self.error.to_string())
            .field("hints", &self.hints)
            .finish()
    }
}

impl From<GrsError> for CommandError {
    fn from(e: GrsError) -> Self {
        match e {
            GrsError::NotInitialized => {
                CommandError::user_error("No .grs/ repository found here")
                    .hinted("Run `grs` to open the TUI; it auto-initializes the repo on first run.")
            }
            GrsError::AlreadyInitialized => {
                CommandError::user_error("This directory is already a grs repo")
                    .hinted("It's already initialized; just run `grs` to open the TUI.")
            }
            GrsError::NotFound(s) => CommandError::user_error(s),
            GrsError::AmbiguousId(s) => CommandError::user_error(format!(
                "id \"{s}\" matched multiple sessions"
            ))
            .hinted("Use more characters of the ULID to disambiguate."),
            GrsError::Config(s) => CommandError::user_error(s),
            GrsError::Ignore(s) => CommandError::user_error(s),
            GrsError::UnsupportedVersion(v) => CommandError::user_error(format!(
                "grs storage version {v} is not supported by this build"
            ))
            .hinted("Upgrade grs."),
            GrsError::Io(e) | GrsError::SnapIo(e) => {
                CommandError::user_error_with_message("io error", e)
            }
            GrsError::Json(e) => CommandError::user_error_with_message("json error", e),
        }
    }
}

#[derive(Debug)]
struct SimpleError(String);
impl std::fmt::Display for SimpleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SimpleError {}

#[derive(Debug)]
struct WithMessage(String, Box<dyn std::error::Error + Send + Sync>);
impl std::fmt::Display for WithMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0, self.1)
    }
}
impl std::error::Error for WithMessage {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.1.as_ref())
    }
}
