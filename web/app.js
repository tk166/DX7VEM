// 主线程：加载数据 → 启动 AudioWorklet → 绑定 UI
const $ = (id) => document.getElementById(id);
const el = {
  play: $("play"), tcur: $("tcur"), tdur: $("tdur"), bar: $("bar"), fill: $("fill"), knob: $("knob"),
  meter: $("meter"), dot: $("dot"), status: $("status"), meta: $("meta"),
  master: $("master"), masterVal: $("masterVal"),
  tracks: $("tracks"), bypass: $("bypass"), hint: $("hint"),
};

const dbToGain = (db) => Math.pow(10, db / 20);
const gainToDb = (g) => (g <= 1e-6 ? -60 : 20 * Math.log10(g));
const fmt = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

let ctx = null, node = null, ready = false, playing = false;
let duration = 0, cur = 0, seekTarget = null, dragging = false;
let originalNotes = null;
let humCfg = null;
let humanizeApi = null;
let humTimer = null;

const fxDefault = {
  reverb: { mix: 0.18, size: 0.6, damp: 0.45, width: 1 },
  comp: { on: 1, thr: -12, ratio: 2, atk: 15, rel: 200, makeup: 1 },
  chorus: { on: 1, rate: 0.6, depth: 0.5, mix: 0.7, spread: 180 },
};
let fx = JSON.parse(JSON.stringify(fxDefault));
let bypass = false;
let trackList = [];
const DEFAULTS_KEY = "vem-ui-defaults-v2";

// VEM（弦乐组）：每条声部的参数在 vem-tracks.json 的 params 里，UI 逐条独立调节
let vemTracks = [];
let mixDefaults = { masterDb: -3, channelsDb: {} };
const postVemTrack = (index, params) => {
  const entry = vemTracks[index];
  if (entry) Object.assign(entry.params, params);
  if (ready) node.port.postMessage({ type: "vemTrack", index, params });
};
const ART_OPTIONS = [
  "Arco 连弓",
  "Marcato 重音",
  "Glissando 滑音",
  "Spiccato 跳弓",
  "Pizzicato 拨弦",
  "Col Legno 击弦",
  "Tremolo 颤弓",
];

// ---- 默认值导入/导出（本机保存用 localStorage） ----
function loadSavedDefaults() {
  try {
    return JSON.parse(localStorage.getItem(DEFAULTS_KEY) || "null");
  } catch (e) {
    return null;
  }
}

function applySavedDefaults(saved) {
  if (!saved) return;
  if (saved.fx) fx = Object.assign(fx, saved.fx);
  if (typeof saved.masterDb === "number") mixDefaults.masterDb = saved.masterDb;
  if (saved.channelsDb) mixDefaults.channelsDb = saved.channelsDb;
  if (Array.isArray(saved.vem)) {
    saved.vem.forEach((s, i) => {
      if (vemTracks[i] && s.params) Object.assign(vemTracks[i].params, s.params);
    });
  }
  if (saved.humanize) Object.assign(humCfg, saved.humanize);
}

function collectDefaults() {
  const channelsDb = {};
  document.querySelectorAll("#tracks .row").forEach((row, i) => {
    const t = trackList[i];
    if (!t) return;
    channelsDb[String(t.channel)] = Number(row.querySelector("input").value);
  });
  return {
    masterDb: Number(el.master.value),
    channelsDb,
    fx,
    vem: vemTracks.map((t) => ({ channel: t.channel, instrument: t.instrument, params: { ...t.params } })),
    humanize: { ...humCfg },
  };
}

function saveDefaults() {
  localStorage.setItem(DEFAULTS_KEY, JSON.stringify(collectDefaults()));
  setStatus("已保存为本机默认（刷新后仍生效）", "ok");
}

