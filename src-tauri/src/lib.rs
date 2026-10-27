//! Tab Guard: a menu-bar app that watches Firefox's tab processes and closes
//! the ones that grow past your limits.

mod config;
mod engine;
mod sampler;

use config::{Config, Mode};
use engine::{gb, Row, Status};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

const MAX_EVENTS: usize = 300;

#[derive(Debug, Clone, Serialize)]
struct Event {
    /// Unix seconds.
    at: u64,
    /// "closed", "would-close", "warn", "info"
    kind: &'static str,
    text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Snapshot {
    firefox_running: bool,
    total_mb: u64,
    tab_count: usize,
    flagged: usize,
    over: usize,
    rows: Vec<Row>,
    /// Seconds until another close is allowed, if a cooldown is running.
    cooldown_secs: u64,
    checked_at: u64,
}

struct Inner {
    config: Config,
    config_dir: PathBuf,
    log_path: PathBuf,
    spared: HashSet<u32>,
    events: VecDeque<Event>,
    /// (pid, status) pairs already written to the activity log.
    logged: HashSet<(u32, Status)>,
    last_close: Option<Instant>,
    snapshot: Snapshot,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Mutex<Sender<()>>,
}

impl Shared {
    fn wake(&self) {
        let _ = self.wake.lock().unwrap().send(());
    }
}

impl Inner {
    fn push_event(&mut self, kind: &'static str, text: String) {
        let at = unix_now();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = writeln!(f, "{} {kind}: {text}", local_time(at));
        }
        self.events.push_front(Event { at, kind, text });
        self.events.truncate(MAX_EVENTS);
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "2026-10-26 21:30:05" in the Mac's time zone, for the log file.
fn local_time(unix: u64) -> String {
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

/// Ask a tab process to quit, then force it if it's still there. Firefox
/// shows the tab as crashed, with a Restore button.
fn close_process(pid: u32) {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    });
}

fn notify(text: &str) {
    let script = format!(
        "display notification \"{}\" with title \"Tab Guard\"",
        text.replace('\\', "").replace('"', "'")
    );
    let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
}

// ---------------------------------------------------------------- tray menu

#[derive(Clone)]
struct TrayItems {
    tray: TrayIcon,
    summary: MenuItem<tauri::Wry>,
    detail: MenuItem<tauri::Wry>,
    enabled: CheckMenuItem<tauri::Wry>,
    modes: Vec<(Mode, CheckMenuItem<tauri::Wry>)>,
}

impl TrayItems {
    fn refresh(&self, cfg: &Config, snap: &Snapshot) {
        let _ = self.enabled.set_checked(cfg.enabled);
        for (mode, item) in &self.modes {
            let _ = item.set_checked(*mode == cfg.mode);
        }
        if !snap.firefox_running {
            let _ = self.summary.set_text("Firefox isn't running");
            let _ = self.detail.set_text(format!("Mode: {}", cfg.mode.label()));
            let _ = self.tray.set_title(if cfg.enabled { None } else { Some("Paused") });
            return;
        }
        let _ = self.summary.set_text(format!("Firefox: {} · {} tab processes", gb(snap.total_mb), snap.tab_count));
        let detail = match (snap.over, snap.flagged) {
            (0, 0) => "Everything's within your limits".to_string(),
            (0, n) => format!("{n} past the warning level"),
            (o, _) if !cfg.enabled || !cfg.mode.closes() => format!("{o} would be closed (not closing: {})",
                if cfg.enabled { "watch only" } else { "paused" }),
            (o, _) => format!("{o} over a limit, closing"),
        };
        let _ = self.detail.set_text(detail);
        let mark = if snap.over > 0 { "⚠︎ " } else { "" };
        let title = if cfg.enabled {
            format!("{mark}{:.1}G", snap.total_mb as f64 / 1024.0)
        } else {
            "Paused".to_string()
        };
        let _ = self.tray.set_title(Some(title));
    }
}

fn build_tray(app: &AppHandle, cfg: &Config) -> tauri::Result<TrayItems> {
    let summary = MenuItem::with_id(app, "summary", "Checking Firefox…", false, None::<&str>)?;
    let detail = MenuItem::with_id(app, "detail", "", false, None::<&str>)?;
    let enabled = CheckMenuItem::with_id(app, "enabled", "Tab Guard On", true, cfg.enabled, None::<&str>)?;
    let modes: Vec<(Mode, CheckMenuItem<tauri::Wry>)> = [Mode::Watch, Mode::WarnThenClose, Mode::Close]
        .into_iter()
        .map(|m| {
            let id = format!("mode:{}", serde_json::to_string(&m).unwrap().trim_matches('"'));
            CheckMenuItem::with_id(app, id, m.label(), true, m == cfg.mode, None::<&str>).map(|i| (m, i))
        })
        .collect::<tauri::Result<_>>()?;
    let mode_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        modes.iter().map(|(_, i)| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>).collect();
    let mode_menu = Submenu::with_items(app, "Mode", true, &mode_refs)?;
    let open = MenuItem::with_id(app, "open", "Open Tab Guard…", true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check", "Check Now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Tab Guard", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(
        app,
        &[&summary, &detail, &sep()?, &enabled, &mode_menu, &sep()?, &open, &check, &sep()?, &quit],
    )?;

    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let tray = TrayIconBuilder::with_id("tab-guard")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Tab Guard")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let shared = app.state::<Arc<Shared>>();
            let id = event.id.as_ref();
            match id {
                "open" => show_window(app),
                "check" => shared.wake(),
                "quit" => app.exit(0),
                "enabled" => update_config(app, &shared, |c| c.enabled = !c.enabled),
                _ => {
                    if let Some(mode) = id.strip_prefix("mode:") {
                        if let Ok(m) = serde_json::from_str::<Mode>(&format!("\"{mode}\"")) {
                            update_config(app, &shared, |c| c.mode = m);
                        }
                    }
                }
            }
        })
        .build(app)?;

    Ok(TrayItems { tray, summary, detail, enabled, modes })
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    app.state::<Arc<Shared>>().wake();
}

