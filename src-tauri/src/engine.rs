//! Decides, from one sample, which tabs are flagged and which one to close.
//! Kept free of I/O so the rules can be tested directly.

use crate::config::{Config, Limits, Mode};
use crate::sampler::{Browser, Kind, Proc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

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
    /// Every copy of Firefox together.
    pub total_mb: u64,
    /// The tab to close this pass, with the reason.
    pub close: Option<(u32, String)>,
    /// Whether that close is for the whole-browser limit (not a per-tab one).
    pub close_by_total: bool,
}

pub fn gb(mb: u64) -> String {
    format!("{:.1} GB", mb as f64 / 1024.0)
}

/// Memory used by each copy of Firefox, by browser id.
pub fn totals(procs: &[Proc]) -> HashMap<u32, u64> {
    let mut t = HashMap::new();
    for p in procs {
        *t.entry(p.browser).or_default() += p.mem_mb;
    }
    t
}

/// `procs` must be sorted biggest first (the sampler does this). The
/// whole-browser limit applies to each copy of Firefox in `browsers`
/// separately, and picks no tab in the ones listed in `held`.
pub fn evaluate(
    cfg: &Config,
    procs: &[Proc],
    browsers: &[Browser],
    spared: &HashSet<u32>,
    can_close_now: bool,
    held: &HashSet<u32>,
) -> Evaluation {
    let total_mb: u64 = procs.iter().map(|p| p.mem_mb).sum();
    let totals = totals(procs);
    let mode = cfg.mode;
    let mut rows: Vec<Row> = procs
        .iter()
        .map(|p| Row { proc: p.clone(), status: Status::Ok, reason: None, action: None })
        .collect();

    // Each copy of Firefox has its own limits.
    let limits: HashMap<u32, Limits> = browsers.iter().map(|b| (b.id, cfg.limits_for(&b.key))).collect();
    let limits_of = |browser: u32| limits.get(&browser).copied().unwrap_or_else(|| cfg.limits_for(""));

    // Per-tab limits.
    for row in rows.iter_mut().filter(|r| r.proc.kind == Kind::Tab) {
        let mb = row.proc.mem_mb;
        let cfg = limits_of(row.proc.browser);
        if spared.contains(&row.proc.pid) {
            row.status = Status::Spared;
            row.reason = Some("You chose to spare this tab".into());
        } else if mb >= cfg.kill_mb {
            row.status = Status::Over;
            row.reason = Some(format!("Over the {} per-tab limit", gb(cfg.kill_mb)));
        } else if mb >= cfg.warn_mb && mode != Mode::Close {
            row.status = Status::Warn;
            row.reason = Some(format!("Past the {} warning level", gb(cfg.warn_mb)));
        }
    }

    // Whole-browser limit, for each copy of Firefox: if nothing in it is
    // already over its own limit, pick its biggest eligible tab, preferring
    // an idle one.
    let mut total_victims = Vec::new();
    for b in browsers {
        let b_total = totals.get(&b.id).copied().unwrap_or(0);
        let lim = limits_of(b.id);
        let mine = |r: &&Row| r.proc.browser == b.id;
        if held.contains(&b.id) || lim.total_mb == 0 || b_total <= lim.total_mb {
            continue;
        }
        if rows.iter().filter(mine).any(|r| r.status == Status::Over) {
            continue;
        }
        let eligible = |r: &&Row| {
            r.proc.browser == b.id
                && r.proc.kind == Kind::Tab
                && r.status != Status::Spared
                && r.proc.mem_mb >= lim.total_min_victim_mb
        };
        let idle = rows.iter().filter(eligible).find(|r| r.proc.cpu < cfg.idle_cpu_percent).map(|r| r.proc.pid);
        let biggest = rows.iter().find(eligible).map(|r| r.proc.pid);
        let victim = if cfg.prefer_idle { idle.or(biggest) } else { biggest };
        if let Some(pid) = victim {
            total_victims.push(pid);
            let row = rows.iter_mut().find(|r| r.proc.pid == pid).unwrap();
            let which = if cfg.prefer_idle && idle == Some(pid) { "biggest idle tab" } else { "biggest tab" };
            row.status = Status::Over;
            row.reason = Some(format!(
                "{} is at {}, over the {} total limit; this is the {which}",
                b.name,
                gb(b_total),
                gb(lim.total_mb)
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

    let close_by_total = close.as_ref().is_some_and(|c| total_victims.contains(&c.0));
    Evaluation { rows, total_mb, close, close_by_total }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(pid: u32, mem_mb: u64, cpu: f32) -> Proc {
        Proc { pid, browser: 0, kind: Kind::Tab, mem_mb, cpu, age_secs: 600, growth_mb_per_min: 0.0 }
    }

    fn browser(id: u32, name: &str) -> Browser {
        let app = format!("/Applications/{name}.app");
        Browser { id, name: name.into(), key: app.clone(), app }
    }

    /// One copy of Firefox (id 0), held or not.
    fn ev(c: &Config, procs: &[Proc], spared: &HashSet<u32>, can_close: bool, held: bool) -> Evaluation {
        let held = if held { HashSet::from([0]) } else { HashSet::new() };
        evaluate(c, procs, &[browser(0, "Firefox")], spared, can_close, &held)
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
        let e = ev(&cfg(Mode::Watch), &procs, &HashSet::new(), true, false);
        assert_eq!(status(&e, 1), Status::Over);
        assert_eq!(e.rows[0].action.as_deref(), Some("Would close"));
        assert_eq!(status(&e, 2), Status::Warn);
        assert_eq!(status(&e, 3), Status::Ok);
        assert!(e.close.is_none());
    }

    #[test]
    fn closes_biggest_over_limit_one_at_a_time() {
        let procs = [tab(1, 3000, 0.0), tab(2, 2200, 0.0)];
        let e = ev(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(1));
        assert_eq!(e.rows[1].action.as_deref(), Some("Next to close"));
    }

    #[test]
    fn cooldown_blocks_closing() {
        let procs = [tab(1, 3000, 0.0)];
        let e = ev(&cfg(Mode::Close), &procs, &HashSet::new(), false, false);
        assert!(e.close.is_none());
        assert_eq!(e.rows[0].action.as_deref(), Some("Next to close"));
    }

    #[test]
    fn total_limit_prefers_idle_tab() {
        let mut procs = vec![tab(1, 1800, 30.0), tab(2, 1500, 0.5)];
        procs.extend((10..17).map(|p| tab(p, 900, 1.0)));
        let e = ev(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert!(e.total_mb > 8192);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(2));
        assert!(e.close.unwrap().1.contains("biggest idle tab"));
    }

    #[test]
    fn spared_tabs_are_left_alone() {
        let procs = [tab(1, 3000, 0.0), tab(2, 500, 0.0)];
        let spared = HashSet::from([1]);
        let e = ev(&cfg(Mode::Close), &procs, &spared, true, false);
        assert_eq!(status(&e, 1), Status::Spared);
        assert!(e.close.is_none());
    }

    #[test]
    fn close_mode_skips_warnings() {
        let procs = [tab(1, 1500, 0.0)];
        let e = ev(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        assert_eq!(status(&e, 1), Status::Ok);
    }

    #[test]
    fn paused_never_closes() {
        let procs = [tab(1, 3000, 0.0)];
        let c = Config { enabled: false, ..cfg(Mode::Close) };
        let e = ev(&c, &procs, &HashSet::new(), true, false);
        assert!(e.close.is_none());
    }
    #[test]
    fn total_limit_off_when_zero() {
        let procs: Vec<Proc> = (1..20).map(|p| tab(p, 900, 0.0)).collect();
        let c = Config { total_mb: 0, ..cfg(Mode::Close) };
        let e = ev(&c, &procs, &HashSet::new(), true, false);
        assert!(e.close.is_none());
        assert!(e.rows.iter().all(|r| r.status == Status::Ok));
    }

    #[test]
    fn total_limit_never_picks_small_tabs() {
        let procs: Vec<Proc> = (1..40).map(|p| tab(p, 300, 0.0)).collect();
        let e = ev(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        assert!(e.total_mb > 8192);
        assert!(e.close.is_none(), "every tab is under the 400 MB minimum");
    }

    #[test]
    fn total_limit_falls_back_to_busy_tab() {
        let mut procs = vec![tab(1, 1500, 50.0), tab(2, 1200, 40.0)];
        procs.extend((10..18).map(|p| tab(p, 900, 20.0)));
        let e = ev(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        let (pid, reason) = e.close.unwrap();
        assert_eq!(pid, 1);
        assert!(reason.ends_with("this is the biggest tab"));
    }

    #[test]
    fn total_limit_ignores_preference_when_off() {
        let mut procs = vec![tab(1, 1800, 30.0), tab(2, 1500, 0.5)];
        procs.extend((10..17).map(|p| tab(p, 900, 1.0)));
        let c = Config { prefer_idle: false, ..cfg(Mode::Close) };
        assert_eq!(ev(&c, &procs, &HashSet::new(), true, false).close.map(|c| c.0), Some(1));
    }

    #[test]
    fn total_limit_skips_spared_and_non_tabs() {
        let gpu = Proc { kind: Kind::Gpu, ..tab(1, 4000, 0.0) };
        let mut procs = vec![gpu, tab(2, 3000, 0.0), tab(3, 1000, 0.0)];
        procs.extend((10..14).map(|p| tab(p, 500, 0.0)));
        let c = Config { kill_mb: 8000, ..cfg(Mode::Close) };
        let e = ev(&c, &procs, &HashSet::from([2]), true, false);
        assert_eq!(e.close.map(|c| c.0), Some(3));
    }

    #[test]
    fn per_tab_limit_wins_over_total_limit() {
        let mut procs = vec![tab(1, 2500, 50.0), tab(2, 1900, 0.0)];
        procs.extend((10..18).map(|p| tab(p, 700, 0.0)));
        let e = ev(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(1));
        assert_eq!(status(&e, 2), Status::Warn, "no second Over from the total rule");
    }

    #[test]
    fn exactly_at_limits_counts_as_over() {
        let procs = [tab(1, 2048, 0.0), tab(2, 1024, 0.0), tab(3, 1023, 0.0)];
        let e = ev(&cfg(Mode::WarnThenClose), &procs, &HashSet::new(), true, false);
        assert_eq!(status(&e, 1), Status::Over);
        assert_eq!(status(&e, 2), Status::Warn);
        assert_eq!(status(&e, 3), Status::Ok);
    }

    #[test]
    fn non_tab_processes_are_never_flagged() {
        let main = Proc { kind: Kind::Main, ..tab(1, 5000, 0.0) };
        let gpu = Proc { kind: Kind::Gpu, ..tab(2, 5000, 0.0) };
        let c = Config { total_mb: 0, ..cfg(Mode::Close) };
        let e = ev(&c, &[main, gpu], &HashSet::new(), true, false);
        assert!(e.rows.iter().all(|r| r.status == Status::Ok));
        assert!(e.close.is_none());
    }

    #[test]
    fn total_counts_every_firefox_process() {
        let main = Proc { kind: Kind::Main, ..tab(1, 700, 0.0) };
        let e = ev(&cfg(Mode::Watch), &[main, tab(2, 300, 0.0)], &HashSet::new(), true, false);
        assert_eq!(e.total_mb, 1000);
    }

    #[test]
    fn held_total_limit_picks_nothing() {
        let procs: Vec<Proc> = (1..12).map(|p| tab(p, 900, 0.0)).collect();
        let held = ev(&cfg(Mode::Close), &procs, &HashSet::new(), true, true);
        assert!(held.close.is_none());
        assert!(held.rows.iter().all(|r| r.status == Status::Ok));
        let active = ev(&cfg(Mode::Close), &procs, &HashSet::new(), true, false);
        assert!(active.close.is_some() && active.close_by_total);
    }

    #[test]
    fn held_total_limit_keeps_per_tab_limits() {
        let mut procs = vec![tab(1, 2500, 0.0)];
        procs.extend((10..18).map(|p| tab(p, 900, 0.0)));
        let e = ev(&cfg(Mode::Close), &procs, &HashSet::new(), true, true);
        assert_eq!(e.close.as_ref().map(|c| c.0), Some(1));
        assert!(!e.close_by_total);
    }

    /// Firefox (id 0) with `a` tabs and Nightly (id 1) with `b` tabs, 900 MB each.
    fn two(a: u32, b: u32) -> (Vec<Proc>, Vec<Browser>) {
        let mut procs: Vec<Proc> = (0..a).map(|i| tab(100 + i, 900, 0.0)).collect();
        procs.extend((0..b).map(|i| Proc { browser: 1, ..tab(200 + i, 900, 0.0) }));
        procs.sort_by(|x, y| y.mem_mb.cmp(&x.mem_mb).then(x.pid.cmp(&y.pid)));
        (procs, vec![browser(0, "Firefox"), browser(1, "Firefox Nightly")])
    }

    #[test]
    fn whole_browser_limit_counts_each_firefox_alone() {
        // 6.3 GB + 6.3 GB: 12.6 GB together, but each is under 8 GB.
        let (procs, browsers) = two(7, 7);
        let e = evaluate(&cfg(Mode::Close), &procs, &browsers, &HashSet::new(), true, &HashSet::new());
        assert_eq!(e.total_mb, 14 * 900);
        assert!(e.close.is_none());
        assert!(e.rows.iter().all(|r| r.status == Status::Ok));
    }

    #[test]
    fn whole_browser_limit_closes_in_the_firefox_thats_over() {
        // Nightly is at 9.7 GB, over the 8 GB limit; Firefox is at 2.6 GB.
        let (procs, browsers) = two(3, 11);
        let e = evaluate(&cfg(Mode::Close), &procs, &browsers, &HashSet::new(), true, &HashSet::new());
        let (pid, reason) = e.close.unwrap();
        assert!((200..211).contains(&pid), "closed {pid}, which isn't a Nightly tab");
        assert!(reason.starts_with("Firefox Nightly is at 9.7 GB"), "{reason}");
        assert!(e.close_by_total);
    }

    #[test]
    fn a_pause_holds_only_its_own_firefox() {
        let (mut procs, browsers) = two(10, 10);
        procs.sort_by_key(|p| p.pid);
        let e = evaluate(&cfg(Mode::Close), &procs, &browsers, &HashSet::new(), true, &HashSet::from([0]));
        assert!(e.close.as_ref().is_some_and(|c| c.0 >= 200), "Nightly still closes");
        assert!(e.rows.iter().filter(|r| r.proc.browser == 0).all(|r| r.status == Status::Ok));
    }

    #[test]
    fn each_firefox_uses_its_own_limits() {
        let (procs, browsers) = two(3, 3);
        let tight = Limits { warn_mb: 500, kill_mb: 800, total_mb: 2048, total_min_victim_mb: 400 };
        let c = Config {
            browser_limits: [(browsers[1].key.clone(), tight)].into(),
            ..cfg(Mode::Watch)
        };
        let e = evaluate(&c, &procs, &browsers, &HashSet::new(), true, &HashSet::new());
        // Nightly's 900 MB tabs are over its 800 MB limit; Firefox's aren't over its 2 GB.
        for r in &e.rows {
            let want = if r.proc.browser == 1 { Status::Over } else { Status::Ok };
            assert_eq!(r.status, want, "pid {}", r.proc.pid);
        }
        assert!(e.rows.iter().any(|r| r.reason.as_deref() == Some("Over the 0.8 GB per-tab limit")));
    }

    #[test]
    fn a_tab_over_its_limit_in_one_firefox_doesnt_stop_the_other() {
        // Firefox has a 3 GB tab (over its per-tab limit); Nightly is over 8 GB.
        let (mut procs, browsers) = two(0, 11);
        procs.insert(0, tab(1, 3000, 0.0));
        let e = evaluate(&cfg(Mode::Watch), &procs, &browsers, &HashSet::new(), true, &HashSet::new());
        assert_eq!(status(&e, 1), Status::Over);
        let over: Vec<u32> = e.rows.iter().filter(|r| r.status == Status::Over).map(|r| r.proc.pid).collect();
        assert_eq!(over.len(), 2, "Firefox's big tab and one Nightly tab: {over:?}");
        assert!(over.iter().any(|p| *p >= 200));
    }

    #[test]
    fn each_firefox_has_its_own_smallest_tab_to_close() {
        // Both copies are over 8 GB with only 900 MB tabs.
        let (procs, browsers) = two(10, 10);
        let fussy = Limits { warn_mb: 1024, kill_mb: 2048, total_mb: 8192, total_min_victim_mb: 1000 };
        let c = Config { browser_limits: [(browsers[0].key.clone(), fussy)].into(), ..cfg(Mode::Watch) };
        let e = evaluate(&c, &procs, &browsers, &HashSet::new(), true, &HashSet::new());
        let over: Vec<u32> = e.rows.iter().filter(|r| r.status == Status::Over).map(|r| r.proc.pid).collect();
        assert_eq!(over.len(), 1);
        assert!(over[0] >= 200, "Firefox won't close a tab under 1000 MB; Nightly will");
    }

    #[test]
    fn gb_formats_one_decimal() {
        assert_eq!(gb(1024), "1.0 GB");
        assert_eq!(gb(1536), "1.5 GB");
        assert_eq!(gb(100), "0.1 GB");
    }
}
