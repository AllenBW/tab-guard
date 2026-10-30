//! Tab Guard: a menu-bar app that watches browsers' tab processes (Firefox
//! and Chrome, and the browsers built on them) and closes the ones that grow
//! past your limits.

mod config;
mod engine;
mod guard;
mod sampler;

use config::Config;
use engine::gb;
use guard::{local_time, next_morning, pause_end_text, unix_now, Guard, Snapshot, View};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
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

/// Ask a tab process to quit, then force it if it's still there. The
/// browser shows the tab as crashed, with a button to reload it.
fn close_process(pid: u32) {
    let start = sampler::proc_start(pid);
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        // Only the same process: the pid could have gone to something else.
        if sampler::proc_start(pid).is_some_and(|s| Some(s) == start) {
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

/// What the menu-bar shield's color says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayState {
    /// Tab Guard is paused: the shield is struck through.
    Off,
    /// Watch only, nothing to look at: the plain menu-bar icon.
    Watching,
    /// A closing mode, everything within limits: green.
    Guarding,
    /// Worth a look: tabs past the warning level, a tab Watch only would
    /// close, or the whole-browser limit paused: orange.
    Attention,
    /// Closing a tab that's over a limit: red.
    Closing,
}

impl TrayState {
    /// The shield's color, or None for the plain icon that follows the menu bar's.
    fn color(self) -> Option<[u8; 3]> {
        match self {
            TrayState::Off | TrayState::Watching => None,
            TrayState::Guarding => Some([48, 209, 88]),
            TrayState::Attention => Some([255, 159, 10]),
            TrayState::Closing => Some([255, 69, 58]),
        }
    }

    /// The same, as a dot in front of the menu's status line.
    fn dot(self) -> &'static str {
        match self {
            TrayState::Off => "⏸",
            TrayState::Watching => "⚪️",
            TrayState::Guarding => "🟢",
            TrayState::Attention => "🟠",
            TrayState::Closing => "🔴",
        }
    }
}

fn tray_state(cfg: &Config, snap: &Snapshot) -> TrayState {
    let closes = cfg.mode.closes();
    if !cfg.enabled {
        TrayState::Off
    } else if snap.over > 0 && closes {
        TrayState::Closing
    } else if snap.over > 0 || snap.flagged > 0 || (closes && snap.total_hold.is_some()) {
        TrayState::Attention
    } else if closes {
        TrayState::Guarding
    } else {
        TrayState::Watching
    }
}

/// The menu-bar icon with a slash through it, for paused. The slash has a
/// clear gap on each side so it reads at menu-bar size.
fn slashed(base: &Image<'_>) -> Image<'static> {
    let (w, h) = (base.width() as f64, base.height() as f64);
    // From the top left to the bottom right, a little inside the edges.
    let (x0, y0, x1, y1) = (0.18 * w, 0.12 * h, 0.82 * w, 0.88 * h);
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    let (line, gap) = (0.045 * w, 0.09 * w);
    let mut rgba = base.rgba().to_vec();
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i as f64 % w) + 0.5, (i as f64 / w).floor() + 0.5);
        let t = (((x - x0) * dx + (y - y0) * dy) / (len * len)).clamp(0.0, 1.0);
        let d = ((x - x0 - t * dx).powi(2) + (y - y0 - t * dy).powi(2)).sqrt();
        if d <= line {
            px.copy_from_slice(&[0, 0, 0, 255]);
        } else if d <= gap {
            px[3] = 0;
        }
    }
    Image::new_owned(rgba, base.width(), base.height())
}

/// The menu-bar icon (black plus transparency) in one color.
fn tinted(base: &Image<'_>, [r, g, b]: [u8; 3]) -> Image<'static> {
    let mut rgba = base.rgba().to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        px[..3].copy_from_slice(&[r, g, b]);
    }
    Image::new_owned(rgba, base.width(), base.height())
}

#[derive(Clone)]
struct TrayItems {
    tray: TrayIcon,
    menu: Menu<tauri::Wry>,
    /// The plain icon, to tint or strike through for each state.
    icon: Image<'static>,
    status: MenuItem<tauri::Wry>,
    sizes: MenuItem<tauri::Wry>,
    /// Pause › while on, Resume Tab Guard while paused: one is in the menu.
    pause: Submenu<tauri::Wry>,
    resume: MenuItem<tauri::Wry>,
    showing_resume: Arc<AtomicBool>,
}

/// Where Pause › or Resume Tab Guard sits: after the two status lines and a separator.
const PAUSE_POSITION: usize = 3;

