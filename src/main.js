import {
  mbOrGb, duration, growth, summaryParts, shortName, bannerText, settingsToConfig, limitsFor, esc,
  keptFilter, filterRows, filterChoices, rowBadges, tabLabel, limitChoice, limitChoices, headline, whichSite,
} from "./format.js";

const { invoke } = window.__TAURI__.core;

const $ = (sel) => document.querySelector(sel);

const MODE_HELP = {
  watch: "Tab Guard closes nothing by itself. Tab processes over a limit are marked “Would close”, so you can see what it would do.",
  "warn-then-close": "Tab processes past the warning level are marked here and turn the shield orange, with no notification. Ones over a limit are closed, one at a time.",
  close: "Tab processes over a limit are closed, one at a time. Nothing is marked early.",
};

let view = null;
let lastJson = "";
let settingsDirty = false;
let confirmPid = null;
/** Which browser's processes to show (a browser id), or null for all. */
let procFilter = null;
/** Which browser's limits the settings edit (a key), or null for the main ones. */
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
 *  that leaves. */
function fitWindow() {
  if (fitted) return;
  fitted = true;
  const panels = [...document.querySelectorAll(".panel")];
  const shown = panels.find((p) => !p.hidden);
  document.body.classList.add("measuring");
  let tallest = 0;
  for (const p of panels.filter((p) => p.dataset.fit !== undefined)) {
    for (const q of panels) q.hidden = q !== p;
    tallest = Math.max(tallest, $("main").getBoundingClientRect().bottom - document.body.getBoundingClientRect().top);
  }
  for (const q of panels) q.hidden = q !== shown;
  document.body.classList.remove("measuring");
  const height = tallest + parseFloat(getComputedStyle(document.body).paddingBottom);
  invoke("fit_window", { height }).catch(() => {});
}

function render() {
  const { config, snapshot: s, events } = view;

  // Header
  if (!s.checked_at) {
    $("#headline").textContent = "Checking your browsers…";
  } else if (!s.browser_running) {
    $("#headline").textContent = "No browser is running";
    $("#subline").textContent = "Tab Guard starts watching as soon as a browser opens.";
  } else {
    $("#headline").textContent = headline(s);
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
  // Close at limit gives no early warning, so the warning level isn't used.
  $("#settings-form").warn_gb.disabled = config.mode === "close";
  $("#warn-unused").hidden = config.mode !== "close";

  const banner = $("#banner");
  const text = bannerText(config, s);
  banner.hidden = !text;
  banner.className = config.enabled ? "banner" : "banner paused";
  banner.textContent = text ?? "";

  const welcome = $("#welcome");
  if (!config.welcomed && !welcome.open) welcome.showModal();
  if (config.welcomed && welcome.open) welcome.close();
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
/** All / Firefox / Chrome, when more than one browser is running. */
function renderProcFilter(browsers) {
  const el = $("#proc-filter");
  procFilter = keptFilter(procFilter, browsers);
  el.hidden = browsers.length < 2;
  const key = JSON.stringify([procFilter, browsers.map((b) => [b.id, b.name])]);
  if (el.hidden || key === filterKey) return;
  filterKey = key;
  radios(el, filterChoices(browsers), procFilter == null ? "all" : String(procFilter), (v) => {
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
  const { choosing, key: chosen, name } = limitChoice(targets, limitTarget);
  limitTarget = chosen;
  picker.hidden = !choosing;
  for (const el of document.querySelectorAll(".limit-for")) el.textContent = choosing ? ` · ${shortName(name)}` : "";
  $("#total-label").textContent = choosing ? `When all of ${name} passes` : "When the whole browser passes";
  $("#limits-heading").textContent = choosing ? "Limits for" : "Limits";
  $("#others-heading").textContent = choosing ? "For all browsers" : "Everything else";
  const key = JSON.stringify([limitTarget, targets]);
  if (!choosing || key === targetsKey) return;
  targetsKey = key;
  radios($("#limit-targets"), limitChoices(targets), limitTarget, async (key) => {
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
/** "Which site is which?": one button, or one per browser when several run. */
function renderAboutProcesses(browsers) {
  const key = JSON.stringify(browsers.map((b) => [b.id, b.name, b.family]));
  if (key === aboutKey) return;
  aboutKey = key;
  const { intro, buttons } = whichSite(browsers);
  $("#about-processes").innerHTML = `${esc(intro)} ${buttons
    .map((b) => `<button type="button" class="link" data-browser="${b.id ?? ""}" aria-label="${esc(b.aria)}">${esc(b.label)}</button>`)
    .join(" ")}`;
}

function renderRows(config, s) {
  const tbody = $("#rows");
  const several = s.browsers.length > 1;
  const rows = filterRows(s.rows, procFilter);
  const names = Object.fromEntries(s.browsers.map((b) => [b.id, b.name]));
  const kindBadges = (r) =>
    rowBadges(r, names, several).map((b) => `<span class="kind${b.browser ? " browser" : ""}">${esc(b.text)}</span>`).join("");
  if (!rows.length) {
    tbody.innerHTML = `<tr><td colspan="7" class="empty">${s.checked_at ? "No browser processes." : "Checking…"}</td></tr>`;
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
        const what = tabLabel(r, names, several);
        const spare = document.createElement("button");
        spare.type = "button";
        spare.dataset.pid = r.pid;
        spare.dataset.action = "spare";
        // A toggle: pressed while spared, so its width never changes.
        const spared = r.status === "spared";
        spare.textContent = "Spare";
        spare.setAttribute("aria-pressed", String(spared));
        spare.setAttribute("aria-label", `Spare ${what}`);
        spare.title = spared
          ? "Spared until it exits or Tab Guard restarts. Click to stop sparing it."
          : "Never close this tab process, until it exits or Tab Guard restarts";
        spare.onclick = () => invoke("set_spared", { pid: r.pid, spared: r.status !== "spared" }).then(refresh);
        const close = document.createElement("button");
        close.type = "button";
        close.dataset.pid = r.pid;
        close.dataset.action = "close";
        close.className = "danger";
        close.textContent = confirmPid === r.pid ? "Confirm" : "Close";
        close.setAttribute("aria-label", confirmPid === r.pid ? `Confirm closing ${what}` : `Close ${what}`);
        close.title = "Close this tab process now. The browser shows its tabs as crashed, with a button to reload them.";
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
            .then(() => toast(`Closed tab process ${r.pid}. Reload it from its crashed tab when you need it.`))
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

const welcomed = () =>
  saveConfig({ ...view.config, welcomed: true, share_usage: view.usage_available && $("#welcome-usage").checked });
$("#welcome-done").addEventListener("click", welcomed);
// Escape closes the welcome too; count that as seen.
$("#welcome").addEventListener("cancel", welcomed);

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
