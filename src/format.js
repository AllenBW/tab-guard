// Pure helpers for the window, kept separate so they can be tested with node.

export const gb = (mb) => `${(mb / 1024).toFixed(1)} GB`;

export const mbOrGb = (mb) => (mb >= 1024 ? gb(mb) : `${mb} MB`);

/** 45s, 12m, 3h 5m, 4d */
export function duration(secs) {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  const h = Math.floor(secs / 3600);
  return h < 48 ? `${h}h ${Math.floor((secs % 3600) / 60)}m` : `${Math.floor(h / 24)}d`;
}

/** "+14 MB/min", "-3 MB/min", or "—" when flat. */
export function growth(mbPerMin) {
  const g = Math.round(mbPerMin);
  if (!g) return "—";
  return `${g > 0 ? "+" : ""}${g} MB/min`;
}

/** "Nightly" for "Firefox Nightly", for labels where "Firefox" goes without
 *  saying. "Firefox" and "Firefox (work)" stay as they are. */
export function shortName(name) {
  const rest = name.replace(/^Firefox /, "");
  return rest === name || rest.startsWith("(") ? name : rest;
}

/** The headline's second line. With more than one Firefox running, it
 *  starts with each one's size. */
export function summaryParts(s) {
  const parts = [];
  if (s.browsers?.length > 1) {
    parts.push(s.browsers.map((b) => `${shortName(b.name)} ${gb(b.total_mb)}`).join(" · "));
  }
  if (s.over) parts.push(`${s.over} over a limit`);
  if (s.flagged) parts.push(`${s.flagged} past the warning level`);
  if (!parts.length) parts.push("Everything's within your limits");
  if (s.cooldown_secs) parts.push(`next close allowed in ${s.cooldown_secs}s`);
  return parts.join(" · ");
}

/** What the banner under the mode switch says, or null for no banner. */
export function bannerText(config, s) {
  if (!config.enabled) {
    return "Tab Guard is paused. It's still measuring, but won't flag or close anything until you turn it back on.";
  }
  if (config.mode === "watch" && s.over) {
    const n = s.over;
    return `Watch only: ${n} tab process${n > 1 ? "es" : ""} would be closed right now. Switch modes to let Tab Guard act.`;
  }
  if (config.mode !== "watch" && s.total_hold) {
    return `${s.total_hold}. Tabs over a per-tab limit are still closed.`;
  }
  return null;
}

/** The limits for one copy of Firefox (by its key), or the main limits for
 *  `null` and for copies without their own. */
export function limitsFor(config, key) {
  const own = key != null ? config.browser_limits?.[key] : null;
  if (own) return own;
  const { warn_mb, kill_mb, total_mb, total_min_victim_mb } = config;
  return { warn_mb, kill_mb, total_mb, total_min_victim_mb };
}

/** Turn the settings form's values (GB, strings) back into a config. Blank or
 *  invalid fields keep their current value. The limits go to the copy of
 *  Firefox `target` names (a key), or to the main limits when it's null. */
export function settingsToConfig(current, form, target = null) {
  const num = (v, fallback) => (v !== "" && v != null && Number.isFinite(+v) ? +v : fallback);
  const was = limitsFor(current, target);
  const limits = {
    warn_mb: Math.round(num(form.warn_gb, was.warn_mb / 1024) * 1024),
    kill_mb: Math.round(num(form.kill_gb, was.kill_mb / 1024) * 1024),
    total_mb: Math.round(num(form.total_gb, was.total_mb / 1024) * 1024),
    total_min_victim_mb: Math.round(num(form.total_min_victim_mb, was.total_min_victim_mb)),
  };
  const placed =
    target == null ? limits : { browser_limits: { ...(current.browser_limits ?? {}), [target]: limits } };
  return {
    ...current,
    ...placed,
    total_backoff_mins: Math.round(num(form.total_backoff_mins, current.total_backoff_mins)),
    total_max_per_hour: Math.round(num(form.total_max_per_hour, current.total_max_per_hour)),
    prefer_idle: Boolean(form.prefer_idle),
    idle_cpu_percent: num(form.idle_cpu_percent, current.idle_cpu_percent),
    interval_secs: Math.round(num(form.interval_secs, current.interval_secs)),
    kill_cooldown_secs: Math.round(num(form.kill_cooldown_secs, current.kill_cooldown_secs)),
    notify_on_close: Boolean(form.notify_on_close),
    share_usage: Boolean(form.share_usage),
    auto_update: Boolean(form.auto_update),
  };
}

/** Escape text for innerHTML. */
export const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
