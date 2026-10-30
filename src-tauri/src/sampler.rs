//! Finds browsers' processes and measures them, straight from the kernel.
//!
//! Each pass lists every pid, but only looks closely at browsers': a pid's
//! path and arguments are read once and cached, and one `proc_pid_rusage`
//! call per browser process gives its memory, CPU time and start time.
//!
//! Two families are supported, both of which run each site in its own
//! process and show a closed one as a crashed tab the user can reload:
//! Firefox and the browsers built on it, and Chrome and the browsers built
//! on Chromium. Several can run at once (or one with a second profile).
//! Each process is tied to the browser that started it, so their limits can
//! be applied separately.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Main,
    Tab,
    Gpu,
    Other,
}

/// Which engine a browser is built on. It decides how its processes are
/// told apart and how the user finds out which site is in which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    /// Firefox, Zen, LibreWolf, Waterfox, Floorp, Mullvad Browser, Tor Browser.
    Gecko,
    /// Chrome, Edge, Brave, Arc, Vivaldi, Opera, Chromium.
    Chromium,
}

/// Supported browsers, by the start of their app's name. "Firefox" also
/// covers "Firefox Nightly"; "Google Chrome" covers "Google Chrome Beta".
/// Apps built on the same engines that aren't browsers (Thunderbird, or
/// Electron apps like Slack) aren't listed, so they're left alone.
const BROWSERS: [(&str, Family); 14] = [
    ("Firefox", Family::Gecko),
    ("Zen", Family::Gecko),
    ("LibreWolf", Family::Gecko),
    ("Waterfox", Family::Gecko),
    ("Floorp", Family::Gecko),
    ("Mullvad Browser", Family::Gecko),
    ("Tor Browser", Family::Gecko),
    ("Google Chrome", Family::Chromium),
    ("Microsoft Edge", Family::Chromium),
    ("Brave Browser", Family::Chromium),
    ("Arc", Family::Chromium),
    ("Vivaldi", Family::Chromium),
    ("Opera", Family::Chromium),
    ("Chromium", Family::Chromium),
];

#[derive(Debug, Clone, Serialize)]
pub struct Proc {
    pub pid: u32,
    /// The browser it belongs to: a `Browser::id`.
    pub browser: u32,
    pub kind: Kind,
    /// Physical footprint, the number Activity Monitor shows as "Memory".
    pub mem_mb: u64,
    /// Percent of one CPU core since the previous sample.
    pub cpu: f32,
    /// Seconds since the process started.
    pub age_secs: u64,
    /// Memory growth over the last few minutes, MB per minute.
    pub growth_mb_per_min: f64,
}

/// One running browser.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Browser {
    /// Its main process's pid.
    pub id: u32,
    /// "Firefox", "Firefox Nightly", "Google Chrome", "Firefox (work)", "Firefox (2)"
    pub name: String,
    pub family: Family,
    /// The app bundle, for bringing it forward or opening about:processes.
    pub app: String,
    /// What its settings are saved under: the app, plus the profile if it
    /// was started with one. Stays the same from one launch to the next.
    pub key: String,
}

/// `Browser::key` for an app and profile.
pub fn browser_key(app: &str, profile: Option<&str>) -> String {
    match profile {
        Some(p) => format!("{app}#{p}"),
        None => app.to_string(),
    }
}

/// The name for a saved `Browser::key`, for copies that aren't running.
pub fn key_name(key: &str) -> String {
    let (app, profile) = key.split_once('#').map_or((key, None), |(a, p)| (a, Some(p)));
    let base = app.rsplit('/').next().unwrap_or(app).trim_end_matches(".app");
    match profile {
        Some(p) => format!("{base} ({p})"),
        None => base.to_string(),
    }
}

#[derive(Debug, Clone)]
enum Known {
    NotBrowser,
    /// `start` tells a reused pid apart from the process we classified.
    /// `content` marks a Firefox content process, whose kind comes from
    /// the name it gives macOS; `named` says that name has been seen (see
    /// `name_content`).
    Browser { kind: Kind, start: u64, parent: u32, family: Family, app: String, profile: Option<String>, content: bool, named: bool },
}

/// What grouping needs to know about one browser process.
#[derive(Debug, Clone)]
pub struct Member<'a> {
    pub pid: u32,
    pub kind: Kind,
    pub parent: u32,
    pub start: u64,
    pub family: Family,
    pub app: &'a str,
    pub profile: Option<&'a str>,
}

