import { gb, mbOrGb, duration, growth, summaryParts, shortName, bannerText, settingsToConfig, limitsFor, esc } from "./format.js";

const { invoke } = window.__TAURI__.core;

const $ = (sel) => document.querySelector(sel);

const MODE_HELP = {
  watch: "Nothing is ever closed. Tabs over a limit are marked “Would close” so you can see what would happen.",
  "warn-then-close": "Tabs past the warning level are flagged; tabs over a hard limit are closed, one at a time.",
  close: "Tabs over a hard limit are closed, one at a time. No early warnings.",
};
const KIND_LABEL = { main: "Firefox", tab: "Tab", gpu: "Graphics", other: "Helper" };

let view = null;
let lastJson = "";
let settingsDirty = false;
let confirmPid = null;
/** Which Firefox's processes to show (a browser id), or null for all. */
let procFilter = null;
/** Which Firefox's limits the settings edit (a key), or null for the main ones. */
let limitTarget = null;

let toastTimer;
/** A short message under the header. The app's web view doesn't show
 *  window.alert, so errors went unseen before. */
function toast(text, isError = false) {
  const el = $("#toast");
  el.textContent = text;
  el.className = isError ? "toast error" : "toast";
  el.setAttribute("role", isError ? "alert" : "status");
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 9000);
}

/** Say something to screen readers without showing it. */
function announce(text) {
  const el = $("#sr-status");
  el.textContent = "";
  setTimeout(() => (el.textContent = text), 50);
}

