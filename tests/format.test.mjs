// node --test tests/
import test from "node:test";
import assert from "node:assert/strict";
import {
  gb, mbOrGb, duration, growth, summaryParts, shortName, bannerText, settingsToConfig, limitsFor, esc,
  keptFilter, filterRows, filterChoices, rowBadges, tabLabel, limitChoice, limitChoices, headline, whichSite,
  pauseEnd,
} from "../src/format.js";

const config = {
  enabled: true, mode: "watch", warn_mb: 1024, kill_mb: 2048, total_mb: 8192, total_min_victim_mb: 400,
  total_backoff_mins: 15, total_max_per_hour: 3, share_usage: false, auto_update: true, welcomed: true,
  interval_secs: 15, kill_cooldown_secs: 60, prefer_idle: true, idle_cpu_percent: 2, notify_on_close: false,
};

test("memory sizes", () => {
  assert.equal(gb(1024), "1.0 GB");
  assert.equal(gb(9216), "9.0 GB");
  assert.equal(mbOrGb(512), "512 MB");
  assert.equal(mbOrGb(1023), "1023 MB");
  assert.equal(mbOrGb(1024), "1.0 GB");
});

test("durations", () => {
  assert.equal(duration(0), "0s");
  assert.equal(duration(59), "59s");
  assert.equal(duration(60), "1m");
  assert.equal(duration(3599), "59m");
  assert.equal(duration(3600), "1h 0m");
  assert.equal(duration(32358), "8h 59m");
  assert.equal(duration(48 * 3600), "2d");
});

test("growth", () => {
  assert.equal(growth(0), "—");
  assert.equal(growth(0.4), "—");
  assert.equal(growth(14.2), "+14 MB/min");
  assert.equal(growth(-3), "-3 MB/min");
});

test("summary line", () => {
  assert.equal(summaryParts({ over: 0, flagged: 0, cooldown_secs: 0 }), "Everything's within your limits");
  assert.equal(summaryParts({ over: 1, flagged: 2, cooldown_secs: 0 }), "1 tab process over a limit · 2 tab processes past the warning level");
  assert.equal(summaryParts({ over: 1, flagged: 0, cooldown_secs: 40 }), "1 tab process over a limit · next close allowed in 40s");
});

test("banner", () => {
  assert.equal(bannerText(config, { over: 0 }), null);
  assert.match(bannerText(config, { over: 1 }), /1 tab process would be closed/);
  assert.match(bannerText(config, { over: 3 }), /3 tab processes would be closed/);
  assert.match(bannerText({ ...config, enabled: false }, { over: 0 }), /paused/);
  assert.equal(bannerText({ ...config, mode: "close" }, { over: 2 }), null);
});

test("banner for a paused whole-browser limit", () => {
  const total_hold = "Whole-browser limit paused until 08:31: closing a 0.9 GB tab process didn't lower Firefox's total";
  assert.equal(
    bannerText({ ...config, mode: "close" }, { over: 0, total_hold }),
    `${total_hold}. Tabs over a per-tab limit are still closed.`,
  );
  assert.equal(bannerText(config, { over: 0, total_hold }), null, "watch only never pauses it");
  assert.match(bannerText({ ...config, mode: "close", enabled: false }, { over: 0, total_hold }), /paused\. It shows what it would close, but closes nothing/);
});

test("settings form to config", () => {
  const form = {
    warn_gb: "1.5", kill_gb: "3", total_gb: "0", total_min_victim_mb: "250", prefer_idle: false,
    idle_cpu_percent: "5", interval_secs: "30", kill_cooldown_secs: "90", notify_on_close: true,
    total_backoff_mins: "30", total_max_per_hour: "0", share_usage: true, auto_update: false,
  };
  const c = settingsToConfig(config, form);
  assert.equal(c.warn_mb, 1536);
  assert.equal(c.kill_mb, 3072);
  assert.equal(c.total_mb, 0, "0 turns the total limit off");
  assert.equal(c.total_min_victim_mb, 250);
  assert.equal(c.prefer_idle, false);
  assert.equal(c.interval_secs, 30);
  assert.equal(c.notify_on_close, true);
  assert.equal(c.total_backoff_mins, 30);
  assert.equal(c.total_max_per_hour, 0, "0 turns the cap off");
  assert.equal(c.share_usage, true);
  assert.equal(c.auto_update, false);
  assert.equal(c.welcomed, true, "the form never resets the welcome card");
  assert.equal(c.mode, "watch", "keeps fields the form doesn't have");
});