pub struct Sampler {
    /// Mach time units to nanoseconds: ns = ticks * numer / denom.
    timebase: (u64, u64),
    known: HashMap<u32, Known>,
    /// pid -> (mach time, CPU ticks) at the previous sample.
    prev_cpu: HashMap<u32, (u64, u64)>,
    /// pid -> recent (time, MB) samples, for the growth rate.
    history: HashMap<u32, VecDeque<(Instant, u64)>>,
    /// App bundle -> the name of its main executable, read once per app.
    main_exe: HashMap<String, String>,
    pid_buf: Vec<libc::pid_t>,
    /// When Firefox content processes' names were last read.
    names_read: Option<Instant>,
}

const HISTORY_WINDOW_SECS: u64 = 300;
/// How long to wait for a new Firefox content process to name itself
/// before treating it as a tab without one.
const NAME_WAIT_SECS: u64 = 30;
/// How often to read the names again: Firefox renames a spare process
/// when it gives it a job.
const NAME_REFRESH: Duration = Duration::from_secs(15);
const MB: u64 = 1024 * 1024;

impl Sampler {
    pub fn new() -> Self {
        let mut tb = MachTimebase { numer: 1, denom: 1 };
        unsafe { mach_timebase_info(&mut tb) };
        Sampler {
            timebase: (tb.numer.max(1) as u64, tb.denom.max(1) as u64),
            known: HashMap::new(),
            prev_cpu: HashMap::new(),
            history: HashMap::new(),
            main_exe: HashMap::new(),
            pid_buf: Vec::new(),
            names_read: None,
        }
    }

    fn ticks_to_ns(&self, ticks: u64) -> u64 {
        (ticks as u128 * self.timebase.0 as u128 / self.timebase.1 as u128) as u64
    }

    /// Browsers' processes, biggest first, and the browsers they belong to,
    /// oldest first.
    pub fn sample(&mut self) -> (Vec<Proc>, Vec<Browser>) {
        list_pids(&mut self.pid_buf);
        let now = Instant::now();
        let now_ticks = unsafe { mach_absolute_time() };
        let alive: HashSet<u32> = self.pid_buf.iter().map(|&p| p as u32).collect();
        // Forget pids that exited, so a reused pid gets looked at afresh.
        self.known.retain(|pid, _| alive.contains(pid));
        self.prev_cpu.retain(|pid, _| alive.contains(pid));
        self.history.retain(|pid, _| alive.contains(pid));

        let mut procs = Vec::new();
        let mut members = Vec::new();
        for i in 0..self.pid_buf.len() {
            let pid = self.pid_buf[i] as u32;
            if pid == 0 {
                continue;
            }
            if !self.known.contains_key(&pid) {
                let k = identify(pid, &mut self.main_exe);
                self.known.insert(pid, k);
            }
            let Some(&Known::Browser { kind, start, parent, .. }) = self.known.get(&pid) else { continue };
            let Some(ri) = rusage(pid) else {
                self.known.remove(&pid);
                continue;
            };
            if ri.ri_proc_start_abstime != start && start != 0 {
                // Same pid, different process: classify it again next pass.
                self.known.remove(&pid);
                continue;
            }

            let cpu_ticks = ri.ri_user_time + ri.ri_system_time;
            let cpu = match self.prev_cpu.insert(pid, (now_ticks, cpu_ticks)) {
                Some((t0, c0)) if now_ticks > t0 => {
                    let wall = self.ticks_to_ns(now_ticks - t0) as f64;
                    let used = self.ticks_to_ns(cpu_ticks.saturating_sub(c0)) as f64;
                    (100.0 * used / wall) as f32
                }
                _ => 0.0,
            };

            let mem_mb = ri.ri_phys_footprint / MB;
            let age_secs = self.ticks_to_ns(now_ticks.saturating_sub(ri.ri_proc_start_abstime)) / 1_000_000_000;
            let hist = self.history.entry(pid).or_default();
            hist.push_back((now, mem_mb));
            while hist.front().is_some_and(|(t, _)| now.duration_since(*t).as_secs() > HISTORY_WINDOW_SECS) {
                hist.pop_front();
            }

            procs.push(Proc { pid, browser: pid, kind, mem_mb, cpu, age_secs, growth_mb_per_min: growth_mb_per_min(hist) });
            members.push((pid, kind, parent, start));
        }

        self.name_content(now, &mut procs, &mut members);

        let members: Vec<Member> = members
            .iter()
            .filter_map(|&(pid, kind, parent, start)| match self.known.get(&pid)? {
                Known::Browser { family, app, profile, .. } => {
                    Some(Member { pid, kind, parent, start, family: *family, app, profile: profile.as_deref() })
                }
                Known::NotBrowser => None,
            })
            .collect();
        let (of, browsers) = group(&members);
        for p in &mut procs {
            p.browser = of.get(&p.pid).copied().unwrap_or(p.pid);
        }
        procs.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb).then(a.pid.cmp(&b.pid)));
        (procs, browsers)
    }

    /// Firefox starts every content process the same way, so its arguments
    /// can't tell a tab from the extensions process or the one for its own
    /// about: pages. Each one does name itself to macOS ("FirefoxCP
    /// WebExtensions"), so the names are read from there: on every pass
    /// while a new process hasn't named itself yet, and every NAME_REFRESH
    /// otherwise, because a spare process starts as "Web Content" and is
    /// renamed when Firefox gives it a job.
    fn name_content(&mut self, now: Instant, procs: &mut [Proc], members: &mut [(u32, Kind, u32, u64)]) {
        let content = |k: &Known| matches!(k, Known::Browser { content: true, .. });
        if !self.known.values().any(content) {
            return;
        }
        let unnamed = self.known.values().any(|k| matches!(k, Known::Browser { content: true, named: false, .. }));
        let due = self.names_read.is_none_or(|t| now.duration_since(t) >= NAME_REFRESH);
        if !unnamed && !due {
            return;
        }
        // Couldn't read them: keep what's known and try again next pass.
        let Some(names) = content_names() else { return };
        self.names_read = Some(now);
        for p in procs.iter_mut() {
            let Some(Known::Browser { kind, content: true, named, .. }) = self.known.get_mut(&p.pid) else { continue };
            match names.get(&p.pid) {
                Some(name) => {
                    *kind = gecko_content_kind(name);
                    *named = true;
                }
                // Not named yet. A browser that never names them leaves them tabs.
                None => *named = p.age_secs > NAME_WAIT_SECS,
            }
            p.kind = *kind;
            if let Some(m) = members.iter_mut().find(|m| m.0 == p.pid) {
                m.1 = *kind;
            }
        }
    }
}

