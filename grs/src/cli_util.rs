//! `CliRunner` builder + `CommandHelper`. Mirrors jj's entry flow: parse args
//! → build Ui → resolve repo root → build helper → dispatch → map to exit code.

use crate::commands::{run_command, Args};
use crate::command_error::CommandError;
use crate::ui::Ui;
use clap::CommandFactory;
use grs_lib::config::Config;
use grs_lib::error::GrsError;
use grs_lib::store::RepoStore;
use std::path::{Path, PathBuf};

/// A cheaply-cloneable handle giving commands access to cwd/global args/config
/// and the `RepoStore`. Each `store()` call opens fresh (it's just a path walk
/// + a small config read) — keeps the helper dead simple.
pub struct CommandHelper {
    cwd: PathBuf,
    global: crate::GlobalArgs,
    root: Option<PathBuf>,
}

impl CommandHelper {
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn global(&self) -> &crate::GlobalArgs {
        &self.global
    }

    /// The resolved repo root, if any (from `--repo` or by walking up from cwd).
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// User-level config (always available, even outside a repo).
    pub fn user_config(&self) -> Config {
        Config::load_user()
    }

    /// Open the existing repo at the resolved root. Errors if not in a repo.
    pub fn store(&self) -> Result<RepoStore, GrsError> {
        let root = self.root.as_ref().ok_or(GrsError::NotInitialized)?;
        RepoStore::open(root)
    }

    /// Open the store if a repo is resolved; `None` otherwise.
    pub fn try_store(&self) -> Option<RepoStore> {
        self.root.as_ref().and_then(|r| RepoStore::open(r).ok())
    }

    /// Open the store, auto-initializing the repo at the resolved root if it
    /// doesn't exist yet. Used by the TUI entry points (`grs`, `grs tui`,
    /// `grs replay`) so users can just start the TUI in any directory and
    /// capture begins immediately.
    pub fn store_or_init(&self) -> Result<RepoStore, GrsError> {
        if let Some(root) = &self.root {
            return RepoStore::open_or_init(root);
        }
        // No root resolved yet: try cwd, then walk up; if still nothing, init
        // in cwd.
        let cwd = self.cwd();
        if let Some(r) = grs_lib::paths::try_find_grs_root(cwd) {
            return RepoStore::open_or_init(&r);
        }
        RepoStore::init(cwd)
    }
}

pub struct CliRunner {
    app: clap::Command,
    version: String,
}

impl CliRunner {
    pub fn init() -> Self {
        Self {
            app: Args::command(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn version(mut self, v: impl Into<String>) -> Self {
        let v = v.into();
        self.app = self.app.version(v.clone());
        self.version = v;
        self
    }

    /// Parse args, build Ui/helper, dispatch, map to an exit code.
    pub fn run(self) -> u8 {
        use clap::Parser;

        let args = match Args::try_parse() {
            Ok(a) => a,
            Err(e) => {
                // clap handles --help/--version printing; render & exit.
                e.exit();
            }
        };

        let quiet = args.global_args.quiet;
        let color = args.global_args.color.unwrap_or_default();
        let mut ui = Ui::new(quiet, color);

        if args.global_args.verbose {
            init_tracing();
        }

        let cwd = current_dir();
        let root = resolve_root(&cwd, args.global_args.repo.as_deref());
        let helper = CommandHelper {
            cwd,
            global: args.global_args.clone(),
            root,
        };

        // All commands run on the tokio multi-thread runtime. Sub-commands
        // are short-lived sync IO but use the universal `async fn cmd_*`
        // signature; the TUI uses the same runtime for any async work it
        // needs.
        let result = std::thread::scope(|s| {
            s.spawn(|| -> std::result::Result<(), CommandError> {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => return Err(CommandError::internal_error(e)),
                };
                runtime.block_on(run_command(&mut ui, &helper, &args))
            })
            .join()
            .unwrap_or_else(|_| {
                Err(CommandError::user_error("command thread panicked"))
            })
        });

        match result {
            Ok(()) => 0,
            Err(e) => {
                e.print(&mut ui);
                e.exit_code()
            }
        }
    }
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Resolve the repo root: `--repo` wins; else walk up from cwd for `.grs/`.
fn resolve_root(cwd: &Path, repo: Option<&Path>) -> Option<PathBuf> {
    if let Some(r) = repo {
        let canon = std::fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf());
        return Some(canon);
    }
    grs_lib::paths::try_find_grs_root(cwd)
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Entry used by `main.rs`.
pub fn run() -> u8 {
    CliRunner::init().version(env!("CARGO_PKG_VERSION")).run()
}
