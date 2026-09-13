//! Where the configuration and the best-ever share live between runs.
//!
//! `hansolo.toml` in the platform's config directory (`~/.config/hansolo` on
//! Linux, `~/Library/Application Support/net.biseth.hansolo` on macOS,
//! `%APPDATA%\biseth\hansolo` on Windows), or wherever `--config` says. A
//! read-only kiosk image is a normal place to be, so failing to save is
//! reported, never fatal.

use std::path::PathBuf;

use hansolo_core::Config;
use serde::{Deserialize, Serialize};

pub struct Store {
    config_path: PathBuf,
    state_path: PathBuf,
}

/// What the miner remembers that nobody configures.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub best_ever_difficulty: f64,
    pub total_hashes: u64,
}

impl Store {
    pub fn new(config_override: Option<PathBuf>) -> Self {
        let dir = directories::ProjectDirs::from("net", "biseth", "hansolo")
            .map(|dirs| dirs.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let config_path = config_override.unwrap_or_else(|| dir.join("hansolo.toml"));
        let state_path = config_path.with_file_name("state.toml");
        Self {
            config_path,
            state_path,
        }
    }

    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }

    pub fn load_config(&self) -> Config {
        match std::fs::read_to_string(&self.config_path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("{}: {e}; using defaults", self.config_path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save_config(&self, config: &Config) -> Result<(), String> {
        write(
            &self.config_path,
            &toml::to_string_pretty(config).map_err(|e| e.to_string())?,
        )
    }

    pub fn load_state(&self) -> State {
        std::fs::read_to_string(&self.state_path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, state: &State) -> Result<(), String> {
        write(
            &self.state_path,
            &toml::to_string_pretty(state).map_err(|e| e.to_string())?,
        )
    }
}

fn write(path: &PathBuf, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}
