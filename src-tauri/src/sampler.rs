//! Finds Firefox's processes and measures them, straight from the kernel.
//!
//! Each pass lists every pid, but only looks closely at Firefox's: a pid's
//! path and arguments are read once and cached, and one `proc_pid_rusage`
//! call per Firefox process gives its memory, CPU time and start time.
//!
//! Several copies of Firefox can run at once (release, Nightly, Developer
//! Edition, or one copy with a second profile). Each process is tied to the
//! copy that started it, so their limits can be applied separately.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Main,
    Tab,
    Gpu,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proc {
    pub pid: u32,
    /// The copy of Firefox it belongs to: a `Browser::id`.
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

/// One running copy of Firefox.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Browser {
    /// Its main process's pid.
    pub id: u32,
    /// "Firefox", "Firefox Nightly", "Firefox (work)", "Firefox (2)"
    pub name: String,
    /// The app bundle, for asking it to open about:processes.
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
    NotFirefox,
    /// `start` tells a reused pid apart from the process we classified.
    Firefox { kind: Kind, start: u64, parent: u32, app: String, profile: Option<String> },
}

/// What grouping needs to know about one Firefox process.
#[derive(Debug, Clone)]
pub struct Member<'a> {
    pub pid: u32,
    pub kind: Kind,
    pub parent: u32,
    pub start: u64,
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
    pid_buf: Vec<libc::pid_t>,
}

const HISTORY_WINDOW_SECS: u64 = 300;
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
            pid_buf: Vec::new(),
        }
    }

    fn ticks_to_ns(&self, ticks: u64) -> u64 {
        (ticks as u128 * self.timebase.0 as u128 / self.timebase.1 as u128) as u64
    }

    /// Firefox's processes, biggest first, and the copies of Firefox they
    /// belong to, oldest first.
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
            let known = self.known.entry(pid).or_insert_with(|| identify(pid));
            let Known::Firefox { kind, start, parent, .. } = *known else { continue };
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

        let members: Vec<Member> = members
            .iter()
            .filter_map(|&(pid, kind, parent, start)| match self.known.get(&pid)? {
                Known::Firefox { app, profile, .. } => {
                    Some(Member { pid, kind, parent, start, app, profile: profile.as_deref() })
                }
                Known::NotFirefox => None,
            })
            .collect();
        let (of, browsers) = group(&members);
        for p in &mut procs {
            p.browser = of.get(&p.pid).copied().unwrap_or(p.pid);
        }
        procs.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb).then(a.pid.cmp(&b.pid)));
        (procs, browsers)
    }
}

/// Look at a pid we haven't seen before.
fn identify(pid: u32) -> Known {
    let Some(path) = pid_path(pid) else { return Known::NotFirefox };
    if !is_firefox_path(&path) {
        return Known::NotFirefox;
    }
    let kind_needs_args = path.contains("plugin-container") || path.ends_with("/MacOS/firefox");
    let args = if kind_needs_args { proc_args(pid).unwrap_or_default() } else { Vec::new() };
    let start = rusage(pid).map(|r| r.ri_proc_start_abstime).unwrap_or(0);
    let kind = classify(&path, &args);
    let profile = if kind == Kind::Main { profile_name(&args) } else { None };
    Known::Firefox { kind, start, parent: parent_pid(pid), app: app_bundle(&path).to_string(), profile }
}

/// "/Applications/Firefox Nightly.app" from any path inside it.
pub fn app_bundle(path: &str) -> &str {
    path.find(".app/Contents/MacOS/").map_or(path, |i| &path[..i + 4])
}

/// The profile a copy of Firefox was started with (`-P work`, or
/// `-profile /path/to/work`), if it was given one.
pub fn profile_name(args: &[String]) -> Option<String> {
    let i = args.iter().position(|a| matches!(a.as_str(), "-P" | "-p" | "--P" | "-profile" | "--profile"))?;
    let value = args.get(i + 1).filter(|v| !v.starts_with('-'))?;
    let name = value.trim_end_matches('/').rsplit('/').next().unwrap_or(value);
    (!name.is_empty()).then(|| name.to_string())
}

/// Tie each process to the copy of Firefox it belongs to, and name the
/// copies. A process belongs to the Firefox that started it; one that was
/// handed to launchd (the crash helper does this) belongs to the oldest
/// Firefox from the same app. Returns pid -> browser id, and the browsers,
/// oldest first.
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
        // Firefox itself is gone but this process isn't yet: it stands alone.
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
            Browser { id: m.pid, name: key_name(&key), app: m.app.to_string(), key }
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