test("blank or junk fields keep the current value", () => {
  const c = settingsToConfig(config, { warn_gb: "", kill_gb: "abc", total_gb: undefined, interval_secs: "" });
  assert.equal(c.warn_mb, 1024);
  assert.equal(c.kill_mb, 2048);
  assert.equal(c.total_mb, 8192);
  assert.equal(c.interval_secs, 15);
});

test("escaping", () => {
  assert.equal(esc(`<b>"Tom" & 'Jerry'</b>`), "&lt;b&gt;&quot;Tom&quot; &amp; &#39;Jerry&#39;&lt;/b&gt;");
  assert.equal(esc(42), "42");
});

test("several Firefoxes are named and sized in the summary", () => {
  assert.equal(shortName("Firefox"), "Firefox");
  assert.equal(shortName("Firefox Nightly"), "Nightly");
  assert.equal(shortName("Firefox Developer Edition (2)"), "Developer Edition (2)");
  assert.equal(shortName("Firefox (work)"), "Firefox (work)");
  const browsers = [
    { name: "Firefox", total_mb: 7270 },
    { name: "Firefox Nightly", total_mb: 2355 },
  ];
  assert.equal(
    summaryParts({ over: 0, flagged: 1, cooldown_secs: 0, browsers }),
    "Firefox 7.1 GB · Nightly 2.3 GB · 1 tab process past the warning level",
  );
  // One Firefox: no breakdown.
  assert.equal(summaryParts({ over: 0, flagged: 0, cooldown_secs: 0, browsers: browsers.slice(0, 1) }), "Everything's within your limits");
});

test("limits can be set for one copy of Firefox", () => {
  const dev = "/Applications/Firefox Developer Edition.app";
  const form = { warn_gb: "0.5", kill_gb: "1", total_gb: "4", total_min_victim_mb: "300", prefer_idle: true,
    idle_cpu_percent: "2", interval_secs: "15", kill_cooldown_secs: "60", total_backoff_mins: "15", total_max_per_hour: "3" };
  const c = settingsToConfig({ ...config, browser_limits: {} }, form, dev);
  assert.deepEqual(c.browser_limits[dev], { warn_mb: 512, kill_mb: 1024, total_mb: 4096, total_min_victim_mb: 300 });
  assert.equal(c.kill_mb, 2048, "the main limits are untouched");
  assert.deepEqual(limitsFor(c, dev).total_mb, 4096);
  assert.equal(limitsFor(c, "/Applications/Firefox.app").total_mb, 8192, "others use the main limits");
  assert.equal(limitsFor(c, null).kill_mb, 2048);
  // A blank field keeps that copy's own value, not the main one.
  const again = settingsToConfig(c, { ...form, total_gb: "" }, dev);
  assert.equal(again.browser_limits[dev].total_mb, 4096);
});

const fx = { id: 10, name: "Firefox" };
const dev = { id: 20, name: "Firefox Developer Edition" };
const names = { 10: "Firefox", 20: "Firefox Developer Edition" };

test("the process filter", () => {
  const rows = [{ pid: 1, browser: 10 }, { pid: 2, browser: 20 }, { pid: 3, browser: 10 }];
  assert.deepEqual(filterRows(rows, null), rows);
  assert.deepEqual(filterRows(rows, 20).map((r) => r.pid), [2]);
  assert.deepEqual(filterChoices([fx, dev]), [["all", "All"], ["10", "Firefox"], ["20", "Developer Edition"]]);
  assert.equal(keptFilter(20, [fx, dev]), 20);
  assert.equal(keptFilter(20, [fx, { id: 30, name: "Firefox Nightly" }]), null, "back to All once it quits");
  assert.equal(keptFilter(10, [fx]), null, "no filter with one Firefox");
});

test("row labels say which browser only when several run", () => {
  const tab = { pid: 7, kind: "tab", browser: 20, mem_mb: 1536 };
  assert.deepEqual(rowBadges(tab, names, false), [{ text: "Tab" }]);
  assert.deepEqual(rowBadges(tab, names, true), [{ text: "Tab" }, { text: "Developer Edition", browser: true }]);
  assert.deepEqual(rowBadges({ ...tab, kind: "main" }, names, true), [{ text: "Developer Edition" }]);
  assert.deepEqual(rowBadges({ ...tab, kind: "main" }, names, false), [{ text: "Developer Edition" }], "a main process is named after its browser");
  assert.deepEqual(rowBadges({ ...tab, browser: 99 }, names, true)[1].text, "Browser", "an unknown browser reads as Browser");
  assert.equal(tabLabel(tab, names, false), "tab process 7 (1.5 GB)");
  assert.equal(tabLabel(tab, names, true), "Firefox Developer Edition tab process 7 (1.5 GB)");
});

