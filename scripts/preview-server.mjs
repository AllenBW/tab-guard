// Serves the window's UI in a normal browser with a fake Tauri backend, so
// the layout can be checked without building the app.
//   node scripts/preview-server.mjs   ->  http://localhost:5174
// Every command the page sends is kept in window.__calls, for the browser
// tests in tests/ui.
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const root = new URL("../src/", import.meta.url).pathname;
const port = Number(process.env.PORT) || 5174;
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

const mock = `
(() => {
  const now = () => Math.floor(Date.now() / 1000);
  // ?browsers=2 shows Firefox and Nightly side by side, ?browsers=mixed
  // Firefox and Chrome, and ?browsers=chrome Chrome alone.
  const mode = new URLSearchParams(location.search).get("browsers");
  const several = mode === "2" || mode === "mixed";
  const first = mode === "chrome"
    ? { id: 48857, name: "Google Chrome", family: "chromium", key: "/Applications/Google Chrome.app" }
    : { id: 48857, name: "Firefox", family: "gecko", key: "/Applications/Firefox.app" };
  const second = mode === "mixed"
    ? { id: 90001, name: "Google Chrome", family: "chromium", key: "/Applications/Google Chrome.app" }
    : { id: 90001, name: "Firefox Nightly", family: "gecko", key: "/Applications/Firefox Nightly.app" };
  const tab = (pid, mem_mb, cpu, age_secs, growth_mb_per_min = 0, browser = 48857) =>
    ({ pid, browser, kind: "tab", mem_mb, cpu, age_secs, growth_mb_per_min, status: "ok", reason: null, action: null });
  let config = { enabled: true, mode: "watch", warn_mb: 1024, kill_mb: 2048, total_mb: 8192, total_min_victim_mb: 400,
    total_backoff_mins: 15, total_max_per_hour: 3, share_usage: false, auto_update: true, welcomed: false,
    interval_secs: 15, kill_cooldown_secs: 60, prefer_idle: true, idle_cpu_percent: 2, notify_on_close: false,
    browser_limits: {} };
  const rows = [
    { ...tab(48991, 2210, 0.4, 32358, 14), status: "over", reason: "Over the 2.0 GB per-tab limit", action: "Would close" },
    { ...tab(48983, 1063, 7.7, 32359, 3), status: "warn", reason: "Past the 1.0 GB warning level" },
    tab(50214, 796, 1.9, 32350),
    { pid: 48880, browser: 48857, kind: "gpu", mem_mb: 777, cpu: 0.1, age_secs: 32359, growth_mb_per_min: 0, status: "ok", reason: null, action: null },
    { pid: 48857, browser: 48857, kind: "main", mem_mb: 743, cpu: 3.8, age_secs: 32360, growth_mb_per_min: 0, status: "ok", reason: null, action: null },
    tab(73899, 616, 0.6, 32172, 0, several ? 90001 : 48857), tab(75737, 474, 0, 2011, 0, several ? 90001 : 48857), { ...tab(48984, 442, 0.3, 32358), status: "spared", reason: "You chose to spare this tab" },
  ];
  const events = [
    { at: now() - 30, kind: "would-close", text: "Would close tab process 48991 (2.2 GB): Over the 2.0 GB per-tab limit" },
    { at: now() - 31, kind: "warn", text: "Flagged tab process 48983 (1.0 GB): Past the 1.0 GB warning level" },
    { at: now() - 600, kind: "closed", text: "Closed tab process 51001 (2.4 GB): Over the 2.0 GB per-tab limit" },
    { at: now() - 900, kind: "info", text: "Started in “Watch only” mode" },
  ];
  let autostart = true;
  // Like the app, the latest update check's result is kept and shown on every refresh.
  let updateStatus = "Tab Guard 1.0.0 is the latest version (checked 09:15)";
  const handlers = {
    get_view: () => ({ config, events, version: "1.0.0", usage_available: true,
      limit_targets: (several ? [first, second] : [first])
        .map(({ key, name }) => ({ key, name, running: true, own: key in config.browser_limits })),
      update_status: updateStatus, spared: [48984], log_path: "~/Library/Logs/Tab Guard.log",
      snapshot: { browser_running: true, total_mb: 8944, tab_count: 24, flagged: 1, over: 1, rows, cooldown_secs: 0, checked_at: now(),
        browsers: several
          ? [{ ...first, total_mb: 7854, tab_count: 22 }, { ...second, total_mb: 1090, tab_count: 2 }]
          : [{ ...first, total_mb: 8944, tab_count: 24 }],
        total_hold: config.mode === "watch" ? null
          : "Whole-browser limit paused until 08:31: closing a 0.9 GB tab didn't bring Firefox down (8.7 GB before, 9.7 GB now)" } }),
    save_config: (a) => { config = a.config; },
    get_autostart: () => autostart,
    set_autostart: (a) => (autostart = a.enabled),
    close_tab: () => {},
    set_spared: () => {},
    open_link: () => {},
    check_for_updates: () => (updateStatus = "Tab Guard 1.0.0 is the latest version (checked 09:16)"),
    fit_window: () => {},
    open_about_processes: () =>
      "Asked Firefox to open about:processes. If no tab appeared, press ⌘L, then ⌘V and Return: the address is on your clipboard.",
  };
  window.__calls = [];
  window.__TAURI__ = { core: { invoke: async (cmd, args) => {
    window.__calls.push([cmd, args ?? null]);
    return handlers[cmd]?.(args);
  } } };
})();`;

createServer(async (req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^\/+/, "") || "index.html";
  try {
    let body = await readFile(join(root, path));
    if (path === "index.html") body = body.toString().replace("<script", `<script>${mock}</script><script`);
    res.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" }).end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
}).listen(port, () => console.log(`UI preview at http://localhost:${port}`));
