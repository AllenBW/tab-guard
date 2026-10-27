//! User settings, saved as JSON in the app's config directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Show what would be closed and why, never close anything.
    Watch,
    /// Flag tabs past the warning level, close past the hard limits.
    WarnThenClose,
    /// Close past the hard limits; nothing is flagged early.
    Close,
}

impl Mode {
    pub fn closes(self) -> bool {
        self != Mode::Watch
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Watch => "Watch only",
            Mode::WarnThenClose => "Warn, then close",
            Mode::Close => "Close at limit",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Master switch; when false nothing is checked or closed.
    pub enabled: bool,
    pub mode: Mode,
    /// A tab process past this is flagged.
    pub warn_mb: u64,
    /// A tab process past this is closed (or, in Watch mode, marked "would close").
    pub kill_mb: u64,
    /// If all of Firefox passes this, the biggest eligible tab is closed. 0 = off.
    pub total_mb: u64,
    /// Never close a tab smaller than this just to get under `total_mb`.
    pub total_min_victim_mb: u64,
    /// Seconds between checks while the window is closed.
    pub interval_secs: u64,
    /// Minimum seconds between two closes.
    pub kill_cooldown_secs: u64,
    /// For the whole-browser limit, pick an idle tab over a busy one when possible.
    pub prefer_idle: bool,
    /// A tab using less CPU than this (percent of one core) counts as idle.
    pub idle_cpu_percent: f32,
    /// macOS notification when a tab is closed.
    pub notify_on_close: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            mode: Mode::Watch,
            warn_mb: 1024,
            kill_mb: 2048,
            total_mb: 8192,
            total_min_victim_mb: 400,
            interval_secs: 15,
            kill_cooldown_secs: 60,
            prefer_idle: true,
            idle_cpu_percent: 2.0,
            notify_on_close: false,
        }
    }
}

impl Config {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("config.json")
    }

    pub fn load(config_dir: &Path) -> Config {
        std::fs::read_to_string(Self::path(config_dir))
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .map(Config::sanitized)
            .unwrap_or_default()
    }

    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let json = serde_json::to_string_pretty(self).expect("config serializes");
        std::fs::write(Self::path(config_dir), json)
    }

    /// Clamp values the UI or a hand-edited file could get wrong.
    pub fn sanitized(mut self) -> Config {
        self.warn_mb = self.warn_mb.max(100);
        self.kill_mb = self.kill_mb.max(self.warn_mb);
        self.interval_secs = self.interval_secs.clamp(3, 600);
        self.idle_cpu_percent = self.idle_cpu_percent.clamp(0.0, 100.0);
        self
    }
}