/// What a Firefox content process is, from the name it gives macOS: web
/// pages ("Web Content", "Isolated Web Content") and local files are tabs;
/// extensions, Firefox's own pages and service workers aren't.
pub fn gecko_content_kind(name: &str) -> Kind {
    if name.ends_with("Web Content") || name.ends_with("File Content") { Kind::Tab } else { Kind::Other }
}

/// pid -> the name each running process gave macOS, from `lsappinfo list`,
/// or None if it failed or took more than a few seconds.
fn content_names() -> Option<HashMap<u32, String>> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/bin/lsappinfo").arg("list").stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read as it writes, so a long list can't fill the pipe and stall.
    let reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut text = String::new();
        stdout.read_to_string(&mut text).ok().map(|_| text)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    Some(parse_lsappinfo(&reader.join().ok()??))
}

/// Each entry starts `12) "Name" ASN:…` and has a `pid = 123 …` line.
pub fn parse_lsappinfo(text: &str) -> HashMap<u32, String> {
    let mut names = HashMap::new();
    let mut name = None;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.split_once(") \"").filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map(|(_, r)| r) {
            name = rest.split_once('"').map(|(n, _)| n.to_string());
        } else if let Some(pid) = t.strip_prefix("pid = ").and_then(|r| r.split_whitespace().next()?.parse().ok()) {
            if let Some(n) = name.take() {
                names.insert(pid, n);
            }
        }
    }
    names
}

/// Look at a pid we haven't seen before. `main_exe` caches each app's main
/// executable name.
fn identify(pid: u32, main_exe: &mut HashMap<String, String>) -> Known {
    let Some(path) = pid_path(pid) else { return Known::NotBrowser };
    let Some((family, app)) = browser_app(&path) else { return Known::NotBrowser };
    let exe = main_exe.entry(app.to_string()).or_insert_with(|| main_executable(app, family));
    let is_main = path == format!("{app}/Contents/MacOS/{exe}");
    let args = proc_args(pid).unwrap_or_default();
    let start = rusage(pid).map(|r| r.ri_proc_start_abstime).unwrap_or(0);
    let kind = classify(family, &path, &args, is_main);
    // Chromium browsers run every profile in one process; Firefox runs one per profile.
    let profile = if kind == Kind::Main && family == Family::Gecko { profile_name(&args) } else { None };
    let content = family == Family::Gecko && kind == Kind::Tab;
    Known::Browser { kind, start, parent: parent_pid(pid), family, app: app.to_string(), profile, content, named: !content }
}