/// "Paused 42m" or "Paused 1h 5m" in the menu bar while a timed pause
/// runs: the time left, rounded up to the minute.
fn pause_countdown(until: u64, now_unix: u64) -> String {
    let mins = until.saturating_sub(now_unix).div_ceil(60);
    match (mins / 60, mins % 60) {
        (0, m) => format!("Paused {m}m"),
        (h, 0) => format!("Paused {h}h"),
        (h, m) => format!("Paused {h}h {m}m"),
    }
}

/// "1 tab process", "3 tab processes".
fn tab_processes(n: usize) -> String {
    format!("{n} tab process{}", if n == 1 { "" } else { "es" })
}

/// The menu-bar title, the menu's status line, and the line of sizes.
type TrayText = (Option<String>, String, String);

fn tray_text(cfg: &Config, snap: &Snapshot, now_unix: u64) -> TrayText {
    if !cfg.enabled {
        let (title, status) = match cfg.paused_until {
            Some(until) => (pause_countdown(until, now_unix), format!("Paused until {}", pause_end_text(until, now_unix))),
            None => ("Paused".to_string(), "Paused until you turn it back on".to_string()),
        };
        let sizes = if snap.browser_running { sizes_line(snap) } else { "No browser is running".to_string() };
        return (Some(title), status, sizes);
    }
    if !snap.browser_running {
        return (None, "No browser is running".into(), format!("Mode: {}", cfg.mode.label()));
    }
    let detail = match (snap.over, snap.flagged) {
        // "Whole-browser limit paused until 08:31", without the why.
        (0, 0) if snap.total_hold.is_some() => {
            let hold = snap.total_hold.as_deref().unwrap_or_default();
            hold.split_once(": ").map_or(hold, |(short, _)| short).to_string()
        }
        (0, 0) => "Everything's within your limits".to_string(),
        (0, n) => format!("{} past the warning level", tab_processes(n)),
        (o, _) if !cfg.mode.closes() => format!("Watch only: {} would be closed", tab_processes(o)),
        (o, _) => format!("{} over a limit, closing one at a time", tab_processes(o)),
    };
    let mark = if snap.over > 0 { "⚠︎ " } else { "" };
    let title = format!("{mark}{:.1}G", snap.total_mb as f64 / 1024.0);
    (Some(title), detail, sizes_line(snap))
}

fn sizes_line(snap: &Snapshot) -> String {
    // "Firefox: 9.2 GB", or "Firefox 7.1 GB · Google Chrome 2.3 GB".
    let sizes = match snap.browsers.as_slice() {
        [one] => format!("{}: {}", one.name, gb(snap.total_mb)),
        [] => format!("Browsers: {}", gb(snap.total_mb)),
        _ => snap.browsers.iter().map(|b| format!("{} {}", b.name, gb(b.total_mb))).collect::<Vec<_>>().join(" · "),
    };
    format!("{sizes} · {} tab processes", snap.tab_count)
}

impl TrayItems {
    fn refresh(&self, cfg: &Config, snap: &Snapshot) {
        let (title, status, sizes) = tray_text(cfg, snap, unix_now());
        let state = tray_state(cfg, snap);
        let _ = match (state, state.color()) {
            (TrayState::Off, _) => self.tray.set_icon(Some(slashed(&self.icon))),
            (_, Some(color)) => self.tray.set_icon(Some(tinted(&self.icon, color))),
            (_, None) => self.tray.set_icon(Some(self.icon.clone())),
        };
        // Set after the icon: a plain one follows the menu bar's light or dark.
        let _ = self.tray.set_icon_as_template(state.color().is_none());
        let _ = self.status.set_text(format!("{} {status}", state.dot()));
        let _ = self.sizes.set_text(sizes);
        let _ = self.tray.set_title(title);

        let paused = !cfg.enabled;
        if self.showing_resume.swap(paused, Ordering::Relaxed) != paused {
            if paused {
                let _ = self.menu.remove(&self.pause);
                let _ = self.menu.insert(&self.resume, PAUSE_POSITION);
            } else {
                let _ = self.menu.remove(&self.resume);
                let _ = self.menu.insert(&self.pause, PAUSE_POSITION);
            }
        }
    }
}

