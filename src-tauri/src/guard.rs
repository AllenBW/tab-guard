//! Tab Guard's state between checks: settings, spared tabs, the activity
//! log, the cooldown, and the whole-browser limit's back-off for each copy of
//! Firefox. `pass` runs one check's bookkeeping. Nothing here touches Tauri,
//! so it can be tested directly.

use crate::config::{Config, Mode};
use crate::engine::{self, gb, Row, Status};
use crate::sampler::{Browser, Kind, Proc};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
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

/// One running copy of Firefox, for the window and the menu bar.
#[derive(Debug, Clone, Serialize)]
pub struct BrowserSummary {
    pub id: u32,
    pub name: String,
    /// The app bundle, for opening about:processes in the right one.
    #[serde(skip)]
    pub app: String,
    pub total_mb: u64,
    pub tab_count: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub firefox_running: bool,
    /// Every copy of Firefox together.
    pub total_mb: u64,
    pub tab_count: usize,
    pub flagged: usize,
    pub over: usize,
    pub rows: Vec<Row>,
    /// The copies of Firefox running, oldest first. Usually just one.
    pub browsers: Vec<BrowserSummary>,
    /// Seconds until another close is allowed, if a cooldown is running.
    pub cooldown_secs: u64,
    /// Why the whole-browser limit is paused, and until when, if it is (for
    /// any copy of Firefox).
    pub total_hold: Option<String>,
    pub checked_at: u64,
}

/// A copy of Firefox whose limits can be set in the window.
#[derive(Debug, Serialize, PartialEq)]
pub struct LimitTarget {
    /// `Browser::key`, as in `Config::browser_limits`.
    pub key: String,
    pub name: String,
    pub running: bool,
    /// Whether it has limits of its own yet.
    pub own: bool,
}

#[derive(Serialize)]
pub struct View {
    pub config: Config,
    pub snapshot: Snapshot,
    /// Running copies first (oldest first), then ones with saved limits.
    pub limit_targets: Vec<LimitTarget>,
    pub events: Vec<Event>,
    pub spared: Vec<u32>,
    pub log_path: String,
}

