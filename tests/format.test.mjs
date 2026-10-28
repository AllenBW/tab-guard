// node --test tests/
import test from "node:test";
import assert from "node:assert/strict";
import { gb, mbOrGb, duration, growth, summaryParts, bannerText, settingsToConfig, esc } from "../src/format.js";

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
  assert.equal(summaryParts({ over: 1, flagged: 2, cooldown_secs: 0 }), "1 over a limit · 2 past the warning level");
  assert.equal(summaryParts({ over: 1, flagged: 0, cooldown_secs: 40 }), "1 over a limit · next close allowed in 40s");
});

test("banner", () => {
  assert.equal(bannerText(config, { over: 0 }), null);
  assert.match(bannerText(config, { over: 1 }), /1 tab process would be closed/);
  assert.match(bannerText(config, { over: 3 }), /3 tab processes would be closed/);
  assert.match(bannerText({ ...config, enabled: false }, { over: 0 }), /paused/);
  assert.equal(bannerText({ ...config, mode: "close" }, { over: 2 }), null);
});

test("banner for a paused whole-browser limit", () => {
  const total_hold = "Whole-browser limit paused until 08:31: closing a 0.9 GB tab didn't bring Firefox down";
  assert.equal(
    bannerText({ ...config, mode: "close" }, { over: 0, total_hold }),
    `${total_hold}. Tabs over a per-tab limit are still closed.`,
  );
  assert.equal(bannerText(config, { over: 0, total_hold }), null, "watch only never pauses it");
  assert.match(bannerText({ ...config, mode: "close", enabled: false }, { over: 0, total_hold }), /paused\. It's still/);
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
