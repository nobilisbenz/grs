//! Layered config: built-in defaults < user config < repo config < CLI flags.
//! Phase 1 keeps it minimal: `watcher.debounce_ms`, plus a few `replay`/`log`
//! knobs.

use crate::error::{GrsError, Result};
use crate::util::time::Millis;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub watcher: WatcherConfig,
    #[serde(default)]
    pub replay: ReplayConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatcherConfig {
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Extra ignore patterns appended to `.grsignore`.
    #[serde(default)]
    pub ignore_extra: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayConfig {
    #[serde(default = "default_speed_ms")]
    pub default_speed_ms: u64,
    #[serde(default = "default_syntax_theme")]
    pub syntax_theme: String,
}

fn default_debounce_ms() -> u64 {
    180
}
fn default_speed_ms() -> u64 {
    600
}
fn default_syntax_theme() -> String {
    "base16-eighties.dark".to_string()
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 180,
            ignore_extra: Vec::new(),
        }
    }
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            default_speed_ms: 600,
            syntax_theme: default_syntax_theme(),
        }
    }
}

impl Config {
    /// Read repo-local config from `.grs/config.toml`, layered over defaults.
    /// Missing file => defaults. Unknown fields ignored.
    pub fn load_repo(root: &Path) -> Result<Config> {
        let path = root.join(crate::paths::GRS_DIR).join(crate::paths::CONFIG_FILE);
        Self::load_file(&path)
    }

    /// Load user-level config (`~/.config/grs/config.toml`) layered over
    /// defaults. Missing => defaults.
    pub fn load_user() -> Config {
        if let Some(p) = user_config_path() {
            if p.exists() {
                return Self::load_file(&p).unwrap_or_default();
            }
        }
        Config::default()
    }

    fn load_file(path: &Path) -> Result<Config> {
        let text = crate::util::fs::read_to_string_or(path, "")?;
        if text.trim().is_empty() {
            return Ok(Config::default());
        }
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| GrsError::Config(format!("{}: {e}", path.display())))?;
        Ok(cfg)
    }

    /// Merge `other` on top of `self` (right wins on scalars).
    pub fn merged_with(self, other: Config) -> Config {
        Config {
            watcher: WatcherConfig {
                debounce_ms: if other.watcher.debounce_ms != WatcherConfig::default().debounce_ms {
                    other.watcher.debounce_ms
                } else {
                    self.watcher.debounce_ms
                },
                ignore_extra: if other.watcher.ignore_extra.is_empty() {
                    self.watcher.ignore_extra
                } else {
                    other.watcher.ignore_extra
                },
            },
            replay: ReplayConfig {
                default_speed_ms: if other.replay.default_speed_ms
                    != ReplayConfig::default().default_speed_ms
                {
                    other.replay.default_speed_ms
                } else {
                    self.replay.default_speed_ms
                },
                syntax_theme: if other.replay.syntax_theme == default_syntax_theme() {
                    self.replay.syntax_theme
                } else {
                    other.replay.syntax_theme
                },
            },
        }
    }

    /// The default config TOML written to `.grs/config.toml` on first run.
    pub fn default_toml() -> &'static str {
        include_str!("../default-config.toml")
    }
}

/// Resolve the user-config path. Linux: `~/.config/grs/config.toml`.
fn user_config_path() -> Option<PathBuf> {
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

/// Expand a leading `~` to the user's home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

/// Recompute `started_at`-style timestamps from the merged config (no-op
/// placeholder kept for API symmetry — config has no time fields).
pub fn _touch(_now: Millis) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.watcher.debounce_ms, 180);
    }

    #[test]
    fn load_missing_is_defaults() {
        let dir = tempdir().unwrap();
        let c = Config::load_repo(dir.path()).unwrap();
        assert_eq!(c.watcher.debounce_ms, 180);
    }

    #[test]
    fn parse_repo_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join(".grs").join("config.toml");
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "[watcher]\ndebounce_ms = 250\n").unwrap();
        let c = Config::load_repo(dir.path()).unwrap();
        assert_eq!(c.watcher.debounce_ms, 250);
    }
}
