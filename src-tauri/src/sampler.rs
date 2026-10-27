//! Finds Firefox's processes and measures them.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

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

pub struct Sampler {
    system: System,
    /// pid -> recent (time, MB) samples, for the growth rate.
    history: HashMap<u32, VecDeque<(Instant, u64)>>,
}

const HISTORY_WINDOW_SECS: u64 = 300;

impl Sampler {
    pub fn new() -> Self {
        Sampler { system: System::new(), history: HashMap::new() }
    }

    pub fn sample(&mut self) -> Vec<Proc> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let now = Instant::now();

        let mut procs = Vec::new();
        for (pid, p) in self.system.processes() {
            let Some(exe) = p.exe() else { continue };
            let exe = exe.to_string_lossy();
            let Some(kind) = classify(&exe, p.cmd()) else { continue };
            let pid = pid.as_u32();
            let mem_mb = footprint_bytes(pid).unwrap_or_else(|| p.memory()) / (1024 * 1024);

            let hist = self.history.entry(pid).or_default();
            hist.push_back((now, mem_mb));
            while hist.front().is_some_and(|(t, _)| now.duration_since(*t).as_secs() > HISTORY_WINDOW_SECS) {
                hist.pop_front();
            }
            let growth_mb_per_min = match (hist.front(), hist.back()) {
                (Some((t0, m0)), Some((t1, m1))) if t1.duration_since(*t0).as_secs() >= 20 => {
                    (*m1 as f64 - *m0 as f64) / (t1.duration_since(*t0).as_secs_f64() / 60.0)
                }
                _ => 0.0,
            };

            procs.push(Proc {
                pid,
                kind,
                mem_mb,
                cpu: p.cpu_usage(),
                age_secs: now_unix.saturating_sub(p.start_time()),
                growth_mb_per_min,
            });
        }
        self.history.retain(|pid, _| self.system.process(Pid::from_u32(*pid)).is_some());
        procs.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb));
        procs
    }
}

/// Which kind of Firefox process this is, or None if it isn't Firefox.
fn classify(exe: &str, cmd: &[std::ffi::OsString]) -> Option<Kind> {
    if !(exe.starts_with("/Applications/Firefox") && exe.contains(".app/Contents/MacOS/")) {
        return None;
    }
    if exe.ends_with("/MacOS/firefox") {
        return Some(Kind::Main);
    }
    if exe.contains("gpu-helper") {
        return Some(Kind::Gpu);
    }
    // Web content processes run plugin-container with -isForBrowser and end
    // their arguments with the process type "tab".
    let is_browser = cmd.iter().any(|a| a == "-isForBrowser");
    if exe.contains("plugin-container") && is_browser && cmd.last().is_some_and(|a| a == "tab") {
        return Some(Kind::Tab);
    }
    Some(Kind::Other)
}

/// Physical footprint in bytes. Unlike resident size, this doesn't count
/// memory shared with other processes, so it can be summed.
fn footprint_bytes(pid: u32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr() as *mut libc::rusage_info_t,
        )
    };
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs Firefox running: `cargo test live_sample -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_sample() {
        let mut s = Sampler::new();
        s.sample();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let procs = s.sample();
        let total: u64 = procs.iter().map(|p| p.mem_mb).sum();
        let tabs = procs.iter().filter(|p| p.kind == Kind::Tab).count();
        println!("{} Firefox processes, {tabs} tabs, {total} MB", procs.len());
        for p in procs.iter().take(8) {
            println!("{:>6} {:?} {} MB cpu {:.1}% age {}s", p.pid, p.kind, p.mem_mb, p.cpu, p.age_secs);
        }
        assert!(tabs > 0, "is Firefox running?");
    }
}