/// The outermost app bundle a path is in: "/Applications/Google
/// Chrome.app" for its main executable and for the helper apps nested inside.
pub fn app_bundle(path: &str) -> Option<&str> {
    let i = path.find(".app/Contents/")?;
    Some(&path[..i + 4])
}

/// The family and app bundle of a supported browser's process.
pub fn browser_app(path: &str) -> Option<(Family, &str)> {
    let app = app_bundle(path)?;
    let name = app.rsplit('/').next()?.trim_end_matches(".app");
    let (_, family) = BROWSERS.iter().find(|(start, _)| name == *start || name.starts_with(&format!("{start} ")))?;
    Some((*family, app))
}

/// The name of an app's main executable, from its Info.plist, with a
/// sensible guess if that can't be read.
fn main_executable(app: &str, family: Family) -> String {
    let plist = format!("{app}/Contents/Info.plist");
    let out = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleExecutable", "raw", "-o", "-", &plist])
        .output();
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => match family {
            Family::Gecko => "firefox".into(),
            Family::Chromium => app.rsplit('/').next().unwrap_or(app).trim_end_matches(".app").to_string(),
        },
    }
}

/// The profile a copy of Firefox was started with (`-P work`, or
/// `-profile /path/to/work`), if it was given one.
pub fn profile_name(args: &[String]) -> Option<String> {
    let i = args.iter().position(|a| matches!(a.as_str(), "-P" | "-p" | "--P" | "-profile" | "--profile"))?;
    let value = args.get(i + 1).filter(|v| !v.starts_with('-'))?;
    let name = value.trim_end_matches('/').rsplit('/').next().unwrap_or(value);
    (!name.is_empty()).then(|| name.to_string())
}

/// Tie each process to the browser it belongs to, and name the browsers.
/// A process belongs to the browser that started it; one that was handed to
/// launchd (Firefox's crash helper does this) belongs to the oldest browser
/// from the same app. Returns pid -> browser id, and the browsers, oldest
/// first.
pub fn group(members: &[Member]) -> (HashMap<u32, u32>, Vec<Browser>) {
    let mut mains: Vec<&Member> = members.iter().filter(|m| m.kind == Kind::Main).collect();
    mains.sort_by_key(|m| (m.start, m.pid));
    let mut of = HashMap::new();
    let mut orphans = Vec::new();
    for m in members {
        let owner = if m.kind == Kind::Main {
            Some(m.pid)
        } else {
            mains.iter().find(|b| b.pid == m.parent).or_else(|| mains.iter().find(|b| b.app == m.app)).map(|b| b.pid)
        };
        // The browser itself is gone but this process isn't yet: it stands alone.
        let owner = owner.unwrap_or_else(|| {
            orphans.push(m);
            m.pid
        });
        of.insert(m.pid, owner);
    }

    let mut browsers: Vec<Browser> = mains
        .iter()
        .chain(orphans.iter())
        .map(|m| {
            let key = browser_key(m.app, m.profile);
            Browser { id: m.pid, name: key_name(&key), family: m.family, app: m.app.to_string(), key }
        })
        .collect();
    // Two copies with the same name: number the later ones.
    let names: Vec<String> = browsers.iter().map(|b| b.name.clone()).collect();
    for (i, b) in browsers.iter_mut().enumerate().skip(1) {
        let earlier = names[..i].iter().filter(|n| **n == names[i]).count();
        if earlier > 0 {
            b.name = format!("{} ({})", names[i], earlier + 1);
        }
    }
    (of, browsers)
}

/// Which kind of browser process this is. `is_main` says it's the app's
/// own executable.
pub fn classify(family: Family, path: &str, args: &[String], is_main: bool) -> Kind {
    if is_main {
        return Kind::Main;
    }
    match family {
        Family::Gecko => {
            if path.contains("gpu-helper") {
                return Kind::Gpu;
            }
            // Web content processes run plugin-container with -isForBrowser
            // and end their arguments with the process type "tab".
            let is_browser = args.iter().any(|a| a == "-isForBrowser");
            if path.contains("plugin-container") && is_browser && args.last().is_some_and(|a| a == "tab") {
                return Kind::Tab;
            }
            Kind::Other
        }
        Family::Chromium => {
            // Helpers say what they are with --type=. Extensions run in
            // renderers too, marked --extension-process; they aren't tabs.
            let kind = args.iter().find_map(|a| a.strip_prefix("--type="));
            match kind {
                Some("renderer") if !args.iter().any(|a| a == "--extension-process") => Kind::Tab,
                Some("gpu-process") => Kind::Gpu,
                _ => Kind::Other,
            }
        }
    }
}