test("the Limits for picker", () => {
  const t = (key, name, running = true, own = false) => ({ key, name, running, own });
  const release = t("/A/Firefox.app", "Firefox");
  const devEd = t("/A/Firefox Developer Edition.app", "Firefox Developer Edition");
  // One Firefox with no limits of its own: no picker, the main limits.
  assert.deepEqual(limitChoice([release], null), { choosing: false, key: null, name: null });
  assert.deepEqual(limitChoice([], "/A/Firefox.app"), { choosing: false, key: null, name: null });
  // Two: the first is picked, and a choice sticks.
  assert.equal(limitChoice([release, devEd], null).key, "/A/Firefox.app");
  assert.equal(limitChoice([release, devEd], devEd.key).name, "Firefox Developer Edition");
  // The chosen copy quit and has no own limits: back to the first.
  assert.equal(limitChoice([release, t("/A/Firefox Nightly.app", "Firefox Nightly")], devEd.key).key, release.key);
  // One copy with limits of its own still shows the picker, so it's clear whose they are.
  assert.equal(limitChoice([{ ...devEd, own: true }], null).choosing, true);
  assert.deepEqual(limitChoices([release, t("/A/Firefox Nightly.app", "Firefox Nightly", false, true)]), [
    ["/A/Firefox.app", "Firefox", ""],
    ["/A/Firefox Nightly.app", "Nightly (not running)", "not-running"],
  ]);
});

test("the headline names the browser, or says browsers", () => {
  const s = { total_mb: 9216, tab_count: 21 };
  assert.equal(headline({ ...s, browsers: [{ name: "Firefox" }] }), "Firefox is using 9.0 GB · 21 tab processes");
  assert.equal(headline({ ...s, browsers: [{ name: "Google Chrome" }] }), "Google Chrome is using 9.0 GB · 21 tab processes");
  assert.equal(headline({ ...s, browsers: [{ name: "Firefox" }, { name: "Google Chrome" }] }), "Your browsers are using 9.0 GB · 21 tab processes");
});

test("which site is which: about:processes for Firefox, process-internals for Chrome", () => {
  const firefox = { id: 1, name: "Firefox", family: "gecko" };
  const chrome = { id: 2, name: "Google Chrome", family: "chromium" };
  assert.deepEqual(whichSite([firefox]).buttons, [{ id: 1, label: "Which site is which? Open about:processes", aria: "Which site is which? Open about:processes" }]);
  assert.equal(whichSite([chrome]).buttons[0].label, "Which site is which? Open chrome://process-internals");
  assert.equal(whichSite([]).buttons[0].id, null);
  const both = whichSite([firefox, chrome]);
  assert.equal(both.intro, "Which site is which? See the process list in");
  assert.deepEqual(both.buttons.map((b) => [b.label, b.aria]), [
    ["Firefox", "Open about:processes in Firefox"],
    ["Google Chrome", "Open chrome://process-internals in Google Chrome"],
  ]);
});

test("a timed pause says when it ends", () => {
  const now = new Date(2026, 9, 28, 13, 48).getTime() / 1000;
  const at = (d, h, m) => new Date(2026, 9, d, h, m).getTime() / 1000;
  // Times follow the Mac's own format, 12- or 24-hour.
  const clockAt = (d) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  assert.equal(pauseEnd(at(28, 14, 48), now), clockAt(new Date(at(28, 14, 48) * 1000)));
  assert.equal(pauseEnd(at(29, 6, 0), now), `tomorrow ${clockAt(new Date(at(29, 6, 0) * 1000))}`);
  const paused = { ...config, enabled: false, paused_until: at(28, 14, 48) };
  assert.match(bannerText(paused, { over: 0 }, now), new RegExp(`^Tab Guard is paused until ${clockAt(new Date(at(28, 14, 48) * 1000))}, then turns itself back on\\.`));
  assert.match(bannerText({ ...paused, paused_until: null }, { over: 0 }, now), /until you turn it back on/);
});