/// Something counted in the anonymous usage stats. Recorded only while the
/// user shares them; `lib.rs` sends them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Usage {
    /// A tab was closed: "per-tab", "whole-browser" or "by-you".
    Closed(&'static str),
    /// The whole-browser limit paused: "no-drop" or "hourly-cap".
    Paused(&'static str),
    ModeChanged(Mode),
}

/// What a pass closed, for the caller to report.
#[derive(Debug, PartialEq)]
pub struct Closed {
    pub pid: u32,
    pub mem_mb: u64,
    /// The name of the Firefox it belonged to.
    pub browser: String,
}

/// The whole-browser limit's state for one copy of Firefox.
#[derive(Default)]
struct TotalLimit {
    /// When each close for this limit in the last hour happened.
    closes: VecDeque<Instant>,
    /// The latest close, still to be judged: (when, this Firefox's total just
    /// before, the tab's size).
    unjudged: Option<(Instant, u64, u64)>,
    /// Paused until then; the text says why.
    hold: Option<(Instant, String)>,
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
    /// The whole-browser limit, for each copy of Firefox by browser id.
    limits: HashMap<u32, TotalLimit>,
    /// The copies of Firefox seen in the latest check.
    browsers: Vec<Browser>,
    /// Usage not yet sent.
    usage: Vec<Usage>,
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
            limits: HashMap::new(),
            browsers: Vec::new(),
            usage: Vec::new(),
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

    fn count(&mut self, u: Usage) {
        if self.config.share_usage {
            self.usage.push(u);
        }
    }

    /// Usage recorded since the last call.
    pub fn take_usage(&mut self) -> Vec<Usage> {
        std::mem::take(&mut self.usage)
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
        if old.share_usage != cfg.share_usage {
            self.push_event(
                "info",
                if cfg.share_usage { "Sharing anonymous usage counts".into() } else { "Stopped sharing usage counts".into() },
            );
        }
        // New whole-browser settings get a fresh start. The hourly count
        // stays, so the cap still holds.
        let total_changed = (old.total_mb, old.total_backoff_mins, old.total_max_per_hour)
            != (cfg.total_mb, cfg.total_backoff_mins, cfg.total_max_per_hour)
            || old.browser_limits != cfg.browser_limits;
        if total_changed {
            let mut ended = false;
            for lim in self.limits.values_mut() {
                ended |= lim.hold.take().is_some();
                lim.unjudged = None;
            }
            if ended {
                self.push_event("info", "The whole-browser limit is back on, with your new settings".into());
            }
        }
        if let Some(dir) = self.config_dir.clone() {
            if let Err(e) = cfg.save(&dir) {
                self.push_event("info", format!("Couldn't save settings: {e}"));
            }
        }
        let mode_changed = old.mode != cfg.mode;
        self.config = cfg;
        if mode_changed {
            self.count(Usage::ModeChanged(self.config.mode));
        }
        // Re-log flags under the new rules.
        self.logged.clear();
    }

    fn cooldown_left(&self, now: Instant) -> Duration {
        let cooldown = Duration::from_secs(self.config.kill_cooldown_secs);
        self.last_close.map(|t| cooldown.saturating_sub(now.duration_since(t))).unwrap_or_default()
    }

    /// " for Firefox Nightly" when more than one Firefox is running, so
    /// messages say which one they mean; otherwise nothing.
    fn which(&self, browser: u32) -> String {
        match self.browsers.iter().find(|b| b.id == browser) {
            Some(b) if self.browsers.len() > 1 => format!(" for {}", b.name),
            _ => String::new(),
        }
    }

    /// "tab process 123", or "Firefox Nightly tab process 123" when more
    /// than one Firefox is running.
    fn tab_name(&self, browser: u32, pid: u32) -> String {
        match self.browsers.iter().find(|b| b.id == browser) {
            Some(b) if self.browsers.len() > 1 => format!("{} tab process {pid}", b.name),
            _ => format!("tab process {pid}"),
        }
    }

    fn hold_total(&mut self, lim: &mut TotalLimit, b: &Browser, until: Instant, now: Instant, kind: &'static str, why: String) {
        self.count(Usage::Paused(kind));
        let at = local_time(unix_now() + until.saturating_duration_since(now).as_secs());
        let text = format!("Whole-browser limit{} paused until {}: {why}", self.which(b.id), &at[11..16]);
        self.push_event("info", text.clone());
        lim.hold = Some((until, text));
    }

    /// Pause the whole-browser limit for one Firefox when closing its tabs
    /// isn't bringing it down, or when it has closed its hourly maximum.
    /// Firefox keeps some memory no tab close can free, so a limit set below
    /// that would otherwise close a tab every cooldown, forever.
    fn update_total_hold(&mut self, b: &Browser, total_mb: u64, now: Instant) {
        let mut lim = self.limits.remove(&b.id).unwrap_or_default();
        self.judge_total(&mut lim, b, total_mb, now);
        self.limits.insert(b.id, lim);
    }

    fn judge_total(&mut self, lim: &mut TotalLimit, b: &Browser, total_mb: u64, now: Instant) {
        let hour = Duration::from_secs(3600);
        while lim.closes.front().is_some_and(|t| now.duration_since(*t) >= hour) {
            lim.closes.pop_front();
        }
        if let Some((until, _)) = &lim.hold {
            if now < *until {
                return;
            }
            lim.hold = None;
            self.push_event("info", format!("The whole-browser limit{} is back on", self.which(b.id)));
        }
        let cfg = self.config.clone();
        let over = cfg.total_mb > 0 && total_mb > cfg.total_mb;

        // Judge the last close once the cooldown is up, just before the
        // rule would pick another tab. It helped if this Firefox is under the
        // limit, or down by at least half of what the tab was using.
        if let Some((at, before, freed)) = lim.unjudged {
            if now.duration_since(at) >= Duration::from_secs(cfg.kill_cooldown_secs) {
                lim.unjudged = None;
                let dropped = before.saturating_sub(total_mb);
                if over && dropped < freed / 2 && cfg.total_backoff_mins > 0 {
                    let until = now + Duration::from_secs(cfg.total_backoff_mins * 60);
                    let why = format!(
                        "closing a {} tab didn't bring {} down ({} before, {} now)",
                        gb(freed),
                        b.name,
                        gb(before),
                        gb(total_mb)
                    );
                    self.hold_total(lim, b, until, now, "no-drop", why);
                    return;
                }
            }
        }

        let cap = cfg.total_max_per_hour as usize;
        if over && cap > 0 && lim.closes.len() >= cap {
            let until = lim.closes[lim.closes.len() - cap] + hour;
            let why = format!("it has closed {cap} tab{} in the last hour", if cap == 1 { "" } else { "s" });
            self.hold_total(lim, b, until, now, "hourly-cap", why);
        }
    }

    /// One check: judge `procs` (biggest first) from the copies of Firefox
    /// in `browsers`, log what's new, close at most one tab through `close`,
    /// and update the snapshot.
    pub fn pass(
        &mut self,
        procs: &[Proc],
        browsers: &[Browser],
        now: Instant,
        close: &mut dyn FnMut(u32),
    ) -> Option<Closed> {
        self.browsers = browsers.to_vec();
        // A Firefox that quit takes its pauses and hourly count with it.
        self.limits.retain(|id, _| browsers.iter().any(|b| b.id == *id));
        let totals = engine::totals(procs);
        // Only a closing mode builds up whole-browser closes to judge.
        if self.config.enabled && self.config.mode.closes() {
            for b in browsers {
                self.update_total_hold(b, totals.get(&b.id).copied().unwrap_or(0), now);
            }
        }
        let can_close = self.cooldown_left(now).is_zero();
        let held: HashSet<u32> =
            self.limits.iter().filter(|(_, lim)| lim.hold.is_some()).map(|(id, _)| *id).collect();
        let eval = engine::evaluate(&self.config, procs, browsers, &self.spared, can_close, &held);

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
            let name = self.tab_name(row.proc.browser, row.proc.pid);
            self.push_event(kind, format!("{verb} {name} ({}): {reason}", gb(row.proc.mem_mb)));
        }

        let mut closed = None;
        if let Some((pid, reason)) = &eval.close {
            let (mem_mb, browser) = procs.iter().find(|p| p.pid == *pid).map_or((0, 0), |p| (p.mem_mb, p.browser));
            close(*pid);
            self.last_close = Some(now);
            if eval.close_by_total {
                let before = totals.get(&browser).copied().unwrap_or(0);
                let lim = self.limits.entry(browser).or_default();
                lim.closes.push_back(now);
                lim.unjudged = Some((now, before, mem_mb));
            }
            self.count(Usage::Closed(if eval.close_by_total { "whole-browser" } else { "per-tab" }));
            let name = self.tab_name(browser, *pid);
            self.push_event("closed", format!("Closed {name} ({}): {reason}", gb(mem_mb)));
            let browser = browsers.iter().find(|b| b.id == browser).map_or("Firefox".into(), |b| b.name.clone());
            closed = Some(Closed { pid: *pid, mem_mb, browser });
        }

        let is_tab = |r: &&Row| r.proc.kind == Kind::Tab;
        let tabs = eval.rows.iter().filter(is_tab);
        // Holds in the order the browsers are listed.
        let holds: Vec<&str> = browsers
            .iter()
            .filter_map(|b| self.limits.get(&b.id)?.hold.as_ref().map(|(_, text)| text.as_str()))
            .collect();
        self.snapshot = Snapshot {
            firefox_running: !procs.is_empty(),
            total_mb: eval.total_mb,
            tab_count: tabs.clone().count(),
            flagged: tabs.clone().filter(|r| r.status == Status::Warn).count(),
            over: tabs.filter(|r| r.status == Status::Over).count(),
            browsers: browsers
                .iter()
                .map(|b| BrowserSummary {
                    id: b.id,
                    name: b.name.clone(),
                    app: b.app.clone(),
                    total_mb: totals.get(&b.id).copied().unwrap_or(0),
                    tab_count: eval.rows.iter().filter(is_tab).filter(|r| r.proc.browser == b.id).count(),
                })
                .collect(),
            rows: eval.rows,
            cooldown_secs: self.cooldown_left(now).as_secs(),
            total_hold: (!holds.is_empty()).then(|| holds.join(". ")),
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
        let name = self.tab_name(row.proc.browser, pid);
        self.push_event("closed", format!("You closed {name} ({})", gb(row.proc.mem_mb)));
        self.count(Usage::Closed("by-you"));
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
        let mut limit_targets: Vec<LimitTarget> = Vec::new();
        let running = self.browsers.iter().map(|b| (b.key.as_str(), true));
        let saved = self.config.browser_limits.keys().map(|k| (k.as_str(), false));
        for (key, running) in running.chain(saved) {
            if !limit_targets.iter().any(|t| t.key == key) {
                limit_targets.push(LimitTarget {
                    key: key.to_string(),
                    name: crate::sampler::key_name(key),
                    running,
                    own: self.config.browser_limits.contains_key(key),
                });
            }
        }
        View {
            config: self.config.clone(),
            snapshot: self.snapshot.clone(),
            limit_targets,
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
        Proc { pid, browser: 0, kind: Kind::Tab, mem_mb, cpu: 0.0, age_secs: 60, growth_mb_per_min: 0.0 }
    }

    fn browser(id: u32, name: &str) -> Browser {
        let app = format!("/Applications/{name}.app");
        Browser { id, name: name.into(), key: app.clone(), app }
    }

    /// The one Firefox most tests run with.
    fn firefox() -> Vec<Browser> {
        vec![browser(0, "Firefox")]
    }

    fn guard(mode: Mode) -> Guard {
        Guard::new(Config { mode, ..Config::default() }, None, None)
    }

    /// Run a pass and return the pids it closed.
    fn run(g: &mut Guard, procs: &[Proc], now: Instant) -> Vec<u32> {
        let mut closed = Vec::new();
        g.pass(procs, &firefox(), now, &mut |pid| closed.push(pid));
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
        let closed = g.pass(&[tab(7, 2100)], &firefox(), Instant::now(), &mut |_| {});
        assert_eq!(closed, Some(Closed { pid: 7, mem_mb: 2100, browser: "Firefox".into() }));
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

    /// Twelve 900 MB tabs: 10.5 GB, over the default 8 GB whole-browser
    /// limit with no tab over a per-tab limit. `drop` leaves out tabs that
    /// were closed; `extra` adds what Firefox grew back.
    fn crowd(drop: &[u32], extra: u64) -> Vec<Proc> {
        let mut v: Vec<Proc> = (1..=12).filter(|p| !drop.contains(p)).map(|p| tab(p, 900)).collect();
        if extra > 0 {
            v.push(Proc { kind: Kind::Main, ..tab(99, extra) });
        }
        v.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb).then(a.pid.cmp(&b.pid)));
        v
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn backs_off_when_a_close_doesnt_lower_the_total() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        assert_eq!(run(&mut g, &crowd(&[], 0), t), [1]);
        // Firefox grows the 900 MB straight back, elsewhere.
        assert!(run(&mut g, &crowd(&[1], 900), t + secs(61)).is_empty());
        let hold = g.snapshot.total_hold.clone().expect("paused");
        assert!(hold.contains("closing a 0.9 GB tab didn't bring Firefox down"), "{hold}");
        assert!(hold.contains("10.5 GB before, 10.5 GB now"), "{hold}");
        assert_eq!(g.snapshot.over, 0, "nothing is marked to close while paused");
        // Still paused 14 minutes later; back on after 15.
        assert!(run(&mut g, &crowd(&[1], 900), t + secs(61 + 14 * 60)).is_empty());
        assert_eq!(run(&mut g, &crowd(&[1], 900), t + secs(61 + 15 * 60)), [2]);
        assert!(g.events().any(|e| e.text == "The whole-browser limit is back on"));
    }

    #[test]
    fn keeps_going_when_closes_help() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        assert_eq!(run(&mut g, &crowd(&[], 0), t), [1]);
        // Each close takes the tab's 900 MB off the total.
        assert_eq!(run(&mut g, &crowd(&[1], 0), t + secs(61)), [2]);
        assert!(g.snapshot.total_hold.is_none());
    }

    #[test]
    fn caps_whole_browser_closes_per_hour() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        let mut gone = vec![];
        for i in 0..3u64 {
            // 2 GB of Firefox that no close frees keeps it over the limit.
            let closed = run(&mut g, &crowd(&gone, 2048), t + secs(61 * i));
            assert_eq!(closed.len(), 1);
            gone.extend(closed);
        }
        // Each close helped, but Firefox is still over; the fourth waits.
        assert!(run(&mut g, &crowd(&gone, 2048), t + secs(61 * 3)).is_empty());
        let hold = g.snapshot.total_hold.clone().expect("paused");
        assert!(hold.contains("it has closed 3 tabs in the last hour"), "{hold}");
        // An hour after the first close, one more is allowed.
        assert_eq!(run(&mut g, &crowd(&gone, 2048), t + secs(3600)).len(), 1);
    }

    #[test]
    fn per_tab_limits_ignore_the_pause() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run(&mut g, &crowd(&[], 0), t);
        run(&mut g, &crowd(&[1], 900), t + secs(61));
        assert!(g.snapshot.total_hold.is_some());
        let mut procs = crowd(&[1], 900);
        procs.insert(0, tab(50, 3000));
        assert_eq!(run(&mut g, &procs, t + secs(122)), [50]);
    }

    #[test]
    fn zero_turns_off_back_off_and_cap() {
        let cfg = Config { mode: Mode::Close, total_backoff_mins: 0, total_max_per_hour: 0, ..Config::default() };
        let mut g = Guard::new(cfg, None, None);
        let t = Instant::now();
        let mut closed = vec![];
        for i in 0..5u64 {
            // Firefox never gets any smaller.
            closed.extend(run(&mut g, &crowd(&closed, 900 * closed.len() as u64), t + secs(61 * i)));
        }
        assert_eq!(closed.len(), 5);
        assert!(g.snapshot.total_hold.is_none());
    }

    #[test]
    fn new_whole_browser_settings_end_the_pause() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run(&mut g, &crowd(&[], 0), t);
        run(&mut g, &crowd(&[1], 900), t + secs(61));
        assert!(g.snapshot.total_hold.is_some());
        // Changing something unrelated keeps it paused.
        g.apply_config(Config { notify_on_close: true, ..g.config.clone() });
        run(&mut g, &crowd(&[1], 900), t + secs(62));
        assert!(g.snapshot.total_hold.is_some());
        g.apply_config(Config { total_mb: 12 * 1024, ..g.config.clone() });
        run(&mut g, &crowd(&[1], 900), t + secs(63));
        assert!(g.snapshot.total_hold.is_none());
        assert_eq!(g.snapshot.over, 0, "10.5 GB is under the new 12 GB limit");
    }

    #[test]
    fn watch_mode_never_pauses() {
        let mut g = guard(Mode::Watch);
        let t = Instant::now();
        for i in 0..5u64 {
            run(&mut g, &crowd(&[], 0), t + secs(61 * i));
        }
        assert!(g.snapshot.total_hold.is_none());
        assert_eq!(g.snapshot.over, 1, "still shows what would be closed");
    }

    #[test]
    fn usage_is_only_recorded_while_shared() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run(&mut g, &[tab(1, 3000)], t);
        g.apply_config(Config { mode: Mode::Watch, ..g.config.clone() });
        assert!(g.take_usage().is_empty());

        g.apply_config(Config { share_usage: true, mode: Mode::Close, ..g.config.clone() });
        assert_eq!(g.events().next().unwrap().text, "Sharing anonymous usage counts");
        run(&mut g, &[tab(2, 3000)], t + secs(120));
        run(&mut g, &crowd(&[], 0), t + secs(240));
        run(&mut g, &crowd(&[1], 900), t + secs(301));
        g.close_by_user(2, &mut |_| {}).unwrap();
        assert_eq!(
            g.take_usage(),
            [
                Usage::ModeChanged(Mode::Close),
                Usage::Closed("per-tab"),
                Usage::Closed("whole-browser"),
                Usage::Paused("no-drop"),
                Usage::Closed("by-you"),
            ]
        );
        assert!(g.take_usage().is_empty(), "taken once");
    }

    /// `crowd` for Firefox (id 0, pids 1-12) beside Nightly (id 1, pids
    /// 101-112), each 10.5 GB: both over the 8 GB limit on their own.
    fn two_crowds(drop: &[u32], extra: [u64; 2]) -> Vec<Proc> {
        let nightly_drop: Vec<u32> = drop.iter().filter(|p| **p > 100).map(|p| p - 100).collect();
        let mut v = crowd(drop, extra[0]);
        v.extend(crowd(&nightly_drop, extra[1]).into_iter().map(|p| Proc {
            pid: p.pid + 100,
            browser: 1,
            ..p
        }));
        v.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb).then(a.pid.cmp(&b.pid)));
        v
    }

    fn run_two(g: &mut Guard, procs: &[Proc], now: Instant) -> Vec<u32> {
        let mut closed = Vec::new();
        g.pass(procs, &[browser(0, "Firefox"), browser(1, "Firefox Nightly")], now, &mut |pid| closed.push(pid));
        closed
    }

    #[test]
    fn each_firefox_backs_off_on_its_own() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        // Firefox's tabs are the biggest-first pick: pid 1.
        assert_eq!(run_two(&mut g, &two_crowds(&[], [0, 0]), t), [1]);
        // Firefox grows it straight back; Firefox pauses, Nightly carries on.
        assert_eq!(run_two(&mut g, &two_crowds(&[1], [900, 0]), t + secs(61)), [101]);
        let hold = g.snapshot.total_hold.clone().expect("Firefox paused");
        assert!(hold.starts_with("Whole-browser limit for Firefox paused until"), "{hold}");
        assert!(hold.contains("didn't bring Firefox down"), "{hold}");
        assert!(g.events().any(|e| e.text.starts_with("Closed Firefox Nightly tab process 101 (0.9 GB): Firefox Nightly is at 10.5 GB")));
        // Nightly's close helped, so it keeps going.
        assert_eq!(run_two(&mut g, &two_crowds(&[1, 101], [900, 0]), t + secs(122)), [102]);
        assert_eq!(g.snapshot.browsers.iter().map(|b| b.tab_count).collect::<Vec<_>>(), [11, 11]);
    }

    #[test]
    fn a_firefox_that_quits_takes_its_pause_with_it() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run(&mut g, &crowd(&[], 0), t);
        run(&mut g, &crowd(&[1], 900), t + secs(61));
        assert!(g.snapshot.total_hold.is_some());
        // Firefox quits, then starts again.
        g.pass(&[], &[], t + secs(70), &mut |_| {});
        assert_eq!(run(&mut g, &crowd(&[], 0), t + secs(140)), [1]);
    }

    #[test]
    fn one_firefox_keeps_the_old_wording() {
        let mut g = guard(Mode::Watch);
        run(&mut g, &[tab(1, 2500)], Instant::now());
        let text = &g.events().next().unwrap().text;
        assert!(text.starts_with("Would close tab process 1 (2.4 GB): Over the"), "{text}");
        assert_eq!(g.snapshot.browsers.len(), 1);
    }

    #[test]
    fn new_limits_for_one_firefox_end_the_pause() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run(&mut g, &crowd(&[], 0), t);
        run(&mut g, &crowd(&[1], 900), t + secs(61));
        assert!(g.snapshot.total_hold.is_some());
        let own = crate::config::Limits { warn_mb: 1024, kill_mb: 2048, total_mb: 12 * 1024, total_min_victim_mb: 400 };
        g.apply_config(Config {
            browser_limits: [("/Applications/Firefox.app".to_string(), own)].into(),
            ..g.config.clone()
        });
        assert_eq!(g.events().next().unwrap().text, "The whole-browser limit is back on, with your new settings");
        run(&mut g, &crowd(&[1], 900), t + secs(62));
        assert!(g.snapshot.total_hold.is_none());
        assert_eq!(g.snapshot.over, 0, "10.5 GB is under this Firefox's new 12 GB limit");
    }

    #[test]
    fn both_pauses_are_shown_when_both_firefoxes_pause() {
        let mut g = guard(Mode::Close);
        let t = Instant::now();
        run_two(&mut g, &two_crowds(&[], [0, 0]), t);
        run_two(&mut g, &two_crowds(&[1], [900, 0]), t + secs(61)); // Firefox pauses; Nightly closes 101
        run_two(&mut g, &two_crowds(&[1, 101], [900, 900]), t + secs(122)); // Nightly grew it back
        let hold = g.snapshot.total_hold.clone().expect("paused");
        assert!(hold.starts_with("Whole-browser limit for Firefox paused"), "{hold}");
        assert!(hold.contains(". Whole-browser limit for Firefox Nightly paused"), "{hold}");
        assert_eq!(g.snapshot.over, 0, "neither closes while paused");
    }

    #[test]
    fn closes_name_the_firefox_when_several_run() {
        let mut g = guard(Mode::Watch);
        let nightly = Proc { browser: 1, ..tab(101, 500) };
        run_two(&mut g, &[tab(1, 500), nightly], Instant::now());
        g.close_by_user(101, &mut |_| {}).unwrap();
        assert_eq!(g.events().next().unwrap().text, "You closed Firefox Nightly tab process 101 (0.5 GB)");

        // And what a pass reports, for the notification.
        let mut g = guard(Mode::Close);
        let nightly = Proc { browser: 1, ..tab(102, 3000) };
        let closed = g.pass(&[nightly], &[browser(0, "Firefox"), browser(1, "Firefox Nightly")], Instant::now(), &mut |_| {});
        assert_eq!(closed.unwrap().browser, "Firefox Nightly");
    }

    #[test]
    fn lists_running_and_saved_copies_for_settings() {
        let mut g = guard(Mode::Watch);
        let saved = crate::config::Limits { warn_mb: 500, kill_mb: 900, total_mb: 0, total_min_victim_mb: 400 };
        g.apply_config(Config {
            browser_limits: [
                ("/Applications/Firefox Nightly.app".to_string(), saved),
                ("/Applications/Firefox Beta.app".to_string(), saved),
            ]
            .into(),
            ..g.config.clone()
        });
        g.pass(&[], &[browser(0, "Firefox"), browser(1, "Firefox Nightly")], Instant::now(), &mut |_| {});
        let view = g.view();
        let t: Vec<(&str, bool, bool)> = view.limit_targets.iter().map(|t| (t.name.as_str(), t.running, t.own)).collect();
        assert_eq!(t, [("Firefox", true, false), ("Firefox Nightly", true, true), ("Firefox Beta", false, true)]);
    }

    #[test]
    fn local_time_looks_like_a_timestamp() {
        let s = local_time(1_793_065_258);
        assert_eq!(s.len(), 19);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[13..14], ":");
    }
}
