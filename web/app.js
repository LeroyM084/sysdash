"use strict";

// Toutes les données serveur passent par textContent : jamais d'innerHTML.

const GAUGE_LEN = 2 * Math.PI * 52; // circonférence des cercles r=52
const HISTORY = 60; // points gardés pour les sparklines (mémoire JS uniquement)
const POLL_MS = 2000;
const RECONNECT_MS = 3000;
const UNITS = ["o", "Ko", "Mo", "Go", "To", "Po"];

const $ = (id) => document.getElementById(id);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// --- Formatage ---------------------------------------------------------------

const fmt1 = (v) => v.toFixed(1).replace(".", ",");

/** Tailles binaires (Kio, Mio…) affichées « Ko, Mo… », 1 décimale. */
function fmtBytes(n) {
  if (n < 1024) return `${Math.round(n)} o`;
  let i = 0;
  while (n >= 1024 && i < UNITS.length - 1) {
    n /= 1024;
    i++;
  }
  return `${fmt1(n)} ${UNITS[i]}`;
}

const fmtRate = (n) => `${fmtBytes(n)}/s`;

function fmtUptime(s) {
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return `${d} j ${h} h ${m} min`;
  if (h) return `${h} h ${m} min`;
  return `${m} min`;
}

const plural = (n, word) => `${n} ${word}${n > 1 ? "s" : ""}`;
const percentOf = (used, total) => (total > 0 ? (used / total) * 100 : 0);
const clamp = (p) => Math.min(100, Math.max(0, p));
const level = (p) => (p >= 90 ? "crit" : p >= 80 ? "warn" : "");

// --- Composants --------------------------------------------------------------

function setGauge(gauge, p) {
  const len = (GAUGE_LEN * clamp(p)) / 100;
  gauge.querySelector(".value").style.strokeDasharray = `${len.toFixed(2)} ${GAUGE_LEN.toFixed(2)}`;
  gauge.dataset.level = level(p);
}

function setBar(bar, p) {
  bar.firstElementChild.style.width = `${clamp(p)}%`;
  bar.dataset.level = level(p);
}

function makeBar() {
  const bar = el("div", "bar");
  bar.append(el("div", "bar-fill"));
  return bar;
}

function renderCores(cores) {
  const box = $("cores");
  if (box.children.length !== cores.length) {
    box.replaceChildren(
      ...cores.map(() => {
        const core = el("div", "core");
        core.append(el("div", "core-fill"));
        return core;
      }),
    );
    $("cores-caption").textContent = plural(cores.length, "cœur");
  }
  cores.forEach((p, i) => {
    const core = box.children[i];
    core.firstElementChild.style.height = `${clamp(p)}%`;
    core.dataset.level = level(p);
    core.title = `Cœur ${i} : ${fmt1(p)} %`;
  });
}

let diskKey = null;

function renderDisks(disks) {
  const box = $("disks");
  const key = disks.map((d) => `${d.device}\n${d.mount}`).join("\n");
  if (key !== diskKey) {
    diskKey = key;
    if (!disks.length) {
      box.replaceChildren(el("p", "empty", "Aucun disque détecté."));
      return;
    }
    box.replaceChildren(
      ...disks.map((d) => {
        const row = el("div", "disk");
        const name = el("div", "disk-name");
        name.append(el("span", "disk-mount", d.mount), el("span", "disk-fs", d.fs));
        name.title = `${d.device} · ${d.fs}`;
        row.append(name, makeBar(), el("span", "disk-size num"), el("span", "disk-pct num"));
        return row;
      }),
    );
  }
  disks.forEach((d, i) => {
    const row = box.children[i];
    const p = percentOf(d.used, d.total);
    setBar(row.querySelector(".bar"), p);
    row.querySelector(".disk-size").textContent = `${fmtBytes(d.used)} / ${fmtBytes(d.total)}`;
    const pct = row.querySelector(".disk-pct");
    pct.textContent = `${fmt1(p)} %`;
    pct.dataset.level = level(p);
  });
}

const history = { rx: [], tx: [] };