/// The menu: what's happening, Pause › (or Resume), Open, Quit. Modes,
/// limits and About live in the window.
fn build_tray(app: &AppHandle, cfg: &Config) -> tauri::Result<TrayItems> {
    let status = MenuItem::with_id(app, "status", "Checking your browsers…", false, None::<&str>)?;
    let sizes = MenuItem::with_id(app, "sizes", "", false, None::<&str>)?;
    let pause = Submenu::with_items(
        app,
        "Pause",
        true,
        &[
            &MenuItem::with_id(app, "pause:hour", "For 1 hour", true, None::<&str>)?,
            &MenuItem::with_id(app, "pause:tomorrow", "Until 6 in the morning", true, None::<&str>)?,
            &MenuItem::with_id(app, "pause:off", "Until I turn it back on", true, None::<&str>)?,
        ],
    )?;
    let resume = MenuItem::with_id(app, "resume", "Resume Tab Guard", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Tab Guard…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Tab Guard", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let menu = Menu::with_items(app, &[&status, &sizes, &sep()?, &pause, &open, &sep()?, &quit])?;

    let icon = Image::from_bytes(include_bytes!("../icons/tray.png"))?.to_owned();
    let tray = TrayIconBuilder::with_id("tab-guard")
        .icon(icon.clone())
        .icon_as_template(true)
        .tooltip("Tab Guard")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let shared = app.state::<Arc<Shared>>();
            let id = event.id.as_ref();
            let pause_until = |until: Option<u64>| {
                move |c: &mut Config| {
                    c.enabled = false;
                    c.paused_until = until;
                }
            };
            match id {
                "open" => show_window(app),
                "quit" => app.exit(0),
                "pause:hour" => update_config(app, &shared, pause_until(Some(unix_now() + 3600))),
                "pause:tomorrow" => update_config(app, &shared, pause_until(Some(next_morning(unix_now())))),
                "pause:off" => update_config(app, &shared, pause_until(None)),
                "resume" => update_config(app, &shared, |c| c.enabled = true),
                _ => {}
            }
        })
        .build(app)?;

    let showing_resume = Arc::new(AtomicBool::new(false));
    let items = TrayItems { tray, menu, icon, status, sizes, pause, resume, showing_resume };
    items.refresh(cfg, &Snapshot::default());
    Ok(items)
}

const MIN_HEIGHT: f64 = 480.0;

/// Open the window, creating it if needed. It's destroyed again when
/// closed, so its web view isn't holding memory while Tab Guard sits in the
/// menu bar.
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
    // What the menu bar shows: its text, and the shield's state.
    let mut last_tray: Option<(TrayText, TrayState)> = None;
    loop {
        let (procs, browsers) = sampler.sample();
        let (cfg, snap, closed) = {
            let mut g = shared.guard.lock().unwrap();
            g.resume_if_due(unix_now());
            let closed = g.pass(&procs, &browsers, Instant::now(), &mut close_process);
            (g.config.clone(), g.snapshot.clone(), closed)
        };
        if let (Some(c), true) = (&closed, cfg.notify_on_close) {
            notify(&format!(
                "Closed a {} tab process using {}. Its tabs show as crashed, with a button to reload them.",
                c.browser,
                gb(c.mem_mb)
            ));
        }

        // Tray calls block until the main thread runs them, and the main
        // thread takes `guard` to answer the window's commands. Making them
        // while holding `guard` deadlocks the app, so they come after. Skip
        // them entirely when nothing visible changed.
        let text = (tray_text(&cfg, &snap, unix_now()), tray_state(&cfg, &snap));
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
    update_status: String,
}