function exportDefaults() {
  const blob = new Blob([JSON.stringify(collectDefaults(), null, 2)], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "vem-defaults.json";
  a.click();
  URL.revokeObjectURL(a.href);
  setStatus("已导出 vem-defaults.json（可发给开发者固化为文件默认）", "ok");
}

function resetDefaults() {
  localStorage.removeItem(DEFAULTS_KEY);
  location.reload();
}

function syncFxUI() {
  const setNum = (id, v, f) => {
    const e = $(id);
    if (!e) return;
    e.value = v;
    const o = $(id + "Val");
    if (o) o.textContent = f(Number(v));
  };
  setNum("revMix", fx.reverb.mix, (v) => `${Math.round(v * 100)}%`);
  setNum("revSize", fx.reverb.size, (v) => v.toFixed(2));
  setNum("revDamp", fx.reverb.damp, (v) => v.toFixed(2));
  setNum("revWidth", fx.reverb.width, (v) => v.toFixed(2));
  if ($("revOn")) $("revOn").checked = fx.reverb.mix > 0;
  setNum("compThr", fx.comp.thr, (v) => `${v} dB`);
  setNum("compRatio", fx.comp.ratio, (v) => `${v}:1`);
  setNum("compAtk", fx.comp.atk, (v) => `${v} ms`);
  setNum("compRel", fx.comp.rel, (v) => `${v} ms`);
  setNum("compMakeup", fx.comp.makeup, (v) => `+${v} dB`);
  if ($("compOn")) $("compOn").checked = !!fx.comp.on;
  setNum("choRate", fx.chorus.rate, (v) => `${v.toFixed(2)} Hz`);
  setNum("choDepth", fx.chorus.depth, (v) => v.toFixed(2));
  setNum("choMix", fx.chorus.mix, (v) => `${Math.round(v * 100)}%`);
  setNum("choSpread", fx.chorus.spread, (v) => `${v}°`);
  if ($("choOn")) $("choOn").checked = !!fx.chorus.on;
}

function setStatus(text, kind = "") {
  el.status.textContent = text;
  el.dot.className = "dot" + (kind ? " " + kind : "");
}

async function boot() {
  try {
    setStatus("加载数据…", "busy");
    const [tracks, mix, patches, notesBuf, vemConfig, defaults, humConfigRaw] = await Promise.all([
      fetch("data/tracks.json").then((r) => r.json()),
      fetch("data/mix.json").then((r) => r.json()),
      fetch("data/patches.bin").then((r) => r.arrayBuffer()),
      fetch("data/notes.bin").then((r) => r.arrayBuffer()),
      fetch("data/vem-tracks.json").then((r) => (r.ok ? r.json() : [])).catch(() => []),
      fetch("data/mix-defaults.json").then((r) => (r.ok ? r.json() : null)).catch(() => null),
      fetch("data/humanize.json").then((r) => (r.ok ? r.json() : null)).catch(() => null),
    ]);
    let notes = notesBuf;
    let exprs = null;
    originalNotes = notesBuf;
    humCfg = Object.assign(
      {
        enabled: true,
        seed: 20261117,
        timingMs: 12,
        velocity: 6,
        phraseTempoMs: 22,
        phraseDynamics: 0.06,
        pitchCents: 4,
        bowJitter: 0.05,
        vibratoJitter: 0.12,
        pressureJitter: 0.05,
        attackJitter: 0.1,
      },
      humConfigRaw || {}
    );
    applySavedDefaults(loadSavedDefaults());
    syncFxUI();
    humanizeApi = await import("./humanize.js");
    setupHumUI();
    if (humCfg.enabled !== false) {
      const h = humanizeApi.humanizeBuffer(notesBuf, humCfg);
      notes = h.buffer;
      exprs = h.exprs;
    }
    vemTracks = vemConfig;
    trackList = tracks;
    if (defaults) mixDefaults = defaults;
    if (defaults && defaults.fx) fx = JSON.parse(JSON.stringify(defaults.fx));
    duration = mix.duration;
    el.tdur.textContent = fmt(duration);
    el.meta.textContent = `DX7 · msfa(WASM) · ${tracks.length} 轨 · 立体声`;
    el.master.value = Number(mixDefaults.masterDb ?? gainToDb(mix.masterVolume)).toFixed(1);
    el.masterVal.textContent = `${Number(el.master.value).toFixed(1)} dB`;
    buildTrackUI(tracks);

    setStatus("启动音频…", "busy");
    ctx = new AudioContext({ latencyHint: "playback" });
    setStatus("加载 worklet…", "busy");
    await ctx.audioWorklet.addModule("worklet.js");
    setStatus("加载 wasm…", "busy");
    const instruments = [...new Set(vemTracks.map((t) => t.instrument))];
    const [wasmBinary, vemBinary, noiseBuf, ...bodies] = await Promise.all([
      fetch(`msfa.wasm?v=${Date.now()}`, { cache: "no-store" }).then((r) => r.arrayBuffer()),
      fetch(`vem.wasm?v=${Date.now()}`, { cache: "no-store" }).then((r) => (r.ok ? r.arrayBuffer() : null)),
      fetch("data/vem/noise.bin").then((r) => (r.ok ? r.arrayBuffer() : null)),
      ...instruments.map((inst) =>
        fetch(`data/vem/body-${inst}.bin`).then((r) => (r.ok ? r.arrayBuffer() : null))
      ),
    ]);
    const bodyByInst = {};
    instruments.forEach((inst, i) => (bodyByInst[inst] = bodies[i]));
    const dataCount =
      (noiseBuf ? 1 : 0) + Object.values(bodyByInst).filter(Boolean).length;
    const vemReady = !!vemBinary && vemTracks.length > 0;

    node = new AudioWorkletNode(ctx, "msfa-processor", {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
    });
    node.connect(ctx.destination);
    node.port.onmessage = onWorklet;

    node.port.postMessage({
      type: "init",
      wasmBinary,
      patches,
      tracks,
      events: notes,
      master: dbToGain(Number(el.master.value)),
      fx: { ...fx, chorus: { ...fx.chorus } },
      duration,
      gains: tracks.map((t) =>
        dbToGain(mixDefaults.channelsDb?.[String(t.channel)] ?? gainToDb(t.volume))
      ),
      exprs,
      vem: vemReady
        ? {
            wasmBinary: vemBinary,
            tracks: vemTracks.map((t) => ({
              ...t,
              data: [bodyByInst[t.instrument], noiseBuf].filter(Boolean),
            })),
          }
        : null,
    });
    if (vemReady) {
      buildVemUI();
      el.meta.textContent = `DX7 9→${9 - new Set(vemTracks.map((t) => t.channel)).size + 1} 轨 + VEM 弦乐 ${vemTracks.length} 个声部${dataCount ? "（含琴体/弓噪）" : ""} · 立体声`;
      setStatus(`VEM: ${vemTracks.length} 个声部，数据 ${dataCount} 个文件`, "busy");
    }
  } catch (e) {
    console.error(e);
    setStatus("失败：" + e.message, "");
    el.status.classList.add("err");
  }
}

function onWorklet(ev) {
  const m = ev.data;
  if (m.type === "ready") {
    ready = true;
    setStatus(`就绪 · ${m.sampleRate} Hz · ${m.events} 事件`, "ok");
  } else if (m.type === "error") {
    console.error(m.message);
    setStatus("音频引擎错误：" + m.message.split("\n")[0], "");
    el.status.classList.add("err");
  } else if (m.type === "warn") {
    console.warn(m.message);
    setStatus("提示：" + m.message, "");
  } else if (m.type === "pos") {
    if (!dragging) cur = m.time;
    el.fill.style.width = `${Math.min(100, (cur / duration) * 100)}%`;
    el.knob.style.left = `${Math.min(100, (cur / duration) * 100)}%`;
    el.tcur.textContent = fmt(Math.min(cur, duration));
    el.meter.style.height = `${Math.min(100, m.peak * 100)}%`;
  } else if (m.type === "ended") {
    playing = false;
    el.play.textContent = "▶";
    el.play.classList.remove("on");
    setStatus("播放结束 · 按播放从头开始", "ok");
  }
}

function vemEntry(channel) {
  return vemTracks.find((t) => t.channel === channel) || null;
}

function buildTrackUI(tracks) {
  el.tracks.innerHTML = "";
  tracks.forEach((t, i) => {
    const row = document.createElement("div");
    row.className = "row";
    const entry = vemEntry(t.channel);
    const chDb = mixDefaults.channelsDb?.[String(t.channel)];
    const db =
      chDb !== undefined
        ? Number(chDb)
        : entry && entry.gain !== undefined
        ? gainToDb(entry.gain)
        : gainToDb(t.volume);
    const tag = entry ? `<small>VEM ${entry.instrument}</small>` : "";
    row.innerHTML = `
      <span class="name">CH${t.channel + 1}${tag}</span>
      <input type="range" min="-40" max="12" step="0.5" value="${db.toFixed(1)}" />
      <span class="val">${db.toFixed(1)} dB</span>`;
    const input = row.querySelector("input");
    const val = row.querySelector(".val");
    input.addEventListener("input", () => {
      const v = Number(input.value);
      val.textContent = `${v.toFixed(1)} dB`;
      if (ready) node.port.postMessage({ type: "trackGain", index: i, value: dbToGain(v) });
    });
    el.tracks.appendChild(row);
  });
}

function addVemRow(panel, idx, key, label, min, max, step, value, fmt, extra) {
  const row = document.createElement("div");
  row.className = "row";
  row.innerHTML = `<span class="name">${label}</span><input type="range" min="${min}" max="${max}" step="${step}" value="${value}" /><span class="val">${fmt(Number(value))}</span>`;
  const input = row.querySelector("input");
  const val = row.querySelector(".val");
  input.addEventListener("input", () => {
    const v = Number(input.value);
    val.textContent = fmt(v);
    postVemTrack(idx, { [key]: v, ...(extra ? extra(v) : {}) });
  });
  panel.appendChild(row);
}

function buildVemUI() {
  const host = $("vemPanels");
  if (!host) return;
  host.innerHTML = "";
  vemTracks.forEach((t, idx) => {
    const p = t.params || {};
    const panel = document.createElement("div");
    panel.className = "vem-panel";
    const head = document.createElement("h3");
    head.innerHTML = `${t.label} <small style="color:var(--dim);font-weight:400">${t.instrument}</small>`;
    panel.appendChild(head);

    const artRow = document.createElement("div");
    artRow.className = "row";
    artRow.innerHTML = `<span class="name">弓法</span><select style="grid-column: span 2; padding:6px; border-radius:8px; background:var(--panel2); color:var(--text); border:1px solid var(--line)">${ART_OPTIONS.map(
      (label, i) => `<option value="${i}">${label}</option>`
    ).join("")}</select>`;
    const sel = artRow.querySelector("select");
    sel.value = String(p.art ?? 0);
    sel.addEventListener("change", () => postVemTrack(idx, { art: Number(sel.value) }));
    panel.appendChild(artRow);

    addVemRow(panel, idx, "bow", "弓位", 0, 1, 0.01, p.bow ?? 0.5, (v) => v.toFixed(2));
    addVemRow(panel, idx, "vibrato", "揉弦深度", 0, 1.5, 0.01, p.vibrato ?? 0.4, (v) => v.toFixed(2));
    addVemRow(panel, idx, "vibOnsetMs", "揉弦渐入", 0, 500, 10, p.vibOnsetMs ?? 120, (v) => `${v} ms`);
    addVemRow(panel, idx, "attackMs", "起音", 2, 300, 1, p.attackMs ?? 8, (v) => `${v} ms`);
    addVemRow(panel, idx, "accent", "音头重音", 0, 0.3, 0.01, p.accent ?? 0, (v) => v.toFixed(2));
    addVemRow(panel, idx, "taper", "换音渐弱", 0, 0.4, 0.01, p.taper ?? 0, (v) => v.toFixed(2));
    addVemRow(panel, idx, "legatoStart", "连弓接续", 0, 1, 0.01, p.legatoStart ?? 0.5, (v) => v.toFixed(2));
    addVemRow(panel, idx, "legatoRampMs", "换音过渡", 0, 100, 1, p.legatoRampMs ?? 0, (v) => `${v} ms`);
    addVemRow(panel, idx, "legatoBoost", "换音补偿", 0, 8, 0.1, p.legatoBoost ?? 0, (v) => v.toFixed(1));
    addVemRow(panel, idx, "bowNoise", "弓噪", 0, 1, 0.01, p.bowNoise ?? 0.5, (v) => `${Math.round(v * 100)}%`, (v) => ({ bowNoiseOn: v > 0 ? 1 : 0 }));
    addVemRow(panel, idx, "bodyMix", "琴体共振", 0, 0.6, 0.01, p.bodyMix ?? 0.18, (v) => v.toFixed(2));

    host.appendChild(panel);
  });
}

function postFx() {
  if (ready) node.port.postMessage({ type: "fx", fx });
}

// ---- 传输控制 ----
function togglePlay() {
  if (!ready) return;
  if (ctx.state === "suspended") ctx.resume();
  playing = !playing;
  el.play.textContent = playing ? "❚❚" : "▶";
  el.play.classList.toggle("on", playing);
  if (playing) {
    if (cur >= duration - 0.2) {
      seek(0);
    }
    node.port.postMessage({ type: "play" });
    setStatus("播放中", "ok");
  } else {
    node.port.postMessage({ type: "pause" });
    setStatus("暂停", "ok");
  }
}

function seek(t) {
  cur = Math.max(0, Math.min(t, duration));
  if (ready) node.port.postMessage({ type: "seek", time: cur });
  el.fill.style.width = `${(cur / duration) * 100}%`;
  el.knob.style.left = `${(cur / duration) * 100}%`;
  el.tcur.textContent = fmt(cur);
}

el.play.addEventListener("click", togglePlay);
window.addEventListener("keydown", (e) => {
  if (e.code === "Space" && e.target.tagName !== "INPUT") {
    e.preventDefault();
    togglePlay();
  }
});

// 进度条拖动
const barAt = (e) => {
  const r = el.bar.getBoundingClientRect();
  return ((e.clientX - r.left) / r.width) * duration;
};
el.bar.addEventListener("pointerdown", (e) => {
  dragging = true;
  el.bar.setPointerCapture(e.pointerId);
  seek(barAt(e));
});
el.bar.addEventListener("pointermove", (e) => {
  if (dragging) seek(barAt(e));
});
el.bar.addEventListener("pointerup", (e) => {
  dragging = false;
  seek(barAt(e));
});

// ---- 参数绑定 ----
function bind(id, fmtVal, onChange) {
  const e = $(id);
  const out = $(id + "Val");
  const upd = () => {
    const v = Number(e.value);
    if (out) out.textContent = fmtVal(v);
    onChange(v);
  };
  e.addEventListener("input", upd);
  upd();
}

el.master.addEventListener("input", () => {
  const db = Number(el.master.value);
  el.masterVal.textContent = `${db.toFixed(1)} dB`;
  if (ready) node.port.postMessage({ type: "master", value: dbToGain(db) });
});

bind("revMix", (v) => `${Math.round(v * 100)}%`, (v) => { fx.reverb.mix = v; postFx(); });
bind("revSize", (v) => v.toFixed(2), (v) => { fx.reverb.size = v; postFx(); });
bind("revDamp", (v) => v.toFixed(2), (v) => { fx.reverb.damp = v; postFx(); });
bind("revWidth", (v) => v.toFixed(2), (v) => { fx.reverb.width = v; postFx(); });
$("revOn").addEventListener("change", (e) => { fx.reverb.mix = e.target.checked ? Number($("revMix").value) : 0; postFx(); });

bind("compThr", (v) => `${v} dB`, (v) => { fx.comp.thr = v; postFx(); });
bind("compRatio", (v) => `${v}:1`, (v) => { fx.comp.ratio = v; postFx(); });
bind("compAtk", (v) => `${v} ms`, (v) => { fx.comp.atk = v; postFx(); });
bind("compRel", (v) => `${v} ms`, (v) => { fx.comp.rel = v; postFx(); });
bind("compMakeup", (v) => `+${v} dB`, (v) => { fx.comp.makeup = v; postFx(); });
$("compOn").addEventListener("change", (e) => { fx.comp.on = e.target.checked ? 1 : 0; postFx(); });

bind("choRate", (v) => `${v.toFixed(2)} Hz`, (v) => { fx.chorus.rate = v; postFx(); });
bind("choDepth", (v) => v.toFixed(2), (v) => { fx.chorus.depth = v; postFx(); });
bind("choMix", (v) => `${Math.round(v * 100)}%`, (v) => { fx.chorus.mix = v; postFx(); });
bind("choSpread", (v) => `${v}°`, (v) => { fx.chorus.spread = v; postFx(); });
$("choOn").addEventListener("change", (e) => { fx.chorus.on = e.target.checked ? 1 : 0; postFx(); });

// ---- VEM：每个声部的控件由 buildVemUI() 动态生成 ----

el.bypass.addEventListener("change", (e) => {
  bypass = e.target.checked;
  if (ready) node.port.postMessage({ type: "bypass", on: bypass });
  setStatus(bypass ? "FX 旁通（干声）" : "FX 开启", "ok");
});

// ---- 演奏起伏 ----
function setupHumUI() {
  const cfg = humCfg;
  const on = $("humOn");
  on.checked = cfg.enabled !== false;
  on.addEventListener("change", (e) => {
    cfg.enabled = e.target.checked;
    scheduleHumanize();
  });

  const slider = (id, key, fmt) => {
    const el = $(id);
    const out = $(id + "Val");
    if (!el) return;
    el.value = cfg[key];
    const upd = () => {
      const v = Number(el.value);
      cfg[key] = v;
      if (out) out.textContent = fmt(v);
      scheduleHumanize();
    };
    el.addEventListener("input", upd);
    if (out) out.textContent = fmt(Number(cfg[key]));
  };
  slider("humTime", "timingMs", (v) => `${v} ms`);
  slider("humVel", "velocity", (v) => v.toFixed(1));
  slider("humTempo", "phraseTempoMs", (v) => `${v} ms`);
  slider("humDyn", "phraseDynamics", (v) => `${Math.round(v * 100)}%`);
  slider("humCents", "pitchCents", (v) => `${v} cent`);
  slider("humBow", "bowJitter", (v) => v.toFixed(2));
  slider("humVib", "vibratoJitter", (v) => `${Math.round(v * 100)}%`);
  slider("humPress", "pressureJitter", (v) => `${Math.round(v * 100)}%`);
  slider("humAttack", "attackJitter", (v) => `${Math.round(v * 100)}%`);

  const seed = $("humSeed");
  seed.value = cfg.seed;
  seed.addEventListener("change", () => {
    cfg.seed = Number(seed.value) || 20261117;
    scheduleHumanize();
  });
}

function scheduleHumanize() {
  if (humTimer) clearTimeout(humTimer);
  humTimer = setTimeout(applyHumanize, 150);
}

function applyHumanize() {
  if (!ready || !originalNotes || !humanizeApi) return;
  let buf = originalNotes;
  let exprs = null;
  if (humCfg.enabled !== false) {
    const h = humanizeApi.humanizeBuffer(originalNotes, humCfg);
    buf = h.buffer;
    exprs = h.exprs;
  }
  node.port.postMessage({ type: "events", events: buf, exprs });
  setStatus("演奏起伏已更新", "ok");
}

// ---- 默认值按钮 ----
$("saveDefaults")?.addEventListener("click", saveDefaults);
$("exportDefaults")?.addEventListener("click", exportDefaults);
$("resetDefaults")?.addEventListener("click", resetDefaults);

setStatus("加载中…", "busy");
boot();
