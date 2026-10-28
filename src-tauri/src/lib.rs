//! Tab Guard: a menu-bar app that watches Firefox's tab processes and closes
//! the ones that grow past your limits.

mod config;
mod engine;
mod guard;
mod sampler;
mod usage;

use config::{Config, Mode};
use engine::gb;
use guard::{local_time, unix_now, Guard, Snapshot, View};
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
    /// The latest update check, in words, for the settings tab.
    update_status: Mutex<String>,
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
    let about = MenuItem::with_id(app, "about", "About Tab Guard", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Tab Guard", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(
        app,
        &[&summary, &detail, &sep()?, &enabled, &mode_menu, &sep()?, &open, &about, &sep()?, &quit],
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
                "open" => show_window(app, None),
                "about" => show_window(app, Some("about")),
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

const MIN_HEIGHT: f64 = 480.0;

/// Open the window, creating it if needed, optionally on one of its tabs
/// ("about"). It's destroyed again when closed, so its web view isn't holding
/// memory while Tab Guard sits in the menu bar.
fn show_window(app: &AppHandle, panel: Option<&'static str>) {
    let shared = app.state::<Arc<Shared>>();
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if let Some(p) = panel {
            let _ = w.eval(format!("window.showPanel && showPanel('{p}')"));
        }
    } else {
        let page = panel.map_or("index.html".to_string(), |p| format!("index.html#{p}"));
        let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()))
            .title("Tab Guard")
            .inner_size(980.0, 680.0)
            .min_inner_size(720.0, MIN_HEIGHT)
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
        let (cfg, snap, closed, counted) = {
            let mut g = shared.guard.lock().unwrap();
            let closed = g.pass(&procs, Instant::now(), &mut close_process);
            (g.config.clone(), g.snapshot.clone(), closed, g.take_usage())
        };
        if cfg.share_usage {
            usage::send(counted.into_iter().map(usage::describe).collect());
        }
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

/// What the window shows: the guard's state plus app-level details.
#[derive(serde::Serialize)]
struct WindowView {
    #[serde(flatten)]
    view: View,
    version: String,
    usage_available: bool,
    update_status: String,
}

#[tauri::command]
fn get_view(app: AppHandle, shared: State<'_, Arc<Shared>>) -> WindowView {
    let view = shared.guard.lock().unwrap().view();
    WindowView {
        view,
        version: app.package_info().version.to_string(),
        usage_available: usage::available(),
        update_status: shared.update_status.lock().unwrap().clone(),
    }
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
/// The only pages the window can open, in the default browser.
const LINKS: [(&str, &str); 6] = [
    ("project", "https://allenbw.github.io/tab-guard/"),
    ("source", "https://github.com/AllenBW/tab-guard"),
    ("changelog", "https://github.com/AllenBW/tab-guard/blob/main/CHANGELOG.md"),
    ("privacy", "https://github.com/AllenBW/tab-guard/blob/main/PRIVACY.md"),
    ("issues", "https://github.com/AllenBW/tab-guard/issues"),
    ("walk", "https://allenmanzanowight.com/walk?ref=tab-guard"),
];

fn link_url(name: &str) -> Option<&'static str> {
    LINKS.iter().find(|(n, _)| *n == name).map(|(_, url)| *url)
}

#[tauri::command]
fn open_link(name: String) -> Result<(), String> {
    let url = link_url(&name).ok_or("Unknown link")?;
    std::process::Command::new("open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

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

// ---------------------------------------------------------------- updates

/// Check for a newer release and install it. Returns the new version, if any.
async fn install_update(app: &AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        // No release manifest on GitHub (nothing published yet) means
        // there's nothing newer to install.
        Ok(None) | Err(tauri_plugin_updater::Error::ReleaseNotFound) => return Ok(None),
        Err(e) => return Err(update_error(&e)),
    };
    let version = update.version.clone();
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    Ok(Some(version))
}

/// Words for a failed check. Network trouble is the usual cause, and the
/// updater's own messages are written for developers.
fn update_error(e: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error::*;
    match e {
        Reqwest(_) | Network(_) => "no connection to GitHub. Tab Guard will try again later.".into(),
        e => e.to_string(),
    }
}

fn set_update_status(shared: &Shared, text: String) {
    *shared.update_status.lock().unwrap() = text;
}

/// Run one check and report it. After an install, restart into the new
/// version: right away when the user asked, otherwise once the window is
/// closed so it doesn't vanish under them.
async fn check_for_updates_now(app: &AppHandle, shared: &Arc<Shared>, by_user: bool) -> Result<String, String> {
    let current = app.package_info().version.to_string();
    let at = || local_time(unix_now())[11..16].to_string();
    match install_update(app).await {
        Ok(None) => {
            let text = format!("Tab Guard {current} is the latest version (checked {})", at());
            set_update_status(shared, text.clone());
            Ok(text)
        }
        Ok(Some(version)) => {
            shared.guard.lock().unwrap().push_event("info", format!("Updated from {current} to {version}"));
            let text = format!("Installed Tab Guard {version}. Restarting…");
            set_update_status(shared, text.clone());
            let (app, shared) = (app.clone(), shared.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                while !by_user && shared.window_open.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(10));
                }
                app.restart();
            });
            Ok(text)
        }
        Err(e) => {
            let text = format!("Couldn't check for updates: {e}");
            set_update_status(shared, text.clone());
            Err(text)
        }
    }
}