function drawSpark(svg, data) {
  const line = svg.querySelector(".line");
  const area = svg.querySelector(".area");
  if (data.length < 2) {
    line.removeAttribute("d");
    area.removeAttribute("d");
    return;
  }
  const w = HISTORY - 1;
  const h = 40;
  const max = Math.max(1024, ...data); // plancher 1 Ko/s : pas de bruit amplifié au repos
  const x0 = w - (data.length - 1);
  const pts = data.map((v, i) => `${x0 + i},${(h - 1 - (v / max) * (h - 4)).toFixed(2)}`);
  const d = `M${pts.join("L")}`;
  line.setAttribute("d", d);
  area.setAttribute("d", `${d}L${w},${h}L${x0},${h}Z`);
}

function pushNet(key, value) {
  const data = history[key];
  data.push(value);
  if (data.length > HISTORY) data.shift();
  $(key).textContent = fmtRate(value);
  drawSpark($(`${key}-spark`), data);
}

// --- Flux temps réel ---------------------------------------------------------

let hostname = "";

function render(s) {
  if (s.hostname !== hostname) {
    hostname = s.hostname;
    $("hostname").textContent = hostname;
    document.title = `${hostname} · sysdash`;
  }
  $("uptime").textContent = fmtUptime(s.uptime_s);

  setGauge($("cpu-gauge"), s.cpu.usage);
  $("cpu-pct").textContent = fmt1(s.cpu.usage);
  renderCores(s.cpu.cores);

  const mem = percentOf(s.mem.used, s.mem.total);
  setGauge($("mem-gauge"), mem);
  $("mem-pct").textContent = fmt1(mem);
  $("mem-text").textContent = `${fmtBytes(s.mem.used)} / ${fmtBytes(s.mem.total)}`;
  if (s.mem.swap_total > 0) {
    setBar($("swap-bar"), percentOf(s.mem.swap_used, s.mem.swap_total));
    $("swap-text").textContent = `${fmtBytes(s.mem.swap_used)} / ${fmtBytes(s.mem.swap_total)}`;
  } else {
    setBar($("swap-bar"), 0);
    $("swap-text").textContent = "aucun";
  }

  renderDisks(s.disks);
  pushNet("rx", s.net.rx_bps);
  pushNet("tx", s.net.tx_bps);
}

function setConnected(ok) {
  const dot = $("conn");
  dot.classList.toggle("ok", ok);
  dot.title = ok ? "Connecté" : "Déconnecté";
  dot.setAttribute("aria-label", dot.title);
  document.body.classList.toggle("offline", !ok);
}

let source = null;
let reconnectTimer = 0;

function connect() {
  clearTimeout(reconnectTimer);
  source = new EventSource("/api/stream");
  source.onopen = () => setConnected(true);
  source.onmessage = (e) => {
    setConnected(true);
    try {
      render(JSON.parse(e.data));
    } catch (err) {
      console.error("sysdash: snapshot invalide", err);
    }
  };
  source.onerror = () => {
    setConnected(false);
    // Le navigateur se reconnecte seul, sauf s'il a abandonné (ex. 502 du proxy).
    if (source.readyState === EventSource.CLOSED) {
      source.close();
      reconnectTimer = setTimeout(connect, RECONNECT_MS);
    }
  };
}

// --- Mises à jour ------------------------------------------------------------

const upd = {
  count: 0,
  running: false,
  seenRunning: false,
  polls: 0,
  timer: 0,
};

async function api(path, options) {
  const res = await fetch(path, { cache: "no-store", ...options });
  let body = null;
  try {
    body = await res.json();
  } catch {
    // 202 sans corps
  }
  return { status: res.status, ok: res.ok, body };
}

const errorText = (status, body) => (body && body.error) || `HTTP ${status}`;

function setUpdState(text, tone) {
  const node = $("upd-state");
  node.hidden = !text;
  node.textContent = text || "";
  node.dataset.tone = tone || "";
}

function refreshApplyButton() {
  const btn = $("upd-apply");
  btn.disabled = upd.running || upd.count === 0;
  btn.textContent = upd.running ? "Mise à jour en cours…" : "Mettre à jour";
}