function timeOf(unix) {
  return new Date(unix * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" });
}

async function refresh() {
  try {
    const next = await invoke("get_view");
    // Skip the redraw when nothing changed since the last poll.
    const json = JSON.stringify(next);
    if (json === lastJson) return;
    lastJson = json;
    view = next;
    render();
    fitWindow();
  } catch (e) {
    $("#headline").textContent = "Couldn't reach Tab Guard";
    $("#subline").textContent = String(e);
  }
}

let fitted = false;
/** Once, after the first render: size the window so Settings and About fit
 *  without scrolling. The process list and the activity log can be any
 *  length, so they scroll inside their own boxes and fill whatever room
 *  that leaves. The welcome card
 *  is left out too, since it goes away after the first run. */
function fitWindow() {
  if (fitted) return;
  fitted = true;
  const panels = [...document.querySelectorAll(".panel")];
  const shown = panels.find((p) => !p.hidden);
  const welcome = $("#welcome");
  const welcomeHidden = welcome.hidden;
  welcome.hidden = true;
  document.body.classList.add("measuring");
  let tallest = 0;
  for (const p of panels.filter((p) => p.dataset.fit !== undefined)) {
    for (const q of panels) q.hidden = q !== p;
    tallest = Math.max(tallest, $("main").getBoundingClientRect().bottom - document.body.getBoundingClientRect().top);
  }
  for (const q of panels) q.hidden = q !== shown;
  document.body.classList.remove("measuring");
  welcome.hidden = welcomeHidden;
  const height = tallest + parseFloat(getComputedStyle(document.body).paddingBottom);
  invoke("fit_window", { height }).catch(() => {});
}

function render() {
  const { config, snapshot: s, events } = view;

  // Header
  if (!s.checked_at) {
    $("#headline").textContent = "Checking Firefox…";
  } else if (!s.firefox_running) {
    $("#headline").textContent = "Firefox isn't running";
    $("#subline").textContent = "Tab Guard will start watching as soon as it opens.";
  } else {
    $("#headline").textContent = `Firefox is using ${gb(s.total_mb)} across ${s.tab_count} tab processes`;
    $("#subline").textContent = summaryParts(s);
  }

  $("#enabled").checked = config.enabled;
  $("#enabled-label").textContent = config.enabled ? "On" : "Paused";
  for (const b of document.querySelectorAll(".segmented button")) {
    const on = b.dataset.mode === config.mode;
    b.setAttribute("aria-checked", String(on));
    b.tabIndex = on ? 0 : -1;
  }
  $("#mode-help").textContent = MODE_HELP[config.mode];

  const banner = $("#banner");
  const text = bannerText(config, s);
  banner.hidden = !text;
  banner.className = config.enabled ? "banner" : "banner paused";
  banner.textContent = text ?? "";

  $("#welcome").hidden = config.welcomed;
  for (const el of document.querySelectorAll(".usage-row")) el.hidden = !view.usage_available;
  $("#version").textContent = `Version ${view.version}`;
  $("#update-status").textContent = view.update_status;

  $("#checked").textContent = s.checked_at ? `Checked ${timeOf(s.checked_at)}` : "";
  renderAboutProcesses(s.browsers);
  renderProcFilter(s.browsers);
  renderRows(config, s);
  renderEvents(events);
  renderLimitTargets(view.limit_targets);
  if (!settingsDirty) fillSettings(config);
  $("#log-path").textContent = view.log_path;
}

/** A radio group of small buttons; `choices` are [value, label, extra class]. */
function radios(el, choices, selected, choose) {
  el.innerHTML = choices
    .map(
      ([value, label, cls]) =>
        `<button type="button" role="radio" class="${cls ?? ""}" data-value="${esc(value)}"
          aria-checked="${value === selected}" tabindex="${value === selected ? 0 : -1}">${esc(label)}</button>`,
    )
    .join("");
  el.onclick = (e) => {
    const b = e.target.closest("button");
    if (b) choose(b.dataset.value);
  };
}

let filterKey = null;
/** All / Firefox / Developer Edition, when more than one Firefox is running. */
function renderProcFilter(browsers) {
  const el = $("#proc-filter");
  if (procFilter != null && !browsers.some((b) => b.id === procFilter)) procFilter = null;
  el.hidden = browsers.length < 2;
  const key = JSON.stringify([procFilter, browsers.map((b) => [b.id, b.name])]);
  if (el.hidden || key === filterKey) return;
  filterKey = key;
  const choices = [["all", "All"], ...browsers.map((b) => [String(b.id), shortName(b.name)])];
  radios(el, choices, procFilter == null ? "all" : String(procFilter), (v) => {
    procFilter = v === "all" ? null : Number(v);
    filterKey = null;
    render();
    $("#proc-filter [aria-checked=true]").focus();
  });
}

let targetsKey = null;
/** "Limits for": shown once there's a choice to make, or a copy has its own limits. */
function renderLimitTargets(targets) {
  const picker = $("#limit-picker");
  const choosing = targets.length > 1 || targets.some((t) => t.own);
  if (!choosing) limitTarget = null;
  else if (!targets.some((t) => t.key === limitTarget)) limitTarget = targets[0].key;
  picker.hidden = !choosing;
  const name = targets.find((t) => t.key === limitTarget)?.name;
  for (const el of document.querySelectorAll(".limit-for")) el.textContent = choosing ? ` · ${shortName(name)}` : "";
  $("#total-label").textContent = `When all of ${choosing ? name : "Firefox"} passes`;
  const key = JSON.stringify([limitTarget, targets]);
  if (!choosing || key === targetsKey) return;
  targetsKey = key;
  const choices = targets.map((t) => [t.key, t.running ? shortName(t.name) : `${shortName(t.name)} (not running)`, t.running ? "" : "not-running"]);
  radios($("#limit-targets"), choices, limitTarget, async (key) => {
    if (key === limitTarget) return;
    // Save what was typed for the copy being left before switching.
    if (settingsDirty) {
      clearTimeout(saveTimer);
      await saveConfig(readSettings());
    }
    limitTarget = key;
    targetsKey = null;
    settingsDirty = false;
    render();
    $("#limit-targets [aria-checked=true]").focus();
  });
}

let aboutKey = null;
/** "Which site is which?": one button, or one per Firefox when several run. */
function renderAboutProcesses(browsers) {
  const key = JSON.stringify(browsers.map((b) => [b.id, b.name]));
  if (key === aboutKey) return;
  aboutKey = key;
  const el = $("#about-processes");
  const button = (id, label, aria) =>
    `<button type="button" class="link" data-browser="${id ?? ""}" aria-label="${esc(aria)}">${esc(label)}</button>`;
  el.innerHTML =
    browsers.length > 1
      ? `Which site is which? Open about:processes in ${browsers
          .map((b) => button(b.id, shortName(b.name), `Open about:processes in ${b.name}`))
          .join(" ")}`
      : button(browsers[0]?.id, "Which site is which? Open about:processes", "Which site is which? Open about:processes");
}

function renderRows(config, s) {
  const tbody = $("#rows");
  const several = s.browsers.length > 1;
  const rows = procFilter == null ? s.rows : s.rows.filter((r) => r.browser === procFilter);
  const names = Object.fromEntries(s.browsers.map((b) => [b.id, b.name]));
  // With several Firefoxes, each row also says which one; a main process
  // is just named after its Firefox.
  const kindBadges = (r) => {
    const browser = shortName(names[r.browser] ?? "Firefox");
    if (!several) return `<span class="kind">${KIND_LABEL[r.kind]}</span>`;
    if (r.kind === "main") return `<span class="kind">${esc(browser)}</span>`;
    return `<span class="kind">${KIND_LABEL[r.kind]}</span><span class="kind browser">${esc(browser)}</span>`;
  };
  if (!rows.length) {
    tbody.innerHTML = `<tr><td colspan="7" class="empty">${s.checked_at ? "No Firefox processes." : "Checking…"}</td></tr>`;
    return;
  }
  const scale = Math.max(limitsFor(config, null).kill_mb, ...rows.map((r) => r.mem_mb));
  // The table is redrawn every few seconds; put keyboard focus back where it was.
  const focused = document.activeElement?.closest?.("#rows") ? document.activeElement.dataset : null;
  tbody.replaceChildren(
    ...rows.map((r) => {
      const tr = document.createElement("tr");
      tr.className = r.status;
      const pill = { warn: "Warning", over: r.action || "Over limit", spared: "Spared" }[r.status];
      const isTab = r.kind === "tab";
      tr.innerHTML = `
        <td class="proc"><span class="pid">${r.pid}</span>${kindBadges(r)}</td>
        <td><div class="mem"><div class="bar" aria-hidden="true"><div class="fill" style="width:${(100 * r.mem_mb) / scale}%"></div></div>
            <span class="val">${mbOrGb(r.mem_mb)}</span></div></td>
        <td class="num ${r.growth_mb_per_min > 5 ? "grow-up" : ""}">${growth(r.growth_mb_per_min)}</td>
        <td class="num">${r.cpu.toFixed(1)}%</td>
        <td class="num age-col">${duration(r.age_secs)}</td>
        <td><div class="status">${pill ? `<span class="pill">${esc(pill)}</span>` : ""}
            ${r.reason ? `<span class="reason">${esc(r.reason)}</span>` : ""}</div></td>
        <td class="actions"></td>`;
      if (isTab) {
        const actions = tr.querySelector(".actions");
        const what = `${several ? `${names[r.browser] ?? "Firefox"} ` : ""}tab process ${r.pid} (${mbOrGb(r.mem_mb)})`;
        const spare = document.createElement("button");
        spare.type = "button";
        spare.dataset.pid = r.pid;
        spare.dataset.action = "spare";
        spare.textContent = r.status === "spared" ? "Unspare" : "Spare";
        spare.setAttribute("aria-label", `${spare.textContent} ${what}`);
        spare.title = "Never close this tab process while it's running";
        spare.onclick = () => invoke("set_spared", { pid: r.pid, spared: r.status !== "spared" }).then(refresh);
        const close = document.createElement("button");
        close.type = "button";
        close.dataset.pid = r.pid;
        close.dataset.action = "close";
        close.className = "danger";
        close.textContent = confirmPid === r.pid ? "Confirm" : "Close";
        close.setAttribute("aria-label", confirmPid === r.pid ? `Confirm closing ${what}` : `Close ${what}`);
        close.title = "Close this tab process now. Firefox shows its tabs as crashed, with a Restore button.";
        close.onclick = () => {
          if (confirmPid !== r.pid) {
            confirmPid = r.pid;
            announce(`Press again within 4 seconds to close ${what}.`);
            setTimeout(() => { if (confirmPid === r.pid) { confirmPid = null; render(); } }, 4000);
            render();
            return;
          }
          confirmPid = null;
          invoke("close_tab", { pid: r.pid })
            .then(() => toast(`Closed tab process ${r.pid}. Click Restore on its crashed tab in Firefox to reload it.`))
            .catch((e) => toast(String(e), true))
            .finally(refresh);
        };
        actions.append(spare, close);
      }
      return tr;
    }),
  );
  if (focused?.pid) tbody.querySelector(`[data-pid="${focused.pid}"][data-action="${focused.action}"]`)?.focus();
}

function renderEvents(events) {
  const closed = events.filter((e) => e.kind === "closed").length;
  $("#activity-count").textContent = closed ? `(${closed} closed)` : "";
  const ul = $("#events");
  if (!events.length) {
    ul.innerHTML = `<li class="muted">Nothing yet.</li>`;
    return;
  }
  ul.replaceChildren(
    ...events.map((e) => {
      const li = document.createElement("li");
      const t = document.createElement("time");
      t.textContent = timeOf(e.at);
      const span = document.createElement("span");
      span.className = e.kind;
      span.textContent = e.text;
      li.append(t, span);
      return li;
    }),
  );
}

// ---------------------------------------------------------------- settings

const form = $("#settings-form");

function fillSettings(c) {
  const l = limitsFor(c, limitTarget);
  form.warn_gb.value = +(l.warn_mb / 1024).toFixed(2);
  form.kill_gb.value = +(l.kill_mb / 1024).toFixed(2);
  form.total_gb.value = +(l.total_mb / 1024).toFixed(2);
  form.total_min_victim_mb.value = l.total_min_victim_mb;
  form.total_backoff_mins.value = c.total_backoff_mins;
  form.total_max_per_hour.value = c.total_max_per_hour;
  form.prefer_idle.checked = c.prefer_idle;
  form.idle_cpu_percent.value = c.idle_cpu_percent;
  form.interval_secs.value = c.interval_secs;
  form.kill_cooldown_secs.value = c.kill_cooldown_secs;
  form.notify_on_close.checked = c.notify_on_close;
  form.share_usage.checked = c.share_usage;
  form.auto_update.checked = c.auto_update;
}

function readSettings() {
  const value = (name) => (form[name].type === "checkbox" ? form[name].checked : form[name].value);
  const names = ["warn_gb", "kill_gb", "total_gb", "total_min_victim_mb", "total_backoff_mins", "total_max_per_hour",
    "prefer_idle", "idle_cpu_percent", "interval_secs", "kill_cooldown_secs", "notify_on_close", "share_usage",
    "auto_update"];
  return settingsToConfig(view.config, Object.fromEntries(names.map((n) => [n, value(n)])), limitTarget);
}

async function saveConfig(config) {
  try {
    await invoke("save_config", { config });
  } catch (e) {
    toast(`Couldn't save: ${e}`, true);
    return;
  }
  settingsDirty = false;
  await refresh();
  $("#saved").textContent = `Saved ${timeOf(Date.now() / 1000)}`;
}

let saveTimer;
form.addEventListener("input", (e) => {
  if (e.target.id === "autostart") return;
  settingsDirty = true;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => saveConfig(readSettings()), 600);
});

