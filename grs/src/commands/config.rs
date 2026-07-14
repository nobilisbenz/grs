//! `grs config` — view and edit `grs`'s layered TOML config.
//!
//! Two layers, with the repo-level (`.grs/config.toml`) overriding user-level
//! (`~/.config/grs/config.toml`):
//!
//! ```text
//! grs config show                        # show effective (merged) config
//! grs config show --layer user           # just the user config file
//! grs config get watcher.debounce_ms     # effective value
//! grs config set watcher.debounce_ms 250 # set in user config
//! grs config unset watcher.debounce_ms   # remove from user config
//! grs config path                       # print path to each config file
//! grs config edit                       # $EDITOR the user config
//! ```
//!
//! `set`/`unset` use `toml_edit` so comments and formatting are preserved.

use crate::cli_util::CommandHelper;
use crate::command_error::CommandError;
use crate::ui::Ui;
use grs_lib::config::Config;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(clap::Args, Clone, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(clap::Subcommand, Clone, Debug)]
pub enum ConfigCommand {
    /// Print the effective merged config (or a specific layer with --layer).
    Show {
        /// Show a specific config layer instead of the merged effective view.
        #[arg(long, value_enum)]
        layer: Option<ConfigLayerArg>,
    },
    /// Print the value of one key (dotted path: e.g. `watcher.debounce_ms`).
    Get {
        /// Dotted key path, e.g. `watcher.debounce_ms`.
        key: String,
    },
    /// Set a value (dotted key) in the user-level config.
    Set {
        /// Dotted key path, e.g. `watcher.debounce_ms`.
        key: String,
        /// Value as a string; parsed as TOML (so `250` is an int, `"x"` a string).
        value: String,
    },
    /// Remove a key from the user-level config.
    Unset {
        /// Dotted key path.
        key: String,
    },
    /// Print the path(s) to the active config files.
    Path,
    /// Open the user config in $EDITOR.
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ConfigLayerArg {
    User,
    Repo,
    Effective,
}

pub async fn cmd_config(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &ConfigArgs,
) -> Result<(), CommandError> {
    match &args.command {
        ConfigCommand::Show { layer } => {
            let layer = layer.unwrap_or(ConfigLayerArg::Effective);
            let out = match layer {
                ConfigLayerArg::User => show_user_config(),
                ConfigLayerArg::Repo => show_repo_config(command)?,
                ConfigLayerArg::Effective => show_effective(command),
            };
            ui.stdout().write_all(out.as_bytes()).map_err(CommandError::internal_error)?;
        }
        ConfigCommand::Get { key } => {
            let cfg = match command.try_store() {
                Some(s) => s.config().clone(),
                None => Config::load_user(),
            };
            let out = get_key(&cfg, key)?;
            ui.stdout().write_all(out.as_bytes()).map_err(CommandError::internal_error)?;
        }
        ConfigCommand::Set { key, value } => {
            let path = user_config_path()
                .ok_or_else(|| CommandError::user_error("cannot determine user config path (no HOME)"))?;
            let mut doc = load_or_default(&path)?;
            set_key(&mut doc, key, value)?;
            write_atomic(&path, &doc.to_string())?;
            ui.status(&format!("Set {key} = {value} in {}", path.display()))
                .map_err(CommandError::internal_error)?;
        }
        ConfigCommand::Unset { key } => {
            let path = user_config_path()
                .ok_or_else(|| CommandError::user_error("cannot determine user config path (no HOME)"))?;
            if !path.exists() {
                return Err(CommandError::user_error(format!(
                    "user config does not exist: {}",
                    path.display()
                )));
            }
            let mut doc = load_or_default(&path)?;
            unset_key(&mut doc, key)?;
            write_atomic(&path, &doc.to_string())?;
            ui.status(&format!("Removed {key} from {}", path.display()))
                .map_err(CommandError::internal_error)?;
        }
        ConfigCommand::Path => {
            let mut out = String::new();
            if let Some(p) = user_config_path() {
                out.push_str(&format!("user: {}\n", p.display()));
            } else {
                out.push_str("user: (no HOME — unset)\n");
            }
            if let Some(root) = command.root() {
                let repo_cfg = root.join(".grs").join("config.toml");
                out.push_str(&format!("repo: {}\n", repo_cfg.display()));
            } else {
                out.push_str("repo: (not in a grs repo)\n");
            }
            ui.stdout().write_all(out.as_bytes()).map_err(CommandError::internal_error)?;
        }
        ConfigCommand::Edit => {
            let path = user_config_path()
                .ok_or_else(|| CommandError::user_error("cannot determine user config path (no HOME)"))?;
            // Ensure the file exists.
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(CommandError::internal_error)?;
                }
                std::fs::write(&path, Config::default_toml())
                    .map_err(CommandError::internal_error)?;
            }
            let editor = std::env::var("VISUAL")
                .ok()
                .or_else(|| std::env::var("EDITOR").ok())
                .ok_or_else(|| {
                    CommandError::user_error("set $VISUAL or $EDITOR to use `grs config edit`")
                })?;
            let status = std::process::Command::new(editor)
                .arg(&path)
                .status()
                .map_err(CommandError::internal_error)?;
            if !status.success() {
                return Err(CommandError::user_error(format!(
                    "editor exited with status {status}"
                )));
            }
        }
    }
    Ok(())
}

// --- helpers ---

fn show_user_config() -> String {
    match user_config_path() {
        Some(p) if p.exists() => match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => format!("# error reading {}: {e}\n", p.display()),
        },
        Some(p) => format!("# {} does not exist (using defaults)\n", p.display()),
        None => "# no HOME set; using defaults\n".to_string(),
    }
}