#[tauri::command]
fn get_view(app: AppHandle, shared: State<'_, Arc<Shared>>) -> WindowView {
    let view = shared.guard.lock().unwrap().view();
    WindowView {
        view,
        version: app.package_info().version.to_string(),
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

/// The only pages the window can open, in the default browser.
const LINKS: [(&str, &str); 6] = [
    ("project", "https://allenbw.github.io/tab-guard/"),
    ("source", "https://github.com/AllenBW/tab-guard"),
    ("changelog", "https://github.com/AllenBW/tab-guard/blob/main/CHANGELOG.md"),
    ("privacy", "https://github.com/AllenBW/tab-guard/blob/main/PRIVACY.md"),
    ("issues", "https://github.com/AllenBW/tab-guard/issues"),
    ("walk", "https://allenmanzanowight.com/walk/?ref=tab-guard-app"),
];

fn link_url(name: &str) -> Option<&'static str> {
    LINKS.iter().find(|(n, _)| *n == name).map(|(_, url)| *url)
}

#[tauri::command]
fn open_link(name: String) -> Result<(), String> {
    let url = link_url(&name).ok_or("Unknown link")?;
    std::process::Command::new("open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Where a browser shows which site is in which process: about:processes in
/// Firefox-family browsers, chrome://process-internals in Chrome-family ones.
/// `browser` says which browser (by its app, so two profiles of the same app
/// can't be told apart). Async so the wait runs off the main thread.
#[tauri::command]
async fn open_about_processes(browser: Option<u32>, shared: State<'_, Arc<Shared>>) -> Result<String, String> {
    let (app, name, family) = {
        let g = shared.guard.lock().unwrap();
        let b = g.snapshot.browsers.iter().find(|b| Some(b.id) == browser).or(g.snapshot.browsers.first());
        let b = b.ok_or("No browser is running")?;
        (b.app.clone(), b.name.clone(), b.family)
    };
    match family {
        sampler::Family::Gecko => Ok(open_gecko_processes_page(&app, &name)),
        sampler::Family::Chromium => {
            // Chrome-family browsers open their own pages when handed the
            // address, unlike Firefox (whose about: scheme `open` doesn't know).
            let opened = run_with_timeout(
                std::process::Command::new("open").args(["-a", &app, PROCESS_INTERNALS]),
                Duration::from_secs(5),
            );
            Ok(if opened {
                format!("Opened {name}'s process page. Under Frame Trees, each tab is listed with its site and process ID.")
            } else {
                task_manager_hint(&name)
            })
        }
    }
}

/// The Chrome page that lists each tab's frames with their site and process.
const PROCESS_INTERNALS: &str = "chrome://process-internals/#web-contents";

/// If the process page can't be opened: where to find the Task Manager in a
/// Chrome-family browser. Chrome keeps
/// it under Window; the others move it around, so they get a general hint.
fn task_manager_hint(name: &str) -> String {
    if name.starts_with("Google Chrome") || name.starts_with("Chromium") {
        format!("In {name}, choose Window › Task Manager. It lists each tab with its process ID.")
    } else {
        format!("Open {name}'s Task Manager (in Chrome it's Window › Task Manager). It lists each tab with its process ID.")
    }
}

/// Ask a Firefox-family browser over Apple Events to open about:processes,
/// with the address on the clipboard as a fallback (`open` doesn't know the
/// about: scheme).
fn open_gecko_processes_page(app: &str, name: &str) -> String {
    let tell = format!("tell application \"{}\"", app.replace(['\\', '"'], ""));
    let asked = run_with_timeout(
        std::process::Command::new("osascript").args([
            "-e",
            "with timeout of 5 seconds",
            "-e",
            &tell,
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
        format!("Asked {name} to open about:processes. If no tab appeared, type about:processes in its address bar.")
    } else {
        // Only now is the clipboard worth taking over.
        let mut pbcopy = std::process::Command::new("pbcopy");
        pbcopy.stdin(std::process::Stdio::piped());
        if let Ok(mut child) = pbcopy.spawn() {
            use std::io::Write as _;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"about:processes");
            }
            let _ = child.wait();
        }
        let _ = run_with_timeout(std::process::Command::new("open").args(["-a", app]), Duration::from_secs(5));
        format!(
            "{name} didn't accept the request. If you declined to let Tab Guard control it, allow it in System Settings › \
             Privacy & Security › Automation. Or, in {name}, press ⌘L, then ⌘V and Return: the address is on your clipboard."
        )
    }
}

// ---------------------------------------------------------------- updates

/// Check for a newer release and install it. Returns the new version, if any.
async fn install_update(app: &AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = found_update(updater.check().await)? else { return Ok(None) };
    let version = update.version.clone();
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    Ok(Some(version))
}

/// What a check's result means: an update to install, nothing newer, or an
/// error to show. No release manifest on GitHub (nothing published yet)
/// means there's nothing newer.
fn found_update<T>(checked: Result<Option<T>, tauri_plugin_updater::Error>) -> Result<Option<T>, String> {
    match checked {
        Ok(found) => Ok(found),
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => Ok(None),
        Err(e) => Err(update_error(&e)),
    }
}

/// Words for a failed check. Network trouble is the usual cause, and the
/// updater's own messages are written for developers.
fn update_error(e: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error::*;
    match e {
        Reqwest(_) | Network(_) => "Couldn't reach GitHub to check for updates.".into(),
        e => format!("The update check failed ({e})."),
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
            let text = if by_user {
                format!("Installed Tab Guard {version}. Restarting…")
            } else {
                format!("Installed Tab Guard {version}. It restarts into it when you close this window.")
            };
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
            let again = shared.guard.lock().unwrap().config.auto_update;
            let text = if again { format!("{e} Tab Guard tries again in 6 hours.") } else { e };
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

            // Show the welcome until it's been seen, including on installs
            // from before it existed.
            if first_run || !config.welcomed {
                show_window(app.handle());
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
        RunEvent::Reopen { .. } => show_window(app),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;
    use crate::engine::Row;

    const NOW: u64 = 1_793_178_785;

    fn snap(total_mb: u64, over: usize, flagged: usize) -> Snapshot {
        Snapshot { browser_running: true, total_mb, tab_count: 20, over, flagged, rows: Vec::<Row>::new(), ..Default::default() }
    }

    #[test]
    fn tray_title_shows_total_and_warning() {
        let cfg = Config::default();
        assert_eq!(tray_text(&cfg, &snap(9216, 0, 0), NOW).0.as_deref(), Some("9.0G"));
        assert_eq!(tray_text(&cfg, &snap(9216, 1, 0), NOW).0.as_deref(), Some("⚠︎ 9.0G"));
        let paused = Config { enabled: false, ..Config::default() };
        assert_eq!(tray_text(&paused, &snap(9216, 1, 0), NOW).0.as_deref(), Some("Paused"));
        let for_a_while = Config { paused_until: Some(NOW + 42 * 60), ..paused.clone() };
        assert_eq!(tray_text(&for_a_while, &snap(9216, 1, 0), NOW).0.as_deref(), Some("Paused 42m"));
    }

    #[test]
    fn the_shield_is_colored_by_state() {
        use TrayState::*;
        let watch = Config::default();
        let close = Config { mode: Mode::Close, ..Config::default() };
        let off = Config { enabled: false, ..close.clone() };
        let held = Snapshot { total_hold: Some("paused".into()), ..snap(9216, 0, 0) };
        let cases = [
            (&off, snap(1, 2, 0), Off),
            (&watch, snap(1, 0, 0), Watching),
            (&watch, snap(1, 0, 1), Attention),
            (&watch, snap(1, 1, 0), Attention),
            (&watch, held.clone(), Watching),
            (&close, snap(1, 0, 0), Guarding),
            (&close, snap(1, 0, 2), Attention),
            (&close, held, Attention),
            (&close, snap(1, 1, 0), Closing),
            (&close, Snapshot::default(), Guarding),
        ];
        for (cfg, s, want) in cases {
            assert_eq!(tray_state(cfg, &s), want, "{:?} over {} flagged {}", cfg.mode, s.over, s.flagged);
        }
        assert_eq!(Watching.color(), None, "the plain icon follows the menu bar");
        assert_eq!(Off.color(), None, "paused is the plain shield, struck through");
        assert!([Guarding, Attention, Closing].iter().all(|s| s.color().is_some()));
    }

    #[test]
    fn tinting_keeps_the_shape() {
        let base = Image::new_owned(vec![0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 128], 3, 1);
        let red = tinted(&base, [255, 69, 58]);
        assert_eq!(red.rgba(), [255, 69, 58, 255, 255, 69, 58, 0, 255, 69, 58, 128]);
        assert_eq!((red.width(), red.height()), (3, 1));
    }

    #[test]
    fn tray_detail_says_whether_it_will_close() {
        let watch = Config::default();
        assert_eq!(tray_text(&watch, &snap(1, 2, 0), NOW).1, "Watch only: 2 tab processes would be closed");
        let close = Config { mode: Mode::Close, ..Config::default() };
        assert_eq!(tray_text(&close, &snap(1, 2, 0), NOW).1, "2 tab processes over a limit, closing one at a time");
        let paused = Config { enabled: false, ..close.clone() };
        assert_eq!(tray_text(&paused, &snap(1, 2, 0), NOW).1, "Paused until you turn it back on");
        assert_eq!(tray_text(&paused, &snap(1, 2, 0), NOW).2, "Browsers: 0.0 GB · 20 tab processes", "sizes still show");
        assert_eq!(tray_text(&close, &snap(1, 0, 3), NOW).1, "3 tab processes past the warning level");
        assert_eq!(tray_text(&close, &snap(1, 0, 0), NOW).1, "Everything's within your limits");
    }

    #[test]
    fn a_timed_pause_says_when_it_ends() {
        // A day ahead, whatever the time zone.
        let until = NOW + 24 * 3600;
        let paused = Config { enabled: false, paused_until: Some(until), ..Config::default() };
        let (title, status, _) = tray_text(&paused, &Snapshot::default(), NOW);
        assert_eq!(status, format!("Paused until tomorrow {}", &local_time(until)[11..16]));
        assert!(title.unwrap().starts_with("Paused "));
    }

    #[test]
    fn the_countdown_rounds_up_to_the_minute() {
        assert_eq!(pause_countdown(NOW + 3600, NOW), "Paused 1h");
        assert_eq!(pause_countdown(NOW + 61, NOW), "Paused 2m");
        assert_eq!(pause_countdown(NOW + 3900, NOW), "Paused 1h 5m");
        assert_eq!(pause_countdown(NOW + 20 * 3600, NOW), "Paused 20h");
        assert_eq!(pause_countdown(NOW, NOW + 5), "Paused 0m", "a pause just ended reads as none left");
    }

    #[test]
    fn the_slash_crosses_the_middle_with_a_gap() {
        let base = Image::new_owned([0u8, 0, 0, 255].repeat(44 * 44), 44, 44);
        let s = slashed(&base);
        let alpha = |x: usize, y: usize| s.rgba()[(y * 44 + x) * 4 + 3];
        assert_eq!(alpha(22, 22), 255, "the slash itself");
        assert_eq!(alpha(24, 20), 0, "the gap beside it");
        assert_eq!(alpha(40, 4), 255, "the shield away from the slash");
    }

    #[test]
    fn tray_detail_mentions_a_paused_whole_browser_limit() {
        let close = Config { mode: Mode::Close, ..Config::default() };
        let held = Snapshot {
            total_hold: Some("Whole-browser limit paused until 08:31: closing a 0.9 GB tab didn't help".into()),
            ..snap(9216, 0, 0)
        };
        assert_eq!(tray_text(&close, &held, NOW).1, "Whole-browser limit paused until 08:31");
        // A tab over its own limit still comes first.
        assert_eq!(tray_text(&close, &Snapshot { over: 1, ..held }, NOW).1, "1 tab process over a limit, closing one at a time");
    }

    #[test]
    fn only_listed_links_open() {
        assert_eq!(link_url("walk"), Some("https://allenmanzanowight.com/walk/?ref=tab-guard-app"));
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
    fn no_published_release_means_up_to_date() {
        use tauri_plugin_updater::Error;
        assert_eq!(found_update(Ok(Some(7))), Ok(Some(7)));
        assert_eq!(found_update::<u8>(Ok(None)), Ok(None));
        assert_eq!(found_update::<u8>(Err(Error::ReleaseNotFound)), Ok(None));
        assert_eq!(
            found_update::<u8>(Err(Error::Network("timed out".into()))),
            Err("Couldn't reach GitHub to check for updates.".into())
        );
        // Anything else (a bad signature, say) is reported as it is.
        assert!(found_update::<u8>(Err(Error::UnsupportedOs)).unwrap_err().len() > 5);
    }

    #[test]
    fn network_trouble_reads_plainly() {
        let e = tauri_plugin_updater::Error::Network("timed out".into());
        assert_eq!(update_error(&e), "Couldn't reach GitHub to check for updates.");
    }

    #[test]
    fn tray_sizes_each_firefox_when_several_run() {
        let b = |id, name: &str, total_mb| crate::guard::BrowserSummary {
            id,
            name: name.into(),
            family: sampler::Family::Gecko,
            app: String::new(),
            total_mb,
            tab_count: 0,
        };
        let mut s = snap(9625, 0, 0);
        s.browsers = vec![b(1, "Firefox", 7270)];
        assert_eq!(tray_text(&Config::default(), &s, NOW).2, "Firefox: 9.4 GB · 20 tab processes");
        s.browsers.push(b(2, "Firefox Nightly", 2355));
        assert_eq!(tray_text(&Config::default(), &s, NOW).2, "Firefox 7.1 GB · Firefox Nightly 2.3 GB · 20 tab processes");
    }

    #[test]
    fn task_manager_hints_are_specific_where_its_known() {
        assert_eq!(
            task_manager_hint("Google Chrome"),
            "In Google Chrome, choose Window › Task Manager. It lists each tab with its process ID."
        );
        assert!(task_manager_hint("Brave Browser").starts_with("Open Brave Browser's Task Manager"));
    }

    #[test]
    fn tray_without_a_browser() {
        let none = Snapshot::default();
        let (title, status, _) = tray_text(&Config::default(), &none, NOW);
        assert_eq!(title, None);
        assert_eq!(status, "No browser is running");
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