function renderUpdates(u) {
  upd.count = u.count;

  const badge = $("upd-count");
  badge.textContent = String(u.count);
  badge.classList.toggle("pending", u.count > 0);
  $("upd-reboot").hidden = !u.reboot_required;

  $("upd-summary").textContent =
    u.count > 0 ? `${plural(u.count, "paquet")} à mettre à jour` : "Le système est à jour.";
  const checked = new Date(u.checked_at * 1000);
  $("upd-checked").textContent = `Vérifié à ${checked.toLocaleTimeString("fr-FR", {
    hour: "2-digit",
    minute: "2-digit",
  })}`;

  $("upd-list").replaceChildren(
    ...u.packages.map((p) => {
      const li = el("li");
      const versions = el("span", "pkg-versions");
      versions.append(`${p.current} → `, el("span", "to", p.candidate));
      li.append(el("span", "pkg-name", p.name), versions);
      return li;
    }),
  );
  $("upd-details").hidden = u.count === 0;
  refreshApplyButton();
}

async function loadUpdates() {
  try {
    const { ok, status, body } = await api("/api/updates");
    if (!ok) throw new Error(errorText(status, body));
    renderUpdates(body);
  } catch (err) {
    $("upd-summary").textContent = "Liste indisponible.";
    setUpdState(`Impossible de lire les mises à jour : ${err.message}`, "err");
  }
}

function renderLog(lines) {
  const log = $("upd-log");
  log.hidden = false;
  log.textContent = lines.join("\n");
  log.scrollTop = log.scrollHeight;
}

function schedulePoll() {
  clearTimeout(upd.timer);
  upd.timer = setTimeout(pollStatus, POLL_MS);
}

function startPolling(alreadyRunning) {
  upd.running = true;
  upd.seenRunning = alreadyRunning;
  upd.polls = 0;
  refreshApplyButton();
  setUpdState("Mise à jour en cours…", "run");
  pollStatus();
}

async function pollStatus() {
  let st;
  try {
    const { ok, status, body } = await api("/api/updates/status");
    if (!ok) throw new Error(errorText(status, body));
    st = body;
  } catch (err) {
    // Serveur momentanément injoignable : on réessaie au prochain tour.
    setUpdState(`Statut indisponible (${err.message}), nouvel essai…`, "err");
    schedulePoll();
    return;
  }

  renderLog(st.log);
  upd.polls++;
  if (st.state === "running") {
    upd.seenRunning = true;
    setUpdState("Mise à jour en cours…", "run");
    schedulePoll();
    return;
  }
  // Juste après le lancement, l'unité peut encore afficher l'état du run précédent.
  if (!upd.seenRunning && upd.polls < 5) {
    schedulePoll();
    return;
  }

  upd.running = false;
  if (st.state === "failed") {
    setUpdState("La mise à jour a échoué. Voir le journal ci-dessous.", "err");
  } else {
    setUpdState("Mise à jour terminée.", "ok");
  }
  await loadUpdates();
}

function showConfirm(show) {
  $("upd-apply").hidden = show;
  $("upd-confirm").hidden = !show;
  if (show) {
    $("upd-confirm-text").textContent = `Installer ${plural(upd.count, "paquet")} sur ${
      hostname || "ce serveur"
    } ?`;
    $("upd-ok").focus();
  } else {
    $("upd-apply").focus();
  }
}

async function applyUpdates() {
  showConfirm(false);
  upd.running = true;
  refreshApplyButton();
  setUpdState("Lancement…", "run");
  try {
    const { status, body } = await api("/api/updates/apply", { method: "POST" });
    if (status !== 202 && status !== 409) throw new Error(errorText(status, body));
    startPolling(status === 409);
  } catch (err) {
    upd.running = false;
    refreshApplyButton();
    setUpdState(`Lancement impossible : ${err.message}`, "err");
  }
}

async function initUpdates() {
  $("upd-apply").addEventListener("click", () => showConfirm(true));
  $("upd-cancel").addEventListener("click", () => showConfirm(false));
  $("upd-ok").addEventListener("click", applyUpdates);

  await loadUpdates();
  // Une MAJ peut déjà tourner (page rechargée pendant l'exécution).
  try {
    const { ok, body } = await api("/api/updates/status");
    if (ok && body.state === "running") startPolling(true);
  } catch {
    // sans conséquence : l'état sera visible au prochain lancement
  }
}

connect();
initUpdates();