/// True for executables inside any Firefox app bundle (release, Developer
/// Edition, Nightly), wherever it's installed.
pub fn is_firefox_path(path: &str) -> bool {
    let Some(i) = path.find(".app/Contents/MacOS/") else { return false };
    let bundle = path[..i].rsplit('/').next().unwrap_or("");
    bundle.starts_with("Firefox")
}

/// Which kind of Firefox process this is. `path` must be a Firefox path.
pub fn classify(path: &str, args: &[String]) -> Kind {
    if path.ends_with("/MacOS/firefox") {
        return Kind::Main;
    }
    if path.contains("gpu-helper") {
        return Kind::Gpu;
    }
    // Web content processes run plugin-container with -isForBrowser and end
    // their arguments with the process type "tab".
    let is_browser = args.iter().any(|a| a == "-isForBrowser");
    if path.contains("plugin-container") && is_browser && args.last().is_some_and(|a| a == "tab") {
        return Kind::Tab;
    }
    Kind::Other
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

    #[test]
    fn recognizes_firefox_bundles_anywhere() {
        assert!(is_firefox_path("/Applications/Firefox.app/Contents/MacOS/firefox"));
        assert!(is_firefox_path(
            "/Applications/Firefox.app/Contents/MacOS/plugin-container.app/Contents/MacOS/plugin-container"
        ));
        assert!(is_firefox_path("/Users/a/Applications/Firefox Nightly.app/Contents/MacOS/firefox"));
        assert!(is_firefox_path("/Applications/Firefox Developer Edition.app/Contents/MacOS/firefox"));
        assert!(!is_firefox_path("/Applications/Safari.app/Contents/MacOS/Safari"));
        assert!(!is_firefox_path("/Applications/Thunderbird.app/Contents/MacOS/thunderbird"));
        assert!(!is_firefox_path("/usr/bin/firefox"));
        // A folder named Firefox isn't enough; the bundle itself must be.
        assert!(!is_firefox_path("/Users/a/Firefox/Other.app/Contents/MacOS/other"));
    }

    #[test]
    fn finds_the_app_and_profile() {
        let nightly = "/Applications/Firefox Nightly.app";
        assert_eq!(app_bundle(&format!("{nightly}/Contents/MacOS/firefox")), nightly);
        assert_eq!(
            app_bundle(&format!("{nightly}/Contents/MacOS/plugin-container.app/Contents/MacOS/plugin-container")),
            nightly
        );
        assert_eq!(profile_name(&args(&["firefox", "-P", "work"])), Some("work".into()));
        assert_eq!(profile_name(&args(&["firefox", "-profile", "/Users/a/Profiles/abc.work/"])), Some("abc.work".into()));
        assert_eq!(profile_name(&args(&["firefox", "-P"])), None);
        assert_eq!(profile_name(&args(&["firefox", "-P", "-no-remote"])), None);
        assert_eq!(profile_name(&args(&["firefox"])), None);
    }

    fn member<'a>(pid: u32, kind: Kind, parent: u32, app: &'a str) -> Member<'a> {
        Member { pid, kind, parent, start: pid as u64, app, profile: None }
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
    fn classifies_process_kinds() {
        let base = "/Applications/Firefox.app/Contents/MacOS";
        let pc = format!("{base}/plugin-container.app/Contents/MacOS/plugin-container");
        assert_eq!(classify(&format!("{base}/firefox"), &[]), Kind::Main);
        assert_eq!(
            classify(&format!("{base}/gpu-helper.app/Contents/MacOS/Firefox GPU Helper"), &[]),
            Kind::Gpu
        );
        assert_eq!(classify(&pc, &args(&["pc", "-isForBrowser", "-prefsHandle", "0:1", "tab"])), Kind::Tab);
        // Utility, socket and media processes aren't tabs.
        assert_eq!(classify(&pc, &args(&["pc", "-appDir", "x", "socket"])), Kind::Other);
        assert_eq!(classify(&pc, &args(&["pc", "-isForBrowser", "rdd"])), Kind::Other);
        assert_eq!(classify(&pc, &args(&["pc", "tab"])), Kind::Other);
        assert_eq!(classify(&format!("{base}/crashhelper"), &[]), Kind::Other);
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
    fn sees_its_own_process_but_not_as_firefox() {
        let me = std::process::id();
        let path = pid_path(me).expect("own path");
        assert!(!is_firefox_path(&path));
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
        println!("{} Firefox processes, {tabs} tabs, {total} MB", procs.len());
        for b in &browsers {
            let n = procs.iter().filter(|p| p.browser == b.id).count();
            println!("{} ({}): {n} processes, {}", b.name, b.id, b.app);
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
