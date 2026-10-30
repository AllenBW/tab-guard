//! Tab Guard: a menu-bar app that watches Firefox's tab processes and closes
//! the ones that grow past your limits.

mod config;
mod engine;
mod guard;
mod sampler;

use config::{Config, Mode};
use engine::gb;
use guard::{Guard, Snapshot, View};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

struct Shared {
    guard: Mutex<Guard>,
    wake: Mutex<Sender<()>>,
    /// Tracked here so the monitor never has to ask the window (a main-thread call).
    window_open: AtomicBool,
}

impl Shared {
    fn wake(&self) {
        let _ = self.wake.lock().unwrap().send(());
    }
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

/// The menu-bar title and the two status lines in its menu.
fn tray_text(cfg: &Config, snap: &Snapshot) -> (Option<String>, String, String) {
    if !snap.firefox_running {
        let title = (!cfg.enabled).then(|| "Paused".to_string());
        return (title, "Firefox isn't running".into(), format!("Mode: {}", cfg.mode.label()));
    }
    let summary = format!("Firefox: {} · {} tab processes", gb(snap.total_mb), snap.tab_count);
    let detail = match (snap.over, snap.flagged) {
        // "Whole-browser limit paused until 08:31", without the why.
        (0, 0) if cfg.enabled && snap.total_hold.is_some() => {
            let hold = snap.total_hold.as_deref().unwrap_or_default();
            hold.split_once(": ").map_or(hold, |(short, _)| short).to_string()
        }
        (0, 0) => "Everything's within your limits".to_string(),
        (0, n) => format!("{n} past the warning level"),
        (o, _) if !cfg.enabled => format!("{o} would be closed (paused)"),
        (o, _) if !cfg.mode.closes() => format!("{o} would be closed (watch only)"),
        (o, _) => format!("{o} over a limit, closing"),
    };
    let title = if cfg.enabled {
        let mark = if snap.over > 0 { "⚠︎ " } else { "" };
        format!("{mark}{:.1}G", snap.total_mb as f64 / 1024.0)
    } else {
        "Paused".to_string()
    };
    (Some(title), summary, detail)
}

impl TrayItems {
    fn refresh(&self, cfg: &Config, snap: &Snapshot) {
        let (title, summary, detail) = tray_text(cfg, snap);
        let _ = self.enabled.set_checked(cfg.enabled);
        for (mode, item) in &self.modes {
            let _ = item.set_checked(*mode == cfg.mode);
        }
        let _ = self.summary.set_text(summary);
        let _ = self.detail.set_text(detail);
        let _ = self.tray.set_title(title);
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
    let quit = MenuItem::with_id(app, "quit", "Quit Tab Guard", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(
        app,
        &[&summary, &detail, &sep()?, &enabled, &mode_menu, &sep()?, &open, &sep()?, &quit],
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

/// Open the window, creating it if needed. It's destroyed again when closed,
/// so its web view isn't holding memory while Tab Guard sits in the menu bar.
fn show_window(app: &AppHandle) {
    let shared = app.state::<Arc<Shared>>();
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    } else {
        let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("Tab Guard")
            .inner_size(980.0, 680.0)
            .min_inner_size(720.0, 480.0)
            .focused(true)
            .build();
        match built {
            Ok(w) => {
                let shared2 = shared.inner().clone();
                w.on_window_event(move |e| {
                    if let WindowEvent::Destroyed = e {
                        shared2.window_open.store(false, Ordering::Relaxed);
                    }
                });
            }
            Err(e) => {
                shared.guard.lock().unwrap().push_event("info", format!("Couldn't open the window: {e}"));
                return;
            }
        }
    }
    shared.window_open.store(true, Ordering::Relaxed);
    shared.wake();
}

fn update_config(app: &AppHandle, shared: &Shared, change: impl FnOnce(&mut Config)) {
    let (cfg, snap) = {
        let mut g = shared.guard.lock().unwrap();
        let mut cfg = g.config.clone();
        change(&mut cfg);
        g.apply_config(cfg);
        (g.config.clone(), g.snapshot.clone())
    };
    // Outside the lock: see the note in `monitor`.
    if let Some(items) = app.try_state::<TrayItems>() {
        items.refresh(&cfg, &snap);
    }
    shared.wake();
}

// ---------------------------------------------------------------- monitor loop

fn monitor(app: AppHandle, shared: Arc<Shared>, wake: Receiver<()>) {
    let mut sampler = sampler::Sampler::new();
    // CPU use is measured between two samples, so prime it before judging
    // which tabs are idle.
    sampler.sample();
    std::thread::sleep(Duration::from_secs(1));
    let mut last_tray: Option<(Option<String>, String, String)> = None;
    loop {
        let procs = sampler.sample();
        let (cfg, snap, closed) = {
            let mut g = shared.guard.lock().unwrap();
            let closed = g.pass(&procs, Instant::now(), &mut close_process);
            (g.config.clone(), g.snapshot.clone(), closed)
        };
        if let (Some(c), true) = (&closed, cfg.notify_on_close) {
            notify(&format!("Closed a Firefox tab using {}. Click Restore on the crashed tab to reload it.", gb(c.mem_mb)));
        }

        // Tray calls block until the main thread runs them, and the main
        // thread takes `guard` to answer the window's commands. Making them
        // while holding `guard` deadlocks the app, so they come after. Skip
        // them entirely when nothing visible changed.
        let text = tray_text(&cfg, &snap);
        if last_tray.as_ref() != Some(&text) {
            if let Some(items) = app.try_state::<TrayItems>() {
                items.refresh(&cfg, &snap);
            }
            last_tray = Some(text);
        }

        let open = shared.window_open.load(Ordering::Relaxed);
        let interval = Duration::from_secs(if open { 3 } else { cfg.interval_secs });
        // Sleep until the next check, or until something wakes us early.
        let _ = wake.recv_timeout(interval);
        while wake.try_recv().is_ok() {}
    }
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn get_view(shared: State<'_, Arc<Shared>>) -> View {
    shared.guard.lock().unwrap().view()
}

#[tauri::command]
fn save_config(app: AppHandle, shared: State<'_, Arc<Shared>>, config: Config) {
    update_config(&app, &shared, |c| *c = config);
}

#[tauri::command]
fn close_tab(shared: State<'_, Arc<Shared>>, pid: u32) -> Result<(), String> {
    shared.guard.lock().unwrap().close_by_user(pid, &mut close_process)?;
    shared.wake();
    Ok(())
}

#[tauri::command]
fn set_spared(shared: State<'_, Arc<Shared>>, pid: u32, spared: bool) {
    shared.guard.lock().unwrap().set_spared(pid, spared);
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

/// Run a command, giving up after `limit`.
fn run_with_timeout(cmd: &mut std::process::Command, limit: Duration) -> bool {
    let Ok(mut child) = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Firefox's own page that names the site in each process. `open` can't do
/// this (it doesn't know the about: scheme), so ask Firefox directly over
/// Apple Events, and put the address on the clipboard as a fallback. Async so
/// the wait runs off the main thread.
#[tauri::command]
async fn open_about_processes() -> Result<String, String> {
    let mut pbcopy = std::process::Command::new("pbcopy");
    pbcopy.stdin(std::process::Stdio::piped());
    if let Ok(mut child) = pbcopy.spawn() {
        use std::io::Write as _;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"about:processes");
        }
        let _ = child.wait();
    }
    let asked = run_with_timeout(
        std::process::Command::new("osascript").args([
            "-e",
            "with timeout of 5 seconds",
            "-e",
            "tell application \"Firefox\"",
            "-e",
            "activate",
            "-e",
            "open location \"about:processes\"",
            "-e",
            "end tell",
            "-e",
            "end timeout",
        ]),
        Duration::from_secs(7),
    );
    if asked {
        Ok("Asked Firefox to open about:processes. If no tab appeared, press ⌘L, then ⌘V and Return: the address is on your clipboard.".into())
    } else {
        let _ = run_with_timeout(std::process::Command::new("open").args(["-a", "Firefox"]), Duration::from_secs(5));
        Ok("Firefox didn't accept the request. The address is on your clipboard: in Firefox press ⌘L, then ⌘V and Return.".into())
    }
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            get_view,
            save_config,
            close_tab,
            set_spared,
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

            let mut guard = Guard::new(config.clone(), Some(config_dir), Some(log_dir.join("Tab Guard.log")));
            guard.push_event("info", format!("Started in “{}” mode", config.mode.label()));
            let (tx, rx) = mpsc::channel();
            let shared =
                Arc::new(Shared { guard: Mutex::new(guard), wake: Mutex::new(tx), window_open: AtomicBool::new(false) });
            app.manage(shared.clone());
            app.manage(build_tray(app.handle(), &config)?);

            if first_run {
                show_window(app.handle());
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || monitor(handle, shared, rx));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tab Guard");

    app.run(|_app, event| {
        // Closing the last window would normally quit; Tab Guard lives in the
        // menu bar, so only an explicit Quit (which passes an exit code) does.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Row;

    fn snap(total_mb: u64, over: usize, flagged: usize) -> Snapshot {
        Snapshot { firefox_running: true, total_mb, tab_count: 20, over, flagged, rows: Vec::<Row>::new(), ..Default::default() }
    }

    #[test]
    fn tray_title_shows_total_and_warning() {
        let cfg = Config::default();
        assert_eq!(tray_text(&cfg, &snap(9216, 0, 0)).0.as_deref(), Some("9.0G"));
        assert_eq!(tray_text(&cfg, &snap(9216, 1, 0)).0.as_deref(), Some("⚠︎ 9.0G"));
        let paused = Config { enabled: false, ..Config::default() };
        assert_eq!(tray_text(&paused, &snap(9216, 1, 0)).0.as_deref(), Some("Paused"));
    }

    #[test]
    fn tray_detail_says_whether_it_will_close() {
        let watch = Config::default();
        assert_eq!(tray_text(&watch, &snap(1, 2, 0)).2, "2 would be closed (watch only)");
        let close = Config { mode: Mode::Close, ..Config::default() };
        assert_eq!(tray_text(&close, &snap(1, 2, 0)).2, "2 over a limit, closing");
        let paused = Config { enabled: false, ..close.clone() };
        assert_eq!(tray_text(&paused, &snap(1, 2, 0)).2, "2 would be closed (paused)");
        assert_eq!(tray_text(&close, &snap(1, 0, 3)).2, "3 past the warning level");
        assert_eq!(tray_text(&close, &snap(1, 0, 0)).2, "Everything's within your limits");
    }

    #[test]
    fn tray_detail_mentions_a_paused_whole_browser_limit() {
        let close = Config { mode: Mode::Close, ..Config::default() };
        let held = Snapshot {
            total_hold: Some("Whole-browser limit paused until 08:31: closing a 0.9 GB tab didn't help".into()),
            ..snap(9216, 0, 0)
        };
        assert_eq!(tray_text(&close, &held).2, "Whole-browser limit paused until 08:31");
        // A tab over its own limit still comes first.
        assert_eq!(tray_text(&close, &Snapshot { over: 1, ..held }).2, "1 over a limit, closing");
    }

    #[test]
    fn tray_without_firefox() {
        let none = Snapshot::default();
        let (title, summary, _) = tray_text(&Config::default(), &none);
        assert_eq!(title, None);
        assert_eq!(summary, "Firefox isn't running");
    }

    #[test]
    fn timeout_kills_slow_commands() {
        let t = Instant::now();
        assert!(!run_with_timeout(std::process::Command::new("sleep").arg("5"), Duration::from_millis(200)));
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(run_with_timeout(&mut std::process::Command::new("true"), Duration::from_secs(2)));
        assert!(!run_with_timeout(&mut std::process::Command::new("false"), Duration::from_secs(2)));
    }
}
