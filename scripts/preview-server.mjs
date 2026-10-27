// Serves the window's UI in a normal browser with a fake Tauri backend, so
// the layout can be checked without building the app.
//   node scripts/preview-server.mjs   ->  http://localhost:5174
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const root = new URL("../src/", import.meta.url).pathname;
const port = 5174;
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

const mock = `
(() => {
  const now = () => Math.floor(Date.now() / 1000);
  const tab = (pid, mem_mb, cpu, age_secs, growth_mb_per_min = 0) =>
    ({ pid, kind: "tab", mem_mb, cpu, age_secs, growth_mb_per_min, status: "ok", reason: null, action: null });
  let config = { enabled: true, mode: "watch", warn_mb: 1024, kill_mb: 2048, total_mb: 8192, total_min_victim_mb: 400,
    interval_secs: 15, kill_cooldown_secs: 60, prefer_idle: true, idle_cpu_percent: 2, notify_on_close: false };
  const rows = [
    { ...tab(48991, 2210, 0.4, 32358, 14), status: "over", reason: "Over the 2.0 GB per-tab limit", action: "Would close" },
    { ...tab(48983, 1063, 7.7, 32359, 3), status: "warn", reason: "Past the 1.0 GB warning level" },
    tab(50214, 796, 1.9, 32350),
    { pid: 48880, kind: "gpu", mem_mb: 777, cpu: 0.1, age_secs: 32359, growth_mb_per_min: 0, status: "ok", reason: null, action: null },
    { pid: 48857, kind: "main", mem_mb: 743, cpu: 3.8, age_secs: 32360, growth_mb_per_min: 0, status: "ok", reason: null, action: null },
    tab(73899, 616, 0.6, 32172), tab(75737, 474, 0, 2011), { ...tab(48984, 442, 0.3, 32358), status: "spared", reason: "You chose to spare this tab" },
  ];
  const events = [
    { at: now() - 30, kind: "would-close", text: "Would close tab process 48991 (2.2 GB): Over the 2.0 GB per-tab limit" },
    { at: now() - 31, kind: "warn", text: "Flagged tab process 48983 (1.0 GB): Past the 1.0 GB warning level" },
    { at: now() - 600, kind: "closed", text: "Closed tab process 51001 (2.4 GB): Over the 2.0 GB per-tab limit" },
    { at: now() - 900, kind: "info", text: "Started in “Watch only” mode" },
  ];
  let autostart = true;
  const handlers = {
    get_view: () => ({ config, events, spared: [48984], log_path: "~/Library/Logs/Tab Guard.log",
      snapshot: { firefox_running: true, total_mb: 8944, tab_count: 24, flagged: 1, over: 1, rows, cooldown_secs: 0, checked_at: now() } }),
    save_config: (a) => { config = a.config; },
    get_autostart: () => autostart,
    set_autostart: (a) => (autostart = a.enabled),
    close_tab: () => {},
    set_spared: () => {},
    check_now: () => {},
    open_about_processes: () =>
      "Asked Firefox to open about:processes. If no tab appeared, press ⌘L, then ⌘V and Return: the address is on your clipboard.",
  };
  window.__TAURI__ = { core: { invoke: async (cmd, args) => { console.log("invoke", cmd, args); return handlers[cmd]?.(args); } } };
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