$("#autostart").addEventListener("change", async (e) => {
  e.target.checked = await invoke("set_autostart", { enabled: e.target.checked }).catch((err) => {
    toast(`Couldn't change login startup: ${err}`, true);
    return !e.target.checked;
  });
});

// ---------------------------------------------------------------- controls

$("#enabled").addEventListener("change", (e) => saveConfig({ ...view.config, enabled: e.target.checked }));

for (const b of document.querySelectorAll(".segmented button")) {
  b.addEventListener("click", () => saveConfig({ ...view.config, mode: b.dataset.mode }));
}

/** Arrow keys, Home and End move between the items of a tab list or radio group. */
function arrowKeys(container, items, choose) {
  container.addEventListener("keydown", (e) => {
    const list = [...container.querySelectorAll(items)];
    const i = list.indexOf(document.activeElement);
    if (i < 0) return;
    const next = { ArrowRight: i + 1, ArrowDown: i + 1, ArrowLeft: i - 1, ArrowUp: i - 1, Home: 0, End: list.length - 1 }[e.key];
    if (next === undefined) return;
    e.preventDefault();
    const target = list[(next + list.length) % list.length];
    target.focus();
    choose(target);
  });
}
arrowKeys($(".mode .segmented"), "button", (b) => b.click());
arrowKeys($("#proc-filter"), "button", (b) => b.click());
arrowKeys($("#limit-targets"), "button", (b) => b.click());
arrowKeys($(".tabs"), "button", (t) => showPanel(t.dataset.panel));