/// A minute after launch, then every 6 hours, while automatic updates are on.
fn update_loop(app: AppHandle, shared: Arc<Shared>) {
    std::thread::sleep(Duration::from_secs(60));
    loop {
        if shared.guard.lock().unwrap().config.auto_update {
            let _ = tauri::async_runtime::block_on(check_for_updates_now(&app, &shared, false));
        }
        std::thread::sleep(Duration::from_secs(6 * 3600));
    }
}

/// Make the window tall enough for `height` points of content, up to the
/// height of the screen, and keep it on screen. The page measures Settings
/// and About and calls this once when it loads.
#[tauri::command]
fn fit_window(window: tauri::WebviewWindow, height: f64) {
    let Ok(Some(monitor)) = window.current_monitor() else { return };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let (top, avail) = (area.position.y as f64 / scale, area.size.height as f64 / scale);
    let Ok(size) = window.inner_size() else { return };
    let Ok(outer) = window.outer_size() else { return };
    let title_bar = (outer.height as f64 - size.height as f64) / scale;
    let height = fit_height(height, avail - title_bar);
    let _ = window.set_size(tauri::LogicalSize::new(size.width as f64 / scale, height));
    if let Ok(pos) = window.outer_position() {
        let y = pos.y as f64 / scale;
        if y + title_bar + height > top + avail {
            let x = pos.x as f64 / scale;
            let _ = window.set_position(tauri::LogicalPosition::new(x, top.max(top + avail - title_bar - height)));
        }
    }
}

/// The window's content height: what the page needs, no taller than the
/// screen allows and no shorter than the minimum.
fn fit_height(wanted: f64, available: f64) -> f64 {
    wanted.ceil().min(available).max(MIN_HEIGHT)
}

#[tauri::command]
async fn check_for_updates(app: AppHandle, shared: State<'_, Arc<Shared>>) -> Result<String, String> {
    check_for_updates_now(&app, shared.inner(), true).await
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_view,
            save_config,
            close_tab,
            set_spared,
            get_autostart,
            set_autostart,
            open_about_processes,
            open_link,
            check_for_updates,
            fit_window
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
            let shared = Arc::new(Shared {
                guard: Mutex::new(guard),
                wake: Mutex::new(tx),
                window_open: AtomicBool::new(false),
                update_status: Mutex::new(String::new()),
            });
            app.manage(shared.clone());
            app.manage(build_tray(app.handle(), &config)?);

            // The welcome card asks about usage sharing, so show it until
            // it's been answered, including on installs from before it existed.
            if first_run || !config.welcomed {
                show_window(app.handle(), None);
            }
            if config.share_usage {
                usage::send(vec![usage::started(config.mode)]);
            }
            let handle = app.handle().clone();
            let for_updates = shared.clone();
            std::thread::spawn(move || monitor(handle, shared, rx));
            let handle = app.handle().clone();
            std::thread::spawn(move || update_loop(handle, for_updates));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tab Guard");

    app.run(|app, event| match event {
        // Closing the last window would normally quit; Tab Guard lives in the
        // menu bar, so only an explicit Quit (which passes an exit code) does.
        RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
        // Opening Tab Guard again (from Spotlight or Applications) while it's
        // running shows the window.
        RunEvent::Reopen { .. } => show_window(app, None),
        _ => {}
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
    fn only_listed_links_open() {
        assert_eq!(link_url("walk"), Some("https://allenmanzanowight.com/walk?ref=tab-guard"));
        assert_eq!(link_url("https://evil.example"), None);
        assert!(LINKS.iter().all(|(_, url)| url.starts_with("https://")));
    }

    #[test]
    fn window_fits_content_within_the_screen() {
        assert_eq!(fit_height(812.4, 1100.0), 813.0);
        assert_eq!(fit_height(2400.0, 1100.0), 1100.0, "never taller than the screen");
        assert_eq!(fit_height(300.0, 1100.0), MIN_HEIGHT);
    }

    #[test]
    fn network_trouble_reads_plainly() {
        let e = tauri_plugin_updater::Error::Network("timed out".into());
        assert_eq!(update_error(&e), "no connection to GitHub. Tab Guard will try again later.");
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