/// MB per minute between the oldest and newest sample, once they're at
/// least 20 seconds apart.
pub fn growth_mb_per_min(hist: &VecDeque<(Instant, u64)>) -> f64 {
    match (hist.front(), hist.back()) {
        (Some((t0, m0)), Some((t1, m1))) => {
            let secs = t1.duration_since(*t0).as_secs_f64();
            if secs < 20.0 {
                0.0
            } else {
                (*m1 as f64 - *m0 as f64) / (secs / 60.0)
            }
        }
        _ => 0.0,
    }
}

/// Parse a KERN_PROCARGS2 buffer: argc, the exec path, NUL padding, then
/// argc NUL-terminated arguments (environment follows; ignored).
pub fn parse_procargs(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc < 0 {
        return None;
    }
    let mut rest = &buf[4..];
    // Skip the exec path and the padding after it.
    let end = rest.iter().position(|&b| b == 0)?;
    rest = &rest[end..];
    let start = rest.iter().position(|&b| b != 0)?;
    rest = &rest[start..];
    let mut args = Vec::with_capacity(argc as usize);
    for part in rest.split(|&b| b == 0).take(argc as usize) {
        args.push(String::from_utf8_lossy(part).into_owned());
    }
    (args.len() == argc as usize).then_some(args)
}

// ---------------------------------------------------------------- syscalls

// Declared here: libc deprecates its copies in favour of the mach2 crate,
// which isn't worth a dependency for two functions.
#[repr(C)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

extern "C" {
    fn mach_timebase_info(info: *mut MachTimebase) -> libc::c_int;
    fn mach_absolute_time() -> u64;
}

