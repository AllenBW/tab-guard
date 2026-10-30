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

/** The headline's second line. With more than one browser running, it
 *  starts with each one's size. */
export function summaryParts(s) {
  const parts = [];
  if (s.browsers?.length > 1) {
    parts.push(s.browsers.map((b) => `${shortName(b.name)} ${gb(b.total_mb)}`).join(" · "));
  }
  const procs = (n) => `${n} tab process${n === 1 ? "" : "es"}`;
  if (s.over) parts.push(`${procs(s.over)} over a limit`);
  if (s.flagged) parts.push(`${procs(s.flagged)} past the warning level`);
  if (!parts.length) parts.push("Everything's within your limits");
  if (s.cooldown_secs) parts.push(`next close allowed in ${s.cooldown_secs}s`);
  return parts.join(" · ");
}

/** A clock time the way this Mac shows them: "2:30 PM" or "14:30". */
export const clock = (date) => date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/** When a pause ends: "2:30 PM", or "tomorrow 6:00 AM". */
export function pauseEnd(until, now = Date.now() / 1000) {
  const end = new Date(until * 1000);
  return end.toDateString() === new Date(now * 1000).toDateString() ? clock(end) : `tomorrow ${clock(end)}`;
}

/** What the banner under the mode switch says, or null for no banner. */
export function bannerText(config, s, now = Date.now() / 1000) {
  if (!config.enabled && config.paused_until) {
    return `Tab Guard is paused until ${pauseEnd(config.paused_until, now)}, then turns itself back on. Until then it shows what it would close, but closes nothing.`;
  }
  if (!config.enabled) {
    return "Tab Guard is paused. It shows what it would close, but closes nothing until you turn it back on.";
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

/** The limits for one browser (by its key), or the main limits for
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
    auto_update: Boolean(form.auto_update),
  };
}

// ---------------------------------------------------------------- several browsers

const KIND_LABEL = { tab: "Tab", gpu: "Graphics", other: "Helper" };

/** The filter to keep: the chosen browser's id, or null (All) once it has
 *  quit or only one browser is left. */
export function keptFilter(filter, browsers) {
  return browsers.length > 1 && browsers.some((b) => b.id === filter) ? filter : null;
}

/** The rows the process table shows for a filter (a browser id, or null for all). */
export const filterRows = (rows, filter) => (filter == null ? rows : rows.filter((r) => r.browser === filter));

/** The filter's buttons, as [value, label]: All, then each browser. */
export const filterChoices = (browsers) => [["all", "All"], ...browsers.map((b) => [String(b.id), shortName(b.name)])];

/** The labels after a row's pid: its kind, and which browser when several
 *  run. A main process is just named after its browser. `names` maps
 *  browser id to name. */
export function rowBadges(r, names, several) {
  const browser = shortName(names[r.browser] ?? "Browser");
  if (r.kind === "main") return [{ text: browser }];
  if (!several) return [{ text: KIND_LABEL[r.kind] }];
  return [{ text: KIND_LABEL[r.kind] }, { text: browser, browser: true }];
}

/** How the Spare and Close buttons name a tab process for screen readers. */
export function tabLabel(r, names, several) {
  return `${several ? `${names[r.browser] ?? "Browser"} ` : ""}tab process ${r.pid} (${mbOrGb(r.mem_mb)})`;
}

/** The headline: "Firefox is using…", or "Your browsers are using…". */
export function headline(s) {
  const who = s.browsers.length === 1 ? `${s.browsers[0].name} is` : "Your browsers are";
  return `${who} using ${gb(s.total_mb)} · ${s.tab_count} tab process${s.tab_count === 1 ? "" : "es"}`;
}

/** "Which site is which?": where each browser shows the site in each
 *  process. Firefox-family browsers have about:processes; Chrome-family
 *  ones have chrome://process-internals. Returns the text before the
 *  buttons, and the buttons (one with no text before it when only one
 *  browser runs). */
export function whichSite(browsers) {
  const place = (b) => (b.family === "chromium" ? "chrome://process-internals" : "about:processes");
  const aria = (b) => `Open ${place(b)} in ${b.name}`;
  if (browsers.length < 2) {
    const b = browsers[0];
    const label = `Which site is which? Open ${b ? place(b) : "about:processes"}`;
    return { intro: "", buttons: [{ id: b?.id ?? null, label, aria: label }] };
  }
  return {
    intro: "Which site is which? See the process list in",
    buttons: browsers.map((b) => ({ id: b.id, label: shortName(b.name), aria: aria(b) })),
  };
}

/** Whether the "Limits for" picker shows, and which browser it
 *  edits: it shows once there's a choice to make, or a copy has its own
 *  limits, and keeps the current choice while that copy is listed. With no
 *  picker, the main limits (null) are edited. */
export function limitChoice(targets, current) {
  const choosing = targets.length > 1 || targets.some((t) => t.own);
  if (!choosing) return { choosing, key: null, name: null };
  const t = targets.find((t) => t.key === current) ?? targets[0];
  return { choosing, key: t.key, name: t.name };
}

/** The picker's buttons, as [key, label, class]. */
export const limitChoices = (targets) =>
  targets.map((t) => [t.key, t.running ? shortName(t.name) : `${shortName(t.name)} (not running)`, t.running ? "" : "not-running"]);

/** Escape text for innerHTML. */
export const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