fn show_repo_config(command: &CommandHelper) -> Result<String, CommandError> {
    match command.root() {
            Some(root) => {
                let p = root.join(".grs").join("config.toml");
                if p.exists() {
                    std::fs::read_to_string(&p).map_err(CommandError::internal_error)
                } else {
                    Ok(format!(
                        "# {} does not exist (using user + defaults)\n",
                        p.display()
                    ))
                }
            }
        None => Err(CommandError::user_error("not in a grs repo")),
    }
}

fn show_effective(command: &CommandHelper) -> String {
    let cfg = match command.try_store() {
        Some(s) => s.config().clone(),
        None => Config::load_user(),
    };
    toml::to_string_pretty(&cfg).unwrap_or_else(|e| format!("# error: {e}\n")) + "\n"
}

fn get_key(cfg: &Config, key: &str) -> Result<String, CommandError> {
    // Render the config to a string and walk the TOML tree.
    let s = toml::to_string_pretty(cfg).map_err(CommandError::internal_error)?;
    let doc: DocumentMut = s
        .parse()
        .map_err(|e: toml_edit::TomlError| CommandError::user_error(format!("parse: {e}")))?;
    let mut current: &Item = doc.as_item();
    for segment in key.split('.') {
        match current.get(segment) {
            Some(item) => current = item,
            None => {
                return Err(CommandError::user_error(format!("key not found: {key}")));
            }
        }
    }
    Ok(format!("{}\n", current.to_string().trim_end()))
}

fn set_key(doc: &mut DocumentMut, key: &str, raw_value: &str) -> Result<(), CommandError> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.is_empty() {
        return Err(CommandError::user_error("empty key"));
    }
    // Walk to the parent table, creating intermediate tables as needed.
    let mut current = doc.as_table_mut();
    for seg in &segments[..segments.len() - 1] {
        if !current.contains_key(seg) {
            current.insert(seg, Item::Table(Table::new()));
        }
        let item = &mut current[seg];
        let next = item
            .as_table_mut()
            .ok_or_else(|| CommandError::user_error(format!("{seg} is not a table")))?;
        current = next;
    }
    let last = segments[segments.len() - 1];
    // Parse the value as a TOML expression (so ints, bools, strings, arrays,
    // inline tables all work).
    let parsed: Value = raw_value
        .parse::<Value>()
        .map_err(|e| CommandError::user_error(format!("invalid TOML value: {e}")))?;
    current.insert(last, Item::Value(parsed));
    Ok(())
}

fn unset_key(doc: &mut DocumentMut, key: &str) -> Result<(), CommandError> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.is_empty() {
        return Err(CommandError::user_error("empty key"));
    }
    let mut current = doc.as_table_mut();
    for seg in &segments[..segments.len() - 1] {
        let next = current[*seg]
            .as_table_mut()
            .ok_or_else(|| CommandError::user_error(format!("{seg} is not a table")))?;
        current = next;
    }
    let last = segments[segments.len() - 1];
    if !current.contains_key(last) {
        return Err(CommandError::user_error(format!("key not found: {key}")));
    }
    current.remove(last);
    Ok(())
}

fn load_or_default(path: &Path) -> Result<DocumentMut, CommandError> {
    if path.exists() {
        let mut s = String::new();
        std::fs::File::open(path)
            .map_err(CommandError::internal_error)?
            .read_to_string(&mut s)
            .map_err(CommandError::internal_error)?;
        s.parse::<DocumentMut>()
            .map_err(|e| CommandError::user_error(format!("parse {}: {e}", path.display())))
    } else {
        // Use the bundled default config as a starting point.
        Ok(Config::default_toml()
            .parse::<DocumentMut>()
            .expect("bundled default config must parse"))
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), CommandError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CommandError::internal_error)?;
    }
    grs_lib::util::fs::atomic_write_str(path, contents).map_err(CommandError::internal_error)?;
    Ok(())
}

fn user_config_path() -> Option<PathBuf> {
    // Duplicates grs_lib::config::user_config_path but we also accept
    // XDG_CONFIG_HOME — keep them in sync.
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("grs").join("config.toml"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(
                PathBuf::from(home)
                    .join(".config")
                    .join("grs")
                    .join("config.toml"),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_key() {
        let mut doc = Config::default_toml().parse::<DocumentMut>().unwrap();
        set_key(&mut doc, "watcher.debounce_ms", "250").unwrap();
        // Round-trip via the parser.
        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.watcher.debounce_ms, 250);
    }

    #[test]
    fn unset_removes_key() {
        let mut doc = Config::default_toml().parse::<DocumentMut>().unwrap();
        set_key(&mut doc, "watcher.debounce_ms", "999").unwrap();
        unset_key(&mut doc, "watcher.debounce_ms").unwrap();
        // After unset, the key shouldn't be present.
        assert!(doc["watcher"].get("debounce_ms").is_none());
    }

    #[test]
    fn set_string_value() {
        let mut doc = Config::default_toml().parse::<DocumentMut>().unwrap();
        set_key(&mut doc, "tui.syntax_theme", "\"Dracula\"").unwrap();
        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.tui.syntax_theme, "Dracula");
    }

    #[test]
    fn set_array_value() {
        let mut doc = Config::default_toml().parse::<DocumentMut>().unwrap();
        set_key(
            &mut doc,
            "watcher.ignore_extra",
            r#"["secret.key", "*.tmp"]"#,
        )
        .unwrap();
        let cfg: Config = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(cfg.watcher.ignore_extra, vec!["secret.key", "*.tmp"]);
    }

    #[test]
    fn set_invalid_value_errors() {
        let mut doc = Config::default_toml().parse::<DocumentMut>().unwrap();
        assert!(set_key(&mut doc, "watcher.debounce_ms", "not-a-number").is_err());
    }
}