fn list_pids(buf: &mut Vec<libc::pid_t>) {
    let needed = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    // Leave room for processes started between the two calls.
    let cap = (needed.max(0) as usize) + 64;
    buf.clear();
    buf.resize(cap, 0);
    let n = unsafe {
        libc::proc_listallpids(
            buf.as_mut_ptr() as *mut libc::c_void,
            (cap * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    buf.truncate(n.max(0) as usize);
}

fn pid_path(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

fn parent_pid(pid: u32) -> u32 {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr() as *mut libc::c_void, size)
    };
    if n == size { unsafe { info.assume_init() }.pbi_ppid } else { 0 }
}

fn proc_args(pid: u32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0)
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs(&buf)
}

/// Unlike resident size, the physical footprint doesn't count memory shared
/// with other processes, so footprints can be summed.
/// When a process started, in Mach time, so a reused pid can be told from it.
pub fn proc_start(pid: u32) -> Option<u64> {
    rusage(pid).map(|r| r.ri_proc_start_abstime)
}

fn rusage(pid: u32) -> Option<libc::rusage_info_v2> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    let rc = unsafe {
        libc::proc_pid_rusage(pid as libc::c_int, libc::RUSAGE_INFO_V2, info.as_mut_ptr() as *mut libc::rusage_info_t)
    };
    (rc == 0).then(|| unsafe { info.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn family(path: &str) -> Option<Family> {
        browser_app(path).map(|(f, _)| f)
    }

    const CHROME_RENDERER: &str = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/\
        Versions/131.0.6778.86/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)";

    #[test]
    fn recognizes_supported_browsers_anywhere() {
        use Family::*;
        assert_eq!(family("/Applications/Firefox.app/Contents/MacOS/firefox"), Some(Gecko));
        assert_eq!(
            family("/Applications/Firefox.app/Contents/MacOS/plugin-container.app/Contents/MacOS/plugin-container"),
            Some(Gecko)
        );
        assert_eq!(family("/Users/a/Applications/Firefox Nightly.app/Contents/MacOS/firefox"), Some(Gecko));
        assert_eq!(family("/Applications/Firefox Developer Edition.app/Contents/MacOS/firefox"), Some(Gecko));
        for app in ["Zen", "Zen Browser", "LibreWolf", "Waterfox", "Floorp", "Mullvad Browser", "Tor Browser"] {
            assert_eq!(family(&format!("/Applications/{app}.app/Contents/MacOS/x")), Some(Gecko), "{app}");
        }
        assert_eq!(family(CHROME_RENDERER), Some(Chromium));
        for app in ["Google Chrome", "Google Chrome Beta", "Microsoft Edge", "Brave Browser", "Arc", "Vivaldi", "Opera", "Opera GX", "Chromium"] {
            assert_eq!(family(&format!("/Applications/{app}.app/Contents/MacOS/x")), Some(Chromium), "{app}");
        }
        // Not browsers, or not ones Tab Guard supports.
        for app in ["Safari", "Thunderbird", "Slack", "Visual Studio Code", "Arcade", "Zenly", "Operator"] {
            assert_eq!(family(&format!("/Applications/{app}.app/Contents/MacOS/x")), None, "{app}");
        }
        assert_eq!(family("/usr/bin/firefox"), None);
        // A folder named Firefox isn't enough; the app itself must be.
        assert_eq!(family("/Users/a/Firefox/Other.app/Contents/MacOS/other"), None);
    }

    #[test]
    fn finds_the_app_and_profile() {
        let nightly = "/Applications/Firefox Nightly.app";
        assert_eq!(app_bundle(&format!("{nightly}/Contents/MacOS/firefox")), Some(nightly));
        assert_eq!(app_bundle(CHROME_RENDERER), Some("/Applications/Google Chrome.app"), "the outer app, not the helper");
        assert_eq!(
            app_bundle(&format!("{nightly}/Contents/MacOS/plugin-container.app/Contents/MacOS/plugin-container")),
            Some(nightly)
        );
        assert_eq!(profile_name(&args(&["firefox", "-P", "work"])), Some("work".into()));
        assert_eq!(profile_name(&args(&["firefox", "-profile", "/Users/a/Profiles/abc.work/"])), Some("abc.work".into()));
        assert_eq!(profile_name(&args(&["firefox", "-P"])), None);
        assert_eq!(profile_name(&args(&["firefox", "-P", "-no-remote"])), None);
        assert_eq!(profile_name(&args(&["firefox"])), None);
    }

    fn member<'a>(pid: u32, kind: Kind, parent: u32, app: &'a str) -> Member<'a> {
        let family = if app.contains("Chrome") { Family::Chromium } else { Family::Gecko };
        Member { pid, kind, parent, start: pid as u64, family, app, profile: None }
    }

    #[test]
    fn groups_processes_by_the_firefox_that_started_them() {
        let (rel, nightly) = ("/Applications/Firefox.app", "/Applications/Firefox Nightly.app");
        let members = [
            member(10, Kind::Main, 1, rel),
            member(11, Kind::Tab, 10, rel),
            member(12, Kind::Gpu, 10, rel),
            member(13, Kind::Other, 1, rel), // the crash helper, handed to launchd
            member(20, Kind::Main, 1, nightly),
            member(21, Kind::Tab, 20, nightly),
        ];
        let (of, browsers) = group(&members);
        assert_eq!([of[&11], of[&12], of[&13], of[&21]], [10, 10, 10, 20]);
        let names: Vec<&str> = browsers.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["Firefox", "Firefox Nightly"]);
        assert_eq!(browsers[1].app, nightly);
    }

    #[test]
    fn names_copies_of_the_same_firefox_apart() {
        let rel = "/Applications/Firefox.app";
        let mut work = member(30, Kind::Main, 1, rel);
        work.profile = Some("work");
        let members = [
            member(10, Kind::Main, 1, rel),
            member(20, Kind::Main, 1, rel),
            work,
            member(40, Kind::Main, 1, rel),
            member(21, Kind::Tab, 20, rel),
        ];
        let (of, browsers) = group(&members);
        assert_eq!(of[&21], 20, "a tab stays with its own Firefox, not the oldest");
        let names: Vec<&str> = browsers.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["Firefox", "Firefox (2)", "Firefox (work)", "Firefox (3)"]);
        // Unnamed copies of the same app share their settings; a profile has its own.
        assert_eq!(browsers[0].key, browsers[1].key);
        assert_eq!(browsers[2].key, "/Applications/Firefox.app#work");
        assert_eq!(key_name(&browsers[2].key), "Firefox (work)");
    }

    #[test]
    fn a_tab_whose_firefox_is_gone_stands_alone() {
        let (of, browsers) = group(&[member(11, Kind::Tab, 1, "/Applications/Firefox.app")]);
        assert_eq!(of[&11], 11);
        assert_eq!(browsers.len(), 1);
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classifies_firefox_processes() {
        let g = |path: &str, a: &[&str]| classify(Family::Gecko, path, &args(a), false);
        let base = "/Applications/Firefox.app/Contents/MacOS";
        let pc = format!("{base}/plugin-container.app/Contents/MacOS/plugin-container");
        assert_eq!(classify(Family::Gecko, &format!("{base}/firefox"), &[], true), Kind::Main);
        assert_eq!(g(&format!("{base}/gpu-helper.app/Contents/MacOS/Firefox GPU Helper"), &[]), Kind::Gpu);
        assert_eq!(g(&pc, &["pc", "-isForBrowser", "-prefsHandle", "0:1", "tab"]), Kind::Tab);
        // Utility, socket and media processes aren't tabs.
        assert_eq!(g(&pc, &["pc", "-appDir", "x", "socket"]), Kind::Other);
        assert_eq!(g(&pc, &["pc", "-isForBrowser", "rdd"]), Kind::Other);
        assert_eq!(g(&pc, &["pc", "tab"]), Kind::Other);
        assert_eq!(g(&format!("{base}/crashhelper"), &[]), Kind::Other);
    }

    #[test]
    fn names_firefox_content_processes() {
        assert_eq!(gecko_content_kind("FirefoxCP Isolated Web Content"), Kind::Tab);
        assert_eq!(gecko_content_kind("FirefoxCP Web Content"), Kind::Tab);
        assert_eq!(gecko_content_kind("LibreWolfCP File Content"), Kind::Tab);
        assert_eq!(gecko_content_kind("FirefoxCP WebExtensions"), Kind::Other);
        assert_eq!(gecko_content_kind("FirefoxCP Privileged Content"), Kind::Other);
        assert_eq!(gecko_content_kind("FirefoxCP Isolated Service Worker"), Kind::Other);
        let text = r#"
41) "Firefox" ASN:0x0-0x32032: (in front)
    bundleID="org.mozilla.firefox"
    pid = 1267 token=[sess=100015 pid=1267] type="Foreground"
128) "FirefoxCP WebExtensions" ASN:0x0-0xa83a83:
    bundleID="org.mozilla.plugincontainer"
    pid = 64290 token=[sess=100015 pid=64290] type="BackgroundOnly"