fn update_config(app: &AppHandle, shared: &Shared, change: impl FnOnce(&mut Config)) {
    {
        let mut inner = shared.inner.lock().unwrap();
        let mut cfg = inner.config.clone();
        change(&mut cfg);
        apply_config(&mut inner, cfg);
        if let Some(items) = app.try_state::<TrayItems>() {
            items.refresh(&inner.config, &inner.snapshot);
        }
    }
    shared.wake();
}

fn apply_config(inner: &mut Inner, cfg: Config) {
    let cfg = cfg.sanitized();
    let old = inner.config.clone();
    if old.enabled != cfg.enabled {
        inner.push_event("info", if cfg.enabled { "Tab Guard turned on".into() } else { "Tab Guard paused".into() });
    }
    if old.mode != cfg.mode {
        inner.push_event("info", format!("Mode changed to “{}”", cfg.mode.label()));
    }
    if let Err(e) = cfg.save(&inner.config_dir) {
        inner.push_event("info", format!("Couldn't save settings: {e}"));
    }
    inner.config = cfg;
    // Re-log flags under the new rules.
    inner.logged.clear();
}

// ---------------------------------------------------------------- monitor loop

fn monitor(app: AppHandle, shared: Arc<Shared>, wake: Receiver<()>) {
    let mut sampler = sampler::Sampler::new();
    // CPU use is measured between two samples, so prime it before judging
    // which tabs are idle.
    sampler.sample();
    std::thread::sleep(Duration::from_secs(1));
    loop {
        let procs = sampler.sample();
        let interval = {
            let mut inner = shared.inner.lock().unwrap();
            let cfg = inner.config.clone();
            let cooldown = Duration::from_secs(cfg.kill_cooldown_secs);
            let cooldown_left = inner.last_close.map(|t| cooldown.saturating_sub(t.elapsed())).unwrap_or_default();
            let eval = engine::evaluate(&cfg, &procs, &inner.spared, cooldown_left.is_zero());

            // Forget processes that are gone.
            let alive: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
            inner.spared.retain(|p| alive.contains(p));
            inner.logged.retain(|(p, _)| alive.contains(p));

            for row in &eval.rows {
                if !matches!(row.status, Status::Warn | Status::Over) || inner.logged.contains(&(row.proc.pid, row.status)) {
                    continue;
                }
                if eval.close.as_ref().is_some_and(|(pid, _)| *pid == row.proc.pid) {
                    continue; // logged as "closed" below
                }
                inner.logged.insert((row.proc.pid, row.status));
                let reason = row.reason.clone().unwrap_or_default();
                let (kind, verb) = match (row.status, row.action.as_deref()) {
                    (Status::Over, Some("Would close")) => ("would-close", "Would close"),
                    (Status::Over, _) => ("warn", "Queued to close"),
                    _ => ("warn", "Flagged"),
                };
                inner.push_event(kind, format!("{verb} tab process {} ({}): {reason}", row.proc.pid, gb(row.proc.mem_mb)));
            }

            if let Some((pid, reason)) = &eval.close {
                let mb = procs.iter().find(|p| p.pid == *pid).map(|p| p.mem_mb).unwrap_or(0);
                close_process(*pid);
                inner.last_close = Some(Instant::now());
                inner.push_event("closed", format!("Closed tab process {pid} ({}): {reason}", gb(mb)));
                if cfg.notify_on_close {
                    notify(&format!("Closed a Firefox tab using {}. Click Restore on the crashed tab to reload it.", gb(mb)));
                }
            }

            let tabs = eval.rows.iter().filter(|r| r.proc.kind == sampler::Kind::Tab);
            inner.snapshot = Snapshot {
                firefox_running: !procs.is_empty(),
                total_mb: eval.total_mb,
                tab_count: tabs.clone().count(),
                flagged: tabs.clone().filter(|r| r.status == Status::Warn).count(),
                over: tabs.filter(|r| r.status == Status::Over).count(),
                rows: eval.rows,
                cooldown_secs: inner
                    .last_close
                    .map(|t| cooldown.saturating_sub(t.elapsed()).as_secs())
                    .unwrap_or(0),
                checked_at: unix_now(),
            };
            if let Some(items) = app.try_state::<TrayItems>() {
                items.refresh(&inner.config, &inner.snapshot);
            }

            let visible = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
            Duration::from_secs(if visible { 3 } else { cfg.interval_secs })
        };
        // Sleep until the next check, or until something wakes us early.
        let _ = wake.recv_timeout(interval);
        while wake.try_recv().is_ok() {}
    }
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
struct View {
    config: Config,
    snapshot: Snapshot,
    events: Vec<Event>,
    spared: Vec<u32>,
    log_path: String,
}

#[tauri::command]
fn get_view(shared: State<'_, Arc<Shared>>) -> View {
    let inner = shared.inner.lock().unwrap();
    View {
        config: inner.config.clone(),
        snapshot: inner.snapshot.clone(),
        events: inner.events.iter().cloned().collect(),
        spared: inner.spared.iter().copied().collect(),
        log_path: inner.log_path.to_string_lossy().into_owned(),
    }
}

#[tauri::command]
fn save_config(app: AppHandle, shared: State<'_, Arc<Shared>>, config: Config) {
    update_config(&app, &shared, |c| *c = config);
}

#[tauri::command]
fn close_tab(shared: State<'_, Arc<Shared>>, pid: u32) -> Result<(), String> {
    {
        let mut inner = shared.inner.lock().unwrap();
        let row = inner
            .snapshot
            .rows
            .iter()
            .find(|r| r.proc.pid == pid && r.proc.kind == sampler::Kind::Tab)
            .cloned()
            .ok_or("That tab process is gone, or isn't a Firefox tab")?;
        close_process(pid);
        inner.push_event("closed", format!("You closed tab process {pid} ({})", gb(row.proc.mem_mb)));
    }
    shared.wake();
    Ok(())
}

#[tauri::command]
fn set_spared(shared: State<'_, Arc<Shared>>, pid: u32, spared: bool) {
    {
        let mut inner = shared.inner.lock().unwrap();
        if spared {
            inner.spared.insert(pid);
            inner.push_event("info", format!("Sparing tab process {pid} until it exits"));
        } else {
            inner.spared.remove(&pid);
            inner.push_event("info", format!("Stopped sparing tab process {pid}"));
        }
    }
    shared.wake();
}

#[tauri::command]
fn check_now(shared: State<'_, Arc<Shared>>) {
    shared.wake();
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let m = app.autolaunch();
    if enabled { m.enable() } else { m.disable() }.map_err(|e| e.to_string())?;
    Ok(m.is_enabled().unwrap_or(false))
}

/// Firefox's own page that names the site in each process.
#[tauri::command]
fn open_about_processes() -> Result<(), String> {
    std::process::Command::new("open")
        .args(["-a", "Firefox", "about:processes"])
        .status()
        .map_err(|e| e.to_string())
        .and_then(|s| if s.success() { Ok(()) } else { Err("Firefox wouldn't open it".into()) })
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            get_view,
            save_config,
            close_tab,
            set_spared,
            check_now,
            get_autostart,
            set_autostart,
            open_about_processes
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_dir = app.path().app_config_dir()?;
            let first_run = !Config::path(&config_dir).exists();
            let config = Config::load(&config_dir);
            if first_run {
                let _ = config.save(&config_dir);
                // Start at login unless the user turns it off in settings.
                let _ = app.autolaunch().enable();
            }
            let log_dir = app.path().home_dir()?.join("Library/Logs");
            let _ = std::fs::create_dir_all(&log_dir);

            let (tx, rx) = mpsc::channel();
            let shared = Arc::new(Shared {
                inner: Mutex::new(Inner {
                    config: config.clone(),
                    config_dir,
                    log_path: log_dir.join("Tab Guard.log"),
                    spared: HashSet::new(),
                    events: VecDeque::new(),
                    logged: HashSet::new(),
                    last_close: None,
                    snapshot: Snapshot::default(),
                }),
                wake: Mutex::new(tx),
            });
            shared.inner.lock().unwrap().push_event("info", format!("Started in “{}” mode", config.mode.label()));
            app.manage(shared.clone());

            let items = build_tray(app.handle(), &config)?;
            app.manage(items);

            // Closing the window hides it; Tab Guard keeps running in the menu bar.
            if let Some(w) = app.get_webview_window("main") {
                let w2 = w.clone();
                w.on_window_event(move |e| {
                    if let WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = w2.hide();
                    }
                });
            }
            if first_run {
                show_window(app.handle());
            }

            let handle = app.handle().clone();
            std::thread::spawn(move || monitor(handle, shared, rx));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tab Guard");
}
