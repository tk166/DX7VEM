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

const fx = {
  reverb: { mix: 0.18, size: 0.6, damp: 0.45, width: 1 },
  comp: { on: 1, thr: -12, ratio: 2, atk: 15, rel: 200, makeup: 1 },
  chorus: { on: 1, rate: 0.6, depth: 0.5, mix: 0.7, spread: 180 },
};
let bypass = false;

function setStatus(text, kind = "") {
  el.status.textContent = text;
  el.dot.className = "dot" + (kind ? " " + kind : "");
}

async function boot() {
  try {
    setStatus("加载数据…", "busy");
    const [tracks, mix, patches, notes] = await Promise.all([
      fetch("data/tracks.json").then((r) => r.json()),
      fetch("data/mix.json").then((r) => r.json()),
      fetch("data/patches.bin").then((r) => r.arrayBuffer()),
      fetch("data/notes.bin").then((r) => r.arrayBuffer()),
    ]);
    duration = mix.duration;
    el.tdur.textContent = fmt(duration);
    el.meta.textContent = `DX7 · msfa(WASM) · ${tracks.length} 轨 · 立体声`;
    el.master.value = gainToDb(mix.masterVolume).toFixed(1);
    el.masterVal.textContent = `${Number(el.master.value).toFixed(1)} dB`;
    buildTrackUI(tracks);

    setStatus("启动音频…", "busy");
    ctx = new AudioContext({ latencyHint: "playback" });
    setStatus("加载 worklet…", "busy");
    await ctx.audioWorklet.addModule("worklet.js");
    setStatus("加载 wasm…", "busy");
    const wasmBinary = await fetch("msfa.wasm").then((r) => r.arrayBuffer());

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
    });
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

function buildTrackUI(tracks) {
  el.tracks.innerHTML = "";
  tracks.forEach((t, i) => {
    const row = document.createElement("div");
    row.className = "row";
    const db = gainToDb(t.volume);
    row.innerHTML = `
      <span class="name">CH${t.channel + 1}<small>${t.voice || t.name}</small></span>
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

el.bypass.addEventListener("change", (e) => {
  bypass = e.target.checked;
  if (ready) node.port.postMessage({ type: "bypass", on: bypass });
  setStatus(bypass ? "FX 旁通（干声）" : "FX 开启", "ok");
});

setStatus("加载中…", "busy");
boot();
