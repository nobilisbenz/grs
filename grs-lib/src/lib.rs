//! grs-lib — the engine for grs.
//!
//! Pure library crate with zero terminal deps (no clap/ratatui/crossterm).
//! The CLI/TUI crate (`grs`) is a thin orchestration layer over this.
//!
//! # Data model
//!
//! - **Session** = one user-named observation period. Lifetime = TUI process.
//!   Folder: `<project>/.grs/sessions/<slug>_<ulid>/`. `meta.toml` holds
//!   the `SessionMeta`.
//! - **Snap** = one whole project tree, captured at a moment in time.
//!   Folder: `<session>/snap-N/`. Contains a `meta.toml` (`SnapMeta`) and
//!   a copy of every tracked file.
//! - **Diff** = always consecutive (snap N vs snap N-1). Computed on
//!   read by walking two snap dirs. Rename detection by SHA-256.

pub mod config;
pub mod diff;
pub mod error;
pub mod ignore;
pub mod lockfile;
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