/** Switch tabs. Also called by the app when the menu bar's About item is used. */
function showPanel(name) {
  for (const t of document.querySelectorAll(".tabs button")) {
    t.setAttribute("aria-selected", String(t.dataset.panel === name));
    t.tabIndex = t.dataset.panel === name ? 0 : -1;
    $(`#${t.dataset.panel}`).hidden = t.dataset.panel !== name;
  }
}
window.showPanel = showPanel;

for (const t of document.querySelectorAll(".tabs button")) {
  t.addEventListener("click", () => showPanel(t.dataset.panel));
}
showPanel(location.hash.length > 1 && $(location.hash)?.classList.contains("panel") ? location.hash.slice(1) : "processes");

for (const a of document.querySelectorAll("[data-link]")) {
  a.addEventListener("click", () => invoke("open_link", { name: a.dataset.link }).catch((e) => toast(String(e), true)));
}

$("#welcome-done").addEventListener("click", () =>
  saveConfig({ ...view.config, welcomed: true, share_usage: view.usage_available && $("#welcome-usage").checked }),
);

$("#check-updates").addEventListener("click", async (e) => {
  e.target.disabled = true;
  $("#update-status").textContent = "Checking…";
  let text;
  try {
    text = await invoke("check_for_updates");
  } catch (err) {
    text = String(err);
  }
  // The result shows under the button, where the user is looking.
  $("#update-status").textContent = text;
  announce(text);
  e.target.disabled = false;
  refresh();
});

$("#about-processes").addEventListener("click", async (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  b.disabled = true;
  try {
    const browser = b.dataset.browser === "" ? null : Number(b.dataset.browser);
    toast(await invoke("open_about_processes", { browser }));
  } catch (err) {
    toast(String(err), true);
  } finally {
    b.disabled = false;
  }
});

invoke("get_autostart").then((on) => ($("#autostart").checked = on));
refresh();
setInterval(() => {
  if (document.visibilityState === "visible") refresh();
}, 2000);