129) "Has no pid" ASN:0x0-0x1:
130) "FirefoxCP Isolated Web Content" ASN:0x0-0xd40d4:
    pid = 15574 token=[]
"#;
        let names = parse_lsappinfo(text);
        assert_eq!(names.get(&1267).map(String::as_str), Some("Firefox"));
        assert_eq!(names.get(&64290).map(String::as_str), Some("FirefoxCP WebExtensions"));
        assert_eq!(names.get(&15574).map(String::as_str), Some("FirefoxCP Isolated Web Content"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn classifies_chrome_processes() {
        let c = |a: &[&str]| classify(Family::Chromium, CHROME_RENDERER, &args(a), false);
        let main = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        assert_eq!(classify(Family::Chromium, main, &args(&[main]), true), Kind::Main);
        assert_eq!(c(&["helper", "--type=renderer", "--lang=en-US", "--renderer-client-id=7"]), Kind::Tab);
        assert_eq!(c(&["helper", "--type=renderer", "--extension-process", "--renderer-client-id=8"]), Kind::Other);
        assert_eq!(c(&["helper", "--type=gpu-process", "--gpu-preferences=x"]), Kind::Gpu);
        assert_eq!(c(&["helper", "--type=utility", "--utility-sub-type=network.mojom.NetworkService"]), Kind::Other);
        assert_eq!(c(&["chrome_crashpad_handler", "--monitor-self"]), Kind::Other);
    }

    #[test]
    fn reads_an_apps_main_executable() {
        assert_eq!(main_executable("/System/Applications/Calculator.app", Family::Gecko), "Calculator");
        // An app that can't be read falls back to a guess.
        assert_eq!(main_executable("/nowhere/Firefox.app", Family::Gecko), "firefox");
        assert_eq!(main_executable("/nowhere/Brave Browser.app", Family::Chromium), "Brave Browser");
    }

    #[test]
    fn groups_chrome_with_its_helpers_beside_firefox() {
        let (fx, chrome) = ("/Applications/Firefox.app", "/Applications/Google Chrome.app");
        let members = [
            member(10, Kind::Main, 1, fx),
            member(11, Kind::Tab, 10, fx),
            member(30, Kind::Main, 1, chrome),
            member(31, Kind::Tab, 30, chrome),
            member(32, Kind::Gpu, 30, chrome),
        ];
        let (of, browsers) = group(&members);
        assert_eq!([of[&11], of[&31], of[&32]], [10, 30, 30]);
        assert_eq!(browsers[1].name, "Google Chrome");
        assert_eq!(browsers[1].family, Family::Chromium);
        assert_eq!(browsers[0].family, Family::Gecko);
    }

    fn procargs(argc: i32, exec: &str, pad: usize, argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut b = argc.to_ne_bytes().to_vec();
        b.extend(exec.as_bytes());
        b.extend(std::iter::repeat(0).take(pad));
        for s in argv.iter().chain(env) {
            b.extend(s.as_bytes());
            b.push(0);
        }
        b
    }

    #[test]
    fn parses_procargs() {
        let buf = procargs(3, "/bin/pc", 5, &["/bin/pc", "-isForBrowser", "tab"], &["HOME=/x"]);
        assert_eq!(parse_procargs(&buf), Some(args(&["/bin/pc", "-isForBrowser", "tab"])));
        // No padding beyond the single terminator.
        let buf = procargs(1, "/bin/a", 1, &["a"], &[]);
        assert_eq!(parse_procargs(&buf), Some(args(&["a"])));
        // argc says more than the buffer holds.
        let buf = procargs(4, "/bin/a", 1, &["a", "b"], &[]);
        assert_eq!(parse_procargs(&buf), None);
        assert_eq!(parse_procargs(&[1, 0]), None);
        assert_eq!(parse_procargs(&(-1i32).to_ne_bytes()), None);
    }

    #[test]
    fn growth_needs_twenty_seconds_of_history() {
        let t0 = Instant::now();
        let mut h = VecDeque::from([(t0, 500)]);
        assert_eq!(growth_mb_per_min(&h), 0.0);
        h.push_back((t0 + Duration::from_secs(10), 600));
        assert_eq!(growth_mb_per_min(&h), 0.0);
        h.push_back((t0 + Duration::from_secs(120), 700));
        assert!((growth_mb_per_min(&h) - 100.0).abs() < 1e-9);
        // Shrinking reads as negative growth.
        let h = VecDeque::from([(t0, 800), (t0 + Duration::from_secs(60), 500)]);
        assert!((growth_mb_per_min(&h) + 300.0).abs() < 1e-9);
        assert_eq!(growth_mb_per_min(&VecDeque::new()), 0.0);
    }

    #[test]
    fn sees_its_own_process_but_not_as_a_browser() {
        let me = std::process::id();
        let path = pid_path(me).expect("own path");
        assert!(browser_app(&path).is_none());
        let ri = rusage(me).expect("own rusage");
        assert!(ri.ri_phys_footprint > 0);
        assert_eq!(parent_pid(me), unsafe { libc::getppid() } as u32);
        let args = proc_args(me).expect("own args");
        assert!(!args.is_empty());
        let mut buf = Vec::new();
        list_pids(&mut buf);
        assert!(buf.contains(&(me as libc::pid_t)));
    }

    /// Needs Firefox running: `cargo test live_sample -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_sample() {
        let mut s = Sampler::new();
        s.sample();
        std::thread::sleep(Duration::from_secs(1));
        let (procs, browsers) = s.sample();
        let total: u64 = procs.iter().map(|p| p.mem_mb).sum();
        let tabs = procs.iter().filter(|p| p.kind == Kind::Tab).count();
        println!("{} browser processes, {tabs} tabs, {total} MB", procs.len());
        for b in &browsers {
            let n = procs.iter().filter(|p| p.browser == b.id).count();
            println!("{} ({}): {n} processes, {}", b.name, b.id, b.app);
            for p in procs.iter().filter(|p| p.browser == b.id && p.kind != Kind::Other) {
                println!("    {:>6} {:?} {} MB", p.pid, p.kind, p.mem_mb);
            }
        }
        for p in procs.iter().take(8) {
            println!("{:>6} {:?} {} MB cpu {:.1}% age {}s", p.pid, p.kind, p.mem_mb, p.cpu, p.age_secs);
        }
        assert!(tabs > 0, "is Firefox running?");
    }

    /// `cargo test --release bench_sample -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_sample() {
        let mut s = Sampler::new();
        let t = Instant::now();
        s.sample();
        println!("first sample: {:?}", t.elapsed());
        let t = Instant::now();
        for _ in 0..20 {
            s.sample();
        }
        println!("steady sample: {:?} each", t.elapsed() / 20);
    }
}
