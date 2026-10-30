//! Decides, from one sample, which tabs are flagged and which one to close.
//! Kept free of I/O so the rules can be tested directly.

use crate::config::{Config, Mode};
use crate::sampler::{Kind, Proc};
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Ok,
    /// Past the warning level.
    Warn,
    /// Past a hard limit: closed in the closing modes, "would close" in Watch.
    Over,
    /// The user asked to leave this one alone.
    Spared,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub proc: Proc,
    pub status: Status,
    /// Why it's flagged, in words.
    pub reason: Option<String>,
    /// What happens to it: "Would close", "Closing now", "Next to close".
    pub action: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Evaluation {
    pub rows: Vec<Row>,
    pub total_mb: u64,
    /// The tab to close this pass, with the reason.
    pub close: Option<(u32, String)>,
    /// Whether that close is for the whole-browser limit (not a per-tab one).
    pub close_by_total: bool,
}

pub fn gb(mb: u64) -> String {
    format!("{:.1} GB", mb as f64 / 1024.0)
}

/// `procs` must be sorted biggest first (the sampler does this). While
/// `total_held`, the whole-browser limit picks no tab.
pub fn evaluate(
    cfg: &Config,
    procs: &[Proc],
    spared: &HashSet<u32>,
    can_close_now: bool,
    total_held: bool,
) -> Evaluation {
    let total_mb: u64 = procs.iter().map(|p| p.mem_mb).sum();
    let mut rows: Vec<Row> = procs
        .iter()
        .map(|p| Row { proc: p.clone(), status: Status::Ok, reason: None, action: None })
        .collect();

    // Per-tab limits.
    for row in rows.iter_mut().filter(|r| r.proc.kind == Kind::Tab) {
        let mb = row.proc.mem_mb;
        if spared.contains(&row.proc.pid) {
            row.status = Status::Spared;
            row.reason = Some("You chose to spare this tab".into());
        } else if mb >= cfg.kill_mb {
            row.status = Status::Over;
            row.reason = Some(format!("Over the {} per-tab limit", gb(cfg.kill_mb)));
        } else if mb >= cfg.warn_mb && cfg.mode != Mode::Close {
            row.status = Status::Warn;
            row.reason = Some(format!("Past the {} warning level", gb(cfg.warn_mb)));
        }
    }

    // Whole-browser limit: if nothing is already over its own limit, pick the
    // biggest eligible tab, preferring an idle one.
    let mut total_victim = None;
    if !total_held && cfg.total_mb > 0 && total_mb > cfg.total_mb && !rows.iter().any(|r| r.status == Status::Over) {
        let eligible = |r: &&Row| {
            r.proc.kind == Kind::Tab && r.status != Status::Spared && r.proc.mem_mb >= cfg.total_min_victim_mb
        };
        let idle = rows.iter().filter(eligible).find(|r| r.proc.cpu < cfg.idle_cpu_percent).map(|r| r.proc.pid);
        let biggest = rows.iter().find(eligible).map(|r| r.proc.pid);
        let victim = if cfg.prefer_idle { idle.or(biggest) } else { biggest };
        total_victim = victim;
        if let Some(pid) = victim {
            let row = rows.iter_mut().find(|r| r.proc.pid == pid).unwrap();
            let which = if cfg.prefer_idle && idle == Some(pid) { "biggest idle tab" } else { "biggest tab" };
            row.status = Status::Over;
            row.reason = Some(format!(
                "Firefox is at {}, over the {} total limit; this is the {which}",
                gb(total_mb),
                gb(cfg.total_mb)
            ));
        }
    }

    // One close per pass, biggest offender first.
    let mut close = None;
    let mut first = true;
    for row in rows.iter_mut().filter(|r| r.status == Status::Over) {
        row.action = Some(if !cfg.mode.closes() || !cfg.enabled {
            "Would close".into()
        } else if first && can_close_now {
            close = Some((row.proc.pid, row.reason.clone().unwrap_or_default()));
            "Closing now".into()
        } else {
            "Next to close".into()
        });
        first = false;
    }

    let close_by_total = close.is_some() && close.as_ref().map(|c| c.0) == total_victim;
    Evaluation { rows, total_mb, close, close_by_total }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(pid: u32, mem_mb: u64, cpu: f32) -> Proc {
        Proc { pid, kind: Kind::Tab, mem_mb, cpu, age_secs: 600, growth_mb_per_min: 0.0 }
    }

    fn cfg(mode: Mode) -> Config {
        Config { mode, ..Config::default() }
    }

    fn status(e: &Evaluation, pid: u32) -> Status {
        e.rows.iter().find(|r| r.proc.pid == pid).unwrap().status
    }

    #[test]
    fn watch_mode_flags_but_never_closes() {
        let procs = [tab(1, 2500, 0.0), tab(2, 1200, 0.0), tab(3, 300, 0.0)];
        let e = evaluate(&cfg(Mode::Watch), &procs, &HashSet::new(), true, false);
        assert_eq!(status(&e, 1), Status::Over);
        assert_eq!(e.rows[0].action.as_deref(), Some("Would close"));
        assert_eq!(status(&e, 2), Status::Warn);
        assert_eq!(status(&e, 3), Status::Ok);
        assert!(e.close.is_none());
    }

    #[test]
    fn closes_biggest_over_limit_one_at_a_time() {
        let procs = [tab(1, 3000, 0.0), tab(2, 2200, 0.0)];
        let e = evaluate(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(1));
        assert_eq!(e.rows[1].action.as_deref(), Some("Next to close"));
    }

    #[test]
    fn cooldown_blocks_closing() {
        let procs = [tab(1, 3000, 0.0)];
        let e = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), false, false);
        assert!(e.close.is_none());
        assert_eq!(e.rows[0].action.as_deref(), Some("Next to close"));
    }

    #[test]
    fn total_limit_prefers_idle_tab() {
        let mut procs = vec![tab(1, 1800, 30.0), tab(2, 1500, 0.5)];
        procs.extend((10..17).map(|p| tab(p, 900, 1.0)));
        let e = evaluate(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert!(e.total_mb > 8192);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(2));
        assert!(e.close.unwrap().1.contains("biggest idle tab"));
    }

    #[test]
    fn spared_tabs_are_left_alone() {
        let procs = [tab(1, 3000, 0.0), tab(2, 500, 0.0)];
        let spared = HashSet::from([1]);
        let e = evaluate(&cfg(Mode::Close), &procs, &spared, true, false);
        assert_eq!(status(&e, 1), Status::Spared);
        assert!(e.close.is_none());
    }

    #[test]
    fn close_mode_skips_warnings() {
        let procs = [tab(1, 1500, 0.0)];
        let e = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        assert_eq!(status(&e, 1), Status::Ok);
    }

    #[test]
    fn paused_never_closes() {
        let procs = [tab(1, 3000, 0.0)];
        let c = Config { enabled: false, ..cfg(Mode::Close) };
        let e = evaluate(&c, &procs, &HashSet::new(), true, false);
        assert!(e.close.is_none());
    }
    #[test]
    fn total_limit_off_when_zero() {
        let procs: Vec<Proc> = (1..20).map(|p| tab(p, 900, 0.0)).collect();
        let c = Config { total_mb: 0, ..cfg(Mode::Close) };
        let e = evaluate(&c, &procs, &HashSet::new(), true, false);
        assert!(e.close.is_none());
        assert!(e.rows.iter().all(|r| r.status == Status::Ok));
    }

    #[test]
    fn total_limit_never_picks_small_tabs() {
        let procs: Vec<Proc> = (1..40).map(|p| tab(p, 300, 0.0)).collect();
        let e = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        assert!(e.total_mb > 8192);
        assert!(e.close.is_none(), "every tab is under the 400 MB minimum");
    }

    #[test]
    fn total_limit_falls_back_to_busy_tab() {
        let mut procs = vec![tab(1, 1500, 50.0), tab(2, 1200, 40.0)];
        procs.extend((10..18).map(|p| tab(p, 900, 20.0)));
        let e = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        let (pid, reason) = e.close.unwrap();
        assert_eq!(pid, 1);
        assert!(reason.ends_with("this is the biggest tab"));
    }

    #[test]
    fn total_limit_ignores_preference_when_off() {
        let mut procs = vec![tab(1, 1800, 30.0), tab(2, 1500, 0.5)];
        procs.extend((10..17).map(|p| tab(p, 900, 1.0)));
        let c = Config { prefer_idle: false, ..cfg(Mode::Close) };
        assert_eq!(evaluate(&c, &procs, &HashSet::new(), true, false).close.map(|c| c.0), Some(1));
    }

    #[test]
    fn total_limit_skips_spared_and_non_tabs() {
        let gpu = Proc { kind: Kind::Gpu, ..tab(1, 4000, 0.0) };
        let mut procs = vec![gpu, tab(2, 3000, 0.0), tab(3, 1000, 0.0)];
        procs.extend((10..14).map(|p| tab(p, 500, 0.0)));
        let c = Config { kill_mb: 8000, ..cfg(Mode::Close) };
        let e = evaluate(&c, &procs, &HashSet::from([2]), true, false);
        assert_eq!(e.close.map(|c| c.0), Some(3));
    }

    #[test]
    fn per_tab_limit_wins_over_total_limit() {
        let mut procs = vec![tab(1, 2500, 50.0), tab(2, 1900, 0.0)];
        procs.extend((10..18).map(|p| tab(p, 700, 0.0)));
        let e = evaluate(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(1));
        assert_eq!(status(&e, 2), Status::Warn, "no second Over from the total rule");
    }

    #[test]
    fn exactly_at_limits_counts_as_over() {
        let procs = [tab(1, 2048, 0.0), tab(2, 1024, 0.0), tab(3, 1023, 0.0)];
        let e = evaluate(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert_eq!(status(&e, 1), Status::Over);
        assert_eq!(status(&e, 2), Status::Warn);
        assert_eq!(status(&e, 3), Status::Ok);
    }

    #[test]
    fn non_tab_processes_are_never_flagged() {
        let main = Proc { kind: Kind::Main, ..tab(1, 5000, 0.0) };
        let gpu = Proc { kind: Kind::Gpu, ..tab(2, 5000, 0.0) };
        let c = Config { total_mb: 0, ..cfg(Mode::Close) };
        let e = evaluate(&c, &[main, gpu], &HashSet::new(), true, false);
        assert!(e.rows.iter().all(|r| r.status == Status::Ok));
        assert!(e.close.is_none());
    }

    #[test]
    fn total_counts_every_firefox_process() {
        let main = Proc { kind: Kind::Main, ..tab(1, 700, 0.0) };
        let e = evaluate(&cfg(Mode::Watch), &[main, tab(2, 300, 0.0)], &HashSet::new(), true, false);
        assert_eq!(e.total_mb, 1000);
    }

    #[test]
    fn held_total_limit_picks_nothing() {
        let procs: Vec<Proc> = (1..12).map(|p| tab(p, 900, 0.0)).collect();
        let held = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), true, true);
        assert!(held.close.is_none());
        assert!(held.rows.iter().all(|r| r.status == Status::Ok));
        let active = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        assert!(active.close.is_some() && active.close_by_total);
    }

    #[test]
    fn held_total_limit_keeps_per_tab_limits() {
        let mut procs = vec![tab(1, 2500, 0.0)];
        procs.extend((10..18).map(|p| tab(p, 900, 0.0)));
        let e = evaluate(&cfg(Mode::Close), &procs, &HashSet::new(), true, true);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(1));
        assert!(!e.close_by_total);
    }

    #[test]
    fn gb_formats_one_decimal() {
        assert_eq!(gb(1024), "1.0 GB");
        assert_eq!(gb(1536), "1.5 GB");
        assert_eq!(gb(100), "0.1 GB");
    }
}
