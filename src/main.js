const { invoke } = window.__TAURI__.core;

const $ = (sel) => document.querySelector(sel);
const gb = (mb) => `${(mb / 1024).toFixed(1)} GB`;
const mbOrGb = (mb) => (mb >= 1024 ? gb(mb) : `${mb} MB`);

const MODE_HELP = {
  watch: "Nothing is ever closed. Tabs over a limit are marked “Would close” so you can see what would happen.",
  "warn-then-close": "Tabs past the warning level are flagged; tabs over a hard limit are closed, one at a time.",
  close: "Tabs over a hard limit are closed, one at a time. No early warnings.",
};
const KIND_LABEL = { main: "Firefox", tab: "Tab", gpu: "Graphics", other: "Helper" };

let view = null;
let settingsDirty = false;
let confirmPid = null;

function duration(secs) {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  const h = Math.floor(secs / 3600);
  return h < 48 ? `${h}h ${Math.floor((secs % 3600) / 60)}m` : `${Math.floor(h / 24)}d`;
}

function timeOf(unix) {
  return new Date(unix * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" });
}

async function refresh() {
  try {
    view = await invoke("get_view");
    render();
  } catch (e) {
    $("#headline").textContent = "Couldn't reach Tab Guard";
    $("#subline").textContent = String(e);
  }
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
    const parts = [];
    if (s.over) parts.push(`${s.over} over a limit`);
    if (s.flagged) parts.push(`${s.flagged} past the warning level`);
    if (!parts.length) parts.push("Everything's within your limits");
    if (s.cooldown_secs) parts.push(`next close allowed in ${s.cooldown_secs}s`);
    $("#subline").textContent = parts.join(" · ");
  }

  $("#enabled").checked = config.enabled;
  $("#enabled-label").textContent = config.enabled ? "On" : "Paused";
  for (const b of document.querySelectorAll(".segmented button")) {
    b.setAttribute("aria-checked", String(b.dataset.mode === config.mode));
  }
  $("#mode-help").textContent = MODE_HELP[config.mode];

  const banner = $("#banner");
  if (!config.enabled) {
    banner.hidden = false;
    banner.className = "banner paused";
    banner.textContent = "Tab Guard is paused. It's still measuring, but won't flag or close anything until you turn it back on.";
  } else if (config.mode === "watch" && s.over) {
    banner.hidden = false;
    banner.className = "banner";
    banner.textContent = `Watch only: ${s.over} tab process${s.over > 1 ? "es" : ""} would be closed right now. Switch modes to let Tab Guard act.`;
  } else {
    banner.hidden = true;
  }

  $("#checked").textContent = s.checked_at ? `Checked ${timeOf(s.checked_at)}` : "";
  renderRows(config, s);
  renderEvents(events);
  if (!settingsDirty) fillSettings(config);
  $("#log-path").textContent = view.log_path;
}

