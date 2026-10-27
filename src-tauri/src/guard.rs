//! Tab Guard's state between checks: settings, spared tabs, the activity
//! log, and the cooldown. `pass` runs one check's bookkeeping. Nothing here
//! touches Tauri, so it can be tested directly.

use crate::config::Config;
use crate::engine::{self, gb, Row, Status};
use crate::sampler::{Kind, Proc};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const MAX_EVENTS: usize = 300;

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    /// "closed", "would-close", "warn", "info"
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub firefox_running: bool,
    pub total_mb: u64,
    pub tab_count: usize,
    pub flagged: usize,
    pub over: usize,
    pub rows: Vec<Row>,
    /// Seconds until another close is allowed, if a cooldown is running.
    pub cooldown_secs: u64,
    pub checked_at: u64,
}

#[derive(Serialize)]
pub struct View {
    pub config: Config,
    pub snapshot: Snapshot,
    pub events: Vec<Event>,
    pub spared: Vec<u32>,
    pub log_path: String,
}

/// What a pass closed, for the caller to report.
#[derive(Debug, PartialEq)]
pub struct Closed {
    pub pid: u32,
    pub mem_mb: u64,
}

pub struct Guard {
    pub config: Config,
    /// Where settings are saved; None in tests.
    config_dir: Option<PathBuf>,
    /// The activity log file; None in tests.
    log_path: Option<PathBuf>,
    spared: HashSet<u32>,
    events: VecDeque<Event>,
    /// (pid, status) pairs already written to the activity log.
    logged: HashSet<(u32, Status)>,
    last_close: Option<Instant>,
    pub snapshot: Snapshot,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "2026-10-26 21:30:05" in the Mac's time zone, for the log file.
pub fn local_time(unix: u64) -> String {
    let t = unix as libc::time_t;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

impl Guard {
    pub fn new(config: Config, config_dir: Option<PathBuf>, log_path: Option<PathBuf>) -> Self {
        Guard {
            config,
            config_dir,
            log_path,
            spared: HashSet::new(),
            events: VecDeque::new(),
            logged: HashSet::new(),
            last_close: None,
            snapshot: Snapshot::default(),
        }
    }

    pub fn push_event(&mut self, kind: &'static str, text: String) {
        let at = unix_now();
        if let Some(path) = &self.log_path {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{} {kind}: {text}", local_time(at));
            }
        }
        self.events.push_front(Event { at, kind, text });
        self.events.truncate(MAX_EVENTS);
    }

    #[cfg(test)]
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    pub fn apply_config(&mut self, cfg: Config) {
        let cfg = cfg.sanitized();
        let old = self.config.clone();
        if old.enabled != cfg.enabled {
            self.push_event("info", if cfg.enabled { "Tab Guard turned on".into() } else { "Tab Guard paused".into() });
        }
        if old.mode != cfg.mode {
            self.push_event("info", format!("Mode changed to “{}”", cfg.mode.label()));
        }
        if let Some(dir) = self.config_dir.clone() {
            if let Err(e) = cfg.save(&dir) {
                self.push_event("info", format!("Couldn't save settings: {e}"));
            }
        }
        self.config = cfg;
        // Re-log flags under the new rules.
        self.logged.clear();
    }

    fn cooldown_left(&self, now: Instant) -> Duration {
        let cooldown = Duration::from_secs(self.config.kill_cooldown_secs);
        self.last_close.map(|t| cooldown.saturating_sub(now.duration_since(t))).unwrap_or_default()
    }

    /// One check: judge `procs` (biggest first), log what's new, close at
    /// most one tab through `close`, and update the snapshot.
    pub fn pass(&mut self, procs: &[Proc], now: Instant, close: &mut dyn FnMut(u32)) -> Option<Closed> {
        let can_close = self.cooldown_left(now).is_zero();
        let eval = engine::evaluate(&self.config, procs, &self.spared, can_close);

        // Forget processes that are gone.
        let alive: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
        self.spared.retain(|p| alive.contains(p));
        self.logged.retain(|(p, _)| alive.contains(p));

        for row in &eval.rows {
            if !matches!(row.status, Status::Warn | Status::Over) || self.logged.contains(&(row.proc.pid, row.status)) {
                continue;
            }
            if eval.close.as_ref().is_some_and(|(pid, _)| *pid == row.proc.pid) {
                continue; // logged as "closed" below
            }
            self.logged.insert((row.proc.pid, row.status));
            let reason = row.reason.clone().unwrap_or_default();
            let (kind, verb) = match (row.status, row.action.as_deref()) {
                (Status::Over, Some("Would close")) => ("would-close", "Would close"),
                (Status::Over, _) => ("warn", "Queued to close"),
                _ => ("warn", "Flagged"),
            };
            self.push_event(kind, format!("{verb} tab process {} ({}): {reason}", row.proc.pid, gb(row.proc.mem_mb)));
        }

        let mut closed = None;
        if let Some((pid, reason)) = &eval.close {
            let mem_mb = procs.iter().find(|p| p.pid == *pid).map(|p| p.mem_mb).unwrap_or(0);
            close(*pid);
            self.last_close = Some(now);
            self.push_event("closed", format!("Closed tab process {pid} ({}): {reason}", gb(mem_mb)));
            closed = Some(Closed { pid: *pid, mem_mb });
        }

        let tabs = eval.rows.iter().filter(|r| r.proc.kind == Kind::Tab);
        self.snapshot = Snapshot {
            firefox_running: !procs.is_empty(),
            total_mb: eval.total_mb,
            tab_count: tabs.clone().count(),
            flagged: tabs.clone().filter(|r| r.status == Status::Warn).count(),
            over: tabs.filter(|r| r.status == Status::Over).count(),
            rows: eval.rows,
            cooldown_secs: self.cooldown_left(now).as_secs(),
            checked_at: unix_now(),
        };
        closed
    }

    /// The user closing a tab from the window. Only tabs in the latest check
    /// can be closed, so a stale or reused pid is never signalled.
    pub fn close_by_user(&mut self, pid: u32, close: &mut dyn FnMut(u32)) -> Result<(), String> {
        let row = self
            .snapshot
            .rows
            .iter()
            .find(|r| r.proc.pid == pid && r.proc.kind == Kind::Tab)
            .cloned()
            .ok_or("That tab process is gone, or isn't a Firefox tab")?;
        close(pid);
        self.push_event("closed", format!("You closed tab process {pid} ({})", gb(row.proc.mem_mb)));
        Ok(())
    }

    pub fn set_spared(&mut self, pid: u32, spared: bool) {
        if spared {
            if self.spared.insert(pid) {
                self.push_event("info", format!("Sparing tab process {pid} until it exits"));
            }
        } else if self.spared.remove(&pid) {
            self.push_event("info", format!("Stopped sparing tab process {pid}"));
        }
    }

    pub fn view(&self) -> View {
        View {
            config: self.config.clone(),
            snapshot: self.snapshot.clone(),
            events: self.events.iter().cloned().collect(),
            spared: self.spared.iter().copied().collect(),
            log_path: self.log_path.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;

    fn tab(pid: u32, mem_mb: u64) -> Proc {
        Proc { pid, kind: Kind::Tab, mem_mb, cpu: 0.0, age_secs: 60, growth_mb_per_min: 0.0 }
    }

    fn guard(mode: Mode) -> Guard {
        Guard::new(Config { mode, ..Config::default() }, None, None)
    }

    /// Run a pass and return the pids it closed.
    fn run(g: &mut Guard, procs: &[Proc], now: Instant) -> Vec<u32> {
        let mut closed = Vec::new();
        g.pass(procs, now, &mut |pid| closed.push(pid));
        closed
    }

    fn kinds(g: &Guard) -> Vec<&'static str> {
        g.events().map(|e| e.kind).collect()
    }

    #[test]
    fn watch_mode_logs_once_and_closes_nothing() {
        let mut g = guard(Mode::Watch);
        let procs = [tab(1, 2500), tab(2, 1200)];
        let t = Instant::now();
        assert!(run(&mut g, &procs, t).is_empty());
        assert_eq!(kinds(&g), ["warn", "would-close"]);
        // The same flags on later passes aren't logged again.
        run(&mut g, &procs, t + Duration::from_secs(15));
        run(&mut g, &procs, t + Duration::from_secs(30));
        assert_eq!(g.events().count(), 2);
        assert_eq!(g.snapshot.over, 1);
        assert_eq!(g.snapshot.flagged, 1);
    }

    #[test]
    fn closes_one_per_cooldown() {
        let mut g = guard(Mode::Close);
        let procs = [tab(1, 3000), tab(2, 2500)];
        let t = Instant::now();
        assert_eq!(run(&mut g, &procs, t), [1]);
        assert_eq!(g.snapshot.cooldown_secs, 60);
        // pid 1 is gone; pid 2 waits out the cooldown.
        let after = [tab(2, 2500)];
        assert!(run(&mut g, &after, t + Duration::from_secs(30)).is_empty());
        assert_eq!(g.snapshot.cooldown_secs, 30);
        assert_eq!(run(&mut g, &after, t + Duration::from_secs(61)), [2]);
        assert_eq!(kinds(&g).iter().filter(|k| **k == "closed").count(), 2);
    }

    #[test]
    fn pass_reports_what_it_closed() {
        let mut g = guard(Mode::WarnThenClose);
        let closed = g.pass(&[tab(7, 2100)], Instant::now(), &mut |_| {});
        assert_eq!(closed, Some(Closed { pid: 7, mem_mb: 2100 }));
        assert!(g.events().next().unwrap().text.contains("Closed tab process 7 (2.1 GB)"));
    }

    #[test]
    fn mode_change_relogs_under_new_rules() {
        let mut g = guard(Mode::Watch);
        let procs = [tab(1, 2500)];
        let t = Instant::now();
        run(&mut g, &procs, t);
        g.apply_config(Config { mode: Mode::Close, ..g.config.clone() });
        assert_eq!(g.events().next().unwrap().text, "Mode changed to “Close at limit”");
        assert_eq!(run(&mut g, &procs, t + Duration::from_secs(1)), [1]);
    }

    #[test]
    fn pausing_stops_closing() {
        let mut g = guard(Mode::Close);
        g.apply_config(Config { enabled: false, ..g.config.clone() });
        assert_eq!(g.events().next().unwrap().text, "Tab Guard paused");
        assert!(run(&mut g, &[tab(1, 5000)], Instant::now()).is_empty());
    }

    #[test]
    fn spared_tab_is_skipped_until_it_exits() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run(&mut g, &[tab(2, 100)], t); // pid 1 not seen yet
        g.set_spared(1, true);
        assert!(run(&mut g, &[tab(1, 3000), tab(2, 100)], t).is_empty());
        // Sparing twice doesn't log twice.
        g.set_spared(1, true);
        assert_eq!(kinds(&g).iter().filter(|k| **k == "info").count(), 1);
        // Once it exits, a new process with the same pid isn't spared.
        run(&mut g, &[tab(2, 100)], t);
        assert_eq!(run(&mut g, &[tab(1, 3000)], t), [1]);
    }

    #[test]
    fn user_can_only_close_tabs_from_the_last_check() {
        let mut g = guard(Mode::Watch);
        let main = Proc { kind: Kind::Main, ..tab(9, 800) };
        run(&mut g, &[tab(1, 500), main], Instant::now());
        let mut closed = Vec::new();
        assert!(g.close_by_user(1, &mut |p| closed.push(p)).is_ok());
        assert!(g.close_by_user(9, &mut |p| closed.push(p)).is_err(), "never closes Firefox itself");
        assert!(g.close_by_user(42, &mut |p| closed.push(p)).is_err());
        assert_eq!(closed, [1]);
    }

    #[test]
    fn activity_log_is_capped() {
        let mut g = guard(Mode::Watch);
        for i in 0..(MAX_EVENTS + 50) {
            g.push_event("info", format!("event {i}"));
        }
        assert_eq!(g.events().count(), MAX_EVENTS);
        assert_eq!(g.events().next().unwrap().text, format!("event {}", MAX_EVENTS + 49));
    }

    #[test]
    fn no_firefox_means_empty_snapshot() {
        let mut g = guard(Mode::Close);
        run(&mut g, &[], Instant::now());
        assert!(!g.snapshot.firefox_running);
        assert_eq!(g.snapshot.total_mb, 0);
        assert!(g.events().next().is_none());
    }

    #[test]
    fn writes_the_log_file() {
        let dir = std::env::temp_dir().join(format!("tab-guard-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log.txt");
        let mut g = Guard::new(Config::default(), Some(dir.clone()), Some(log.clone()));
        g.push_event("info", "hello".into());
        g.apply_config(Config { mode: Mode::Close, ..Config::default() });
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains(" info: hello\n"));
        assert!(text.contains("Mode changed to “Close at limit”"));
        assert!(Config::load(&dir).mode == Mode::Close, "settings were saved");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn local_time_looks_like_a_timestamp() {
        let s = local_time(1_793_065_258);
        assert_eq!(s.len(), 19);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[13..14], ":");
    }
}
