//! grs-lib — the engine for grs.
//!
//! Pure library crate with zero terminal deps (no clap/ratatui/crossterm).
//! The CLI/TUI crate (`grs`) is a thin orchestration layer over this.
//!
//! See `plan/01-architecture.md` for the module tree.

pub mod config;
pub mod diff;
pub mod error;
pub mod ignore;
pub mod model;
pub mod paths;
pub mod session;
pub mod snap;
pub mod store;
pub mod ulid;
pub mod util;
pub mod watcher;

pub use error::{GrsError, Result};
pub use ulid::SessionId;