function renderRows(config, s) {
  const tbody = $("#rows");
  if (!s.rows.length) {
    tbody.innerHTML = `<tr><td colspan="7" class="empty">${s.checked_at ? "No Firefox processes." : "Checking…"}</td></tr>`;
    return;
  }
  const scale = Math.max(config.kill_mb, ...s.rows.map((r) => r.mem_mb));
  tbody.replaceChildren(
    ...s.rows.map((r) => {
      const tr = document.createElement("tr");
      tr.className = r.status;
      const pill = { warn: "Warning", over: r.action || "Over limit", spared: "Spared" }[r.status];
      const growth = Math.round(r.growth_mb_per_min);
      const isTab = r.kind === "tab";
      tr.innerHTML = `
        <td><span class="pid">${r.pid}</span><span class="kind">${KIND_LABEL[r.kind]}</span></td>
        <td><div class="mem"><div class="bar"><div class="fill" style="width:${(100 * r.mem_mb) / scale}%"></div></div>
            <span class="val">${mbOrGb(r.mem_mb)}</span></div></td>
        <td class="num ${growth > 5 ? "grow-up" : ""}">${growth > 0 ? "+" : ""}${growth ? `${growth} MB/min` : "—"}</td>
        <td class="num">${r.cpu.toFixed(1)}%</td>
        <td class="num">${duration(r.age_secs)}</td>
        <td><div class="status">${pill ? `<span class="pill">${pill}</span>` : ""}
            ${r.reason ? `<span class="reason">${r.reason}</span>` : ""}</div></td>
        <td class="actions"></td>`;
      if (isTab) {
        const actions = tr.querySelector(".actions");
        const spare = document.createElement("button");
        spare.textContent = r.status === "spared" ? "Unspare" : "Spare";
        spare.title = "Never close this tab process while it's running";
        spare.onclick = () => invoke("set_spared", { pid: r.pid, spared: r.status !== "spared" }).then(refresh);
        const close = document.createElement("button");
        close.className = "danger";
        close.textContent = confirmPid === r.pid ? "Confirm" : "Close";
        close.title = "Close this tab process now. Firefox shows its tabs as crashed, with a Restore button.";
        close.onclick = () => {
          if (confirmPid !== r.pid) {
            confirmPid = r.pid;
            setTimeout(() => { if (confirmPid === r.pid) { confirmPid = null; render(); } }, 4000);
            render();
            return;
          }
          confirmPid = null;
          invoke("close_tab", { pid: r.pid }).catch((e) => alert(e)).finally(refresh);
        };
        actions.append(spare, close);
      }
      return tr;
    }),
  );
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
  form.warn_gb.value = +(c.warn_mb / 1024).toFixed(2);
  form.kill_gb.value = +(c.kill_mb / 1024).toFixed(2);
  form.total_gb.value = +(c.total_mb / 1024).toFixed(2);
  form.total_min_victim_mb.value = c.total_min_victim_mb;
  form.prefer_idle.checked = c.prefer_idle;
  form.idle_cpu_percent.value = c.idle_cpu_percent;
  form.interval_secs.value = c.interval_secs;
  form.kill_cooldown_secs.value = c.kill_cooldown_secs;
  form.notify_on_close.checked = c.notify_on_close;
}

function readSettings() {
  const num = (el, fallback) => (Number.isFinite(+el.value) && el.value !== "" ? +el.value : fallback);
  const c = view.config;
  return {
    ...c,
    warn_mb: Math.round(num(form.warn_gb, c.warn_mb / 1024) * 1024),
    kill_mb: Math.round(num(form.kill_gb, c.kill_mb / 1024) * 1024),
    total_mb: Math.round(num(form.total_gb, c.total_mb / 1024) * 1024),
    total_min_victim_mb: Math.round(num(form.total_min_victim_mb, c.total_min_victim_mb)),
    prefer_idle: form.prefer_idle.checked,
    idle_cpu_percent: num(form.idle_cpu_percent, c.idle_cpu_percent),
    interval_secs: Math.round(num(form.interval_secs, c.interval_secs)),
    kill_cooldown_secs: Math.round(num(form.kill_cooldown_secs, c.kill_cooldown_secs)),
    notify_on_close: form.notify_on_close.checked,
  };
}

async function saveConfig(config) {
  await invoke("save_config", { config });
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
    alert(err);
    return !e.target.checked;
  });
});

// ---------------------------------------------------------------- controls

$("#enabled").addEventListener("change", (e) => saveConfig({ ...view.config, enabled: e.target.checked }));

for (const b of document.querySelectorAll(".segmented button")) {
  b.addEventListener("click", () => saveConfig({ ...view.config, mode: b.dataset.mode }));
}

for (const t of document.querySelectorAll(".tabs button")) {
  t.addEventListener("click", () => {
    for (const other of document.querySelectorAll(".tabs button")) {
      other.setAttribute("aria-selected", String(other === t));
      $(`#${other.dataset.panel}`).hidden = other !== t;
    }
  });
}

$("#check-now").addEventListener("click", () => invoke("check_now").then(() => setTimeout(refresh, 600)));
$("#about-processes").addEventListener("click", () => invoke("open_about_processes").catch((e) => alert(e)));

invoke("get_autostart").then((on) => ($("#autostart").checked = on));
refresh();
setInterval(() => {
  if (document.visibilityState === "visible") refresh();
}, 2000);
