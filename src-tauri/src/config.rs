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
    /// When a close for `total_mb` doesn't bring Firefox's total down, stop
    /// applying `total_mb` for this many minutes. 0 = never pause.
    pub total_backoff_mins: u64,
    /// Close at most this many tabs an hour for `total_mb`. 0 = no cap.
    pub total_max_per_hour: u32,
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
            total_backoff_mins: 15,
            total_max_per_hour: 3,
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
        self.total_backoff_mins = self.total_backoff_mins.min(24 * 60);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tab-guard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_never_close_anything() {
        let c = Config::default();
        assert!(c.enabled);
        assert_eq!(c.mode, Mode::Watch);
        assert!(!c.mode.closes());
        assert!(c.warn_mb < c.kill_mb);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = temp_dir("missing");
        assert_eq!(Config::load(&d).mode, Mode::Watch);
    }

    #[test]
    fn save_then_load_round_trips() {
        let d = temp_dir("roundtrip");
        let c = Config { mode: Mode::WarnThenClose, warn_mb: 1500, kill_mb: 3000, notify_on_close: true, ..Config::default() };
        c.save(&d).unwrap();
        let back = Config::load(&d);
        assert_eq!(back.mode, Mode::WarnThenClose);
        assert_eq!((back.warn_mb, back.kill_mb), (1500, 3000));
        assert!(back.notify_on_close);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let d = temp_dir("partial");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(Config::path(&d), r#"{"mode":"close","kill_mb":4096}"#).unwrap();
        let c = Config::load(&d);
        assert_eq!(c.mode, Mode::Close);
        assert_eq!(c.kill_mb, 4096);
        assert_eq!(c.warn_mb, Config::default().warn_mb);
        assert_eq!((c.total_backoff_mins, c.total_max_per_hour), (15, 3), "settings saved before these existed");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn broken_file_falls_back_to_safe_defaults() {
        let d = temp_dir("broken");
        std::fs::create_dir_all(&d).unwrap();
        for bad in ["not json", r#"{"mode":"destroy-everything"}"#, r#"{"warn_mb":"lots"}"#] {
            std::fs::write(Config::path(&d), bad).unwrap();
            assert_eq!(Config::load(&d).mode, Mode::Watch, "for {bad}");
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn sanitize_clamps_bad_values() {
        let c = Config { warn_mb: 10, kill_mb: 50, interval_secs: 0, idle_cpu_percent: -5.0, ..Config::default() }
            .sanitized();
        assert_eq!(c.warn_mb, 100);
        assert_eq!(c.kill_mb, 100, "kill limit can't be below the warning level");
        assert_eq!(c.interval_secs, 3);
        assert_eq!(c.idle_cpu_percent, 0.0);
        let c = Config { interval_secs: 100_000, idle_cpu_percent: 900.0, total_backoff_mins: 1_000_000, ..Config::default() }
            .sanitized();
        assert_eq!(c.interval_secs, 600);
        assert_eq!(c.total_backoff_mins, 24 * 60);
        assert_eq!(c.idle_cpu_percent, 100.0);
    }

    #[test]
    fn modes_serialize_as_the_ui_expects() {
        assert_eq!(serde_json::to_string(&Mode::Watch).unwrap(), r#""watch""#);
        assert_eq!(serde_json::to_string(&Mode::WarnThenClose).unwrap(), r#""warn-then-close""#);
        assert_eq!(serde_json::to_string(&Mode::Close).unwrap(), r#""close""#);
    }
}
