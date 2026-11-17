// 离线整曲混音：DX7(msfa) 保留未接管的轨道 + VEM 接管多条弦乐轨 → float32 WAV
//
// 用法:
//   node scripts/render-mix.mjs out/p03-vem-mix.wav [选项]
// 选项:
//   --sr 44100
//   --vem-tracks web/data/vem-tracks.json   通道→乐器映射（可多条同通道，按音域拆分）
//   --notes/--tracks/--patches/--mix        输入
//   --data out/vem-data                     琴体 IR / 弓噪
//   --preset melody|fidelity                包络预设（默认 melody）
//   --attack-ms/--attack-vel-ms/--release-ms/--release-slow-ms/--vib-onset-ms
//   --dur 秒
//   --stems                                 写出每个 VEM 引擎的 stem
//   --write-config                          把校准后的增益写回 vem-tracks.json
//
// 电平校准：每个 VEM 通道，用 DX7 同通道 stem 的 RMS 对齐 VEM 该通道的合成 RMS。

import { readFileSync, writeFileSync, existsSync, mkdirSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const argv = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined && !argv[i + 1].startsWith("--") ? argv[i + 1] : dflt;
};
const has = (name) => argv.includes(name);

const SR = Number(opt("--sr", 44100));
const BLOCK = 64;
const notesPath = opt("--notes", join(root, "web/data/notes.bin"));
const tracksPath = opt("--tracks", join(root, "web/data/tracks.json"));
const patchesPath = opt("--patches", join(root, "web/data/patches.bin"));
const mixPath = opt("--mix", join(root, "web/data/mix.json"));
const dataDir = opt("--data", join(root, "out/vem-data"));
const configPath = opt("--vem-tracks", join(root, "web/data/vem-tracks.json"));
const mixDefaultsPath = opt("--mix-defaults", "");
const dbToGain = (db) => Math.pow(10, db / 20);
const mixDefaults = mixDefaultsPath && existsSync(mixDefaultsPath)
  ? JSON.parse(readFileSync(mixDefaultsPath, "utf8"))
  : null;
const mixDb = mixDefaults?.channelsDb || null;
const outPath = argv.find((a) => a.endsWith(".wav")) || join(root, "out/p03-vem-mix.wav");
const preset = opt("--preset", "melody");
const explicitEnv = ["--attack-ms", "--attack-vel-ms", "--release-ms", "--release-slow-ms", "--bow-settle-ms", "--vib-onset-ms"].some((k) => argv.includes(k));

const INSTRUMENT_INDEX = { violin: 0, viola: 1, cello: 2, bass: 3 };
const BODY_MIX = { violin: 0.18, viola: 0.18, cello: 0.22, bass: 0.3 };

// ---------------------------------------------------------------- 输入

function loadEvents(path) {
  const b = readFileSync(path);
  if (b.toString("ascii", 0, 4) !== "MFA1") throw new Error(`notes.bin 魔数不对: ${path}`);
  const n = b.readUInt32LE(4);
  const duration = b.readFloatLE(8);
  const events = [];
  for (let i = 0; i < n; i++) {
    const o = 12 + i * 8;
    events.push({
      t: b.readFloatLE(o),
      ch: b.readUInt8(o + 4),
      note: b.readUInt8(o + 5),
      vel: b.readUInt8(o + 6),
      on: b.readUInt8(o + 7) !== 0,
    });
  }
  events.sort((a, b) => a.t - b.t || (a.on === b.on ? 0 : a.on ? 1 : -1));
  return { events, duration };
}

const { events, duration: notesDuration } = loadEvents(notesPath);
const tracks = JSON.parse(readFileSync(tracksPath, "utf8"));
const patches = readFileSync(patchesPath);
const mix = JSON.parse(readFileSync(mixPath, "utf8"));
let vemTracks = JSON.parse(readFileSync(configPath, "utf8"));
const master = mixDefaults
  ? dbToGain(mixDefaults.masterDb ?? 0)
  : mix.masterVolume ?? 1;
const dur = Number(opt("--dur", Math.max(notesDuration, mix.duration ?? 0) + 2));
const frames = Math.ceil(dur * SR);
const vemChannels = new Set(vemTracks.map((v) => v.channel));
console.log(
  `输入: ${events.length} 事件 / ${dur.toFixed(1)}s @${SR}Hz | master=${master.toFixed(3)} | VEM 通道 ${[...vemChannels].map((c) => `CH${c + 1}`).join(", ")}`
);

// ---------------------------------------------------------------- wasm

async function loadWasm(path, imports = {}) {
  const bytes = readFileSync(path);
  const { instance } = await WebAssembly.instantiate(bytes, imports);
  return instance.exports;
}
const heapF32 = (E, ptr, n) => new Float32Array(E.memory.buffer, ptr, n);
function heapBytes(E, ptr, bytes) {
  new Uint8Array(E.memory.buffer, ptr, bytes.length).set(bytes);
}

const msfa = await loadWasm(join(root, "web/msfa.wasm"), {
  env: { emscripten_notify_memory_growth: () => {} },
});
const vem = await loadWasm(join(root, "vem/wasm/vem.wasm"), {});

// ---------------------------------------------------------------- msfa

const TRACK_PATCH = 156;
function makeMsfa(channels) {
  const mx = msfa.msfa_mixer_new(SR);
  msfa.msfa_mixer_set_master(mx, master);
  msfa.msfa_mixer_set_reverb(mx, 0, 0.6, 0.45, 1);
  msfa.msfa_mixer_set_comp(mx, 0, -12, 2, 15, 200, 1);
  const ptrs = [];
  for (let i = 0; i < tracks.length; i++) {
    const ch = tracks[i].channel;
    if (!channels.has(ch)) continue;
    const patch = patches.subarray(i * TRACK_PATCH, i * TRACK_PATCH + TRACK_PATCH);
    const p = msfa.malloc(TRACK_PATCH);
    heapBytes(msfa, p, patch);
    ptrs.push(p);
    msfa.msfa_mixer_set_patch(mx, ch, p);
    const db = mixDb ? mixDb[String(ch)] : undefined;
    msfa.msfa_mixer_set_gain(mx, ch, db !== undefined ? dbToGain(db) : tracks[i].volume ?? 1);
    msfa.msfa_mixer_set_chorus(mx, ch, 0, 0.6, 0.5, 0.7, 180);
  }
  return { mx, ptrs };
}

// 渲染（可选累加）；channels 决定触发哪些通道的事件
function renderMsfa({ mx }, channels, targetL, targetR, add) {
  const outL = msfa.malloc(BLOCK * 4);
  const outR = msfa.malloc(BLOCK * 4);
  let ei = 0;
  for (let pos = 0; pos < frames; pos += BLOCK) {
    const nowT = pos / SR;
    while (ei < events.length && events[ei].t <= nowT) {
      const e = events[ei++];
      if (!channels.has(e.ch)) continue;
      if (e.on) msfa.msfa_mixer_note_on(mx, e.ch, e.note, e.vel);
      else msfa.msfa_mixer_note_off(mx, e.ch, e.note);
    }
    msfa.msfa_mixer_render(mx, outL, outR, BLOCK);
    const sl = heapF32(msfa, outL, BLOCK);
    const sr = heapF32(msfa, outR, BLOCK);
    const n = Math.min(BLOCK, frames - pos);
    if (add) {
      for (let i = 0; i < n; i++) {
        targetL[pos + i] += sl[i];
        targetR[pos + i] += sr[i];
      }
    } else {
      for (let i = 0; i < n; i++) {
        targetL[pos + i] = sl[i];
        targetR[pos + i] = sr[i];
      }
    }
  }
  msfa.free(outL);
  msfa.free(outR);
}

// 渲染某个通道的 DX7 stem（单声道，用于校准和诊断）
function msfaChannelStem(ch) {
  const { mx } = makeMsfa(new Set([ch]));
  const outL = msfa.malloc(BLOCK * 4);
  const outR = msfa.malloc(BLOCK * 4);
  const mono = new Float32Array(frames);
  let ei = 0;
  for (let pos = 0; pos < frames; pos += BLOCK) {
    const nowT = pos / SR;
    while (ei < events.length && events[ei].t <= nowT) {
      const e = events[ei++];
      if (e.ch !== ch) continue;
      if (e.on) msfa.msfa_mixer_note_on(mx, ch, e.note, e.vel);
      else msfa.msfa_mixer_note_off(mx, ch, e.note);
    }
    msfa.msfa_mixer_render(mx, outL, outR, BLOCK);
    const sl = heapF32(msfa, outL, BLOCK);
    const n = Math.min(BLOCK, frames - pos);
    for (let i = 0; i < n; i++) mono[pos + i] = sl[i];
  }
  msfa.free(outL);
  msfa.free(outR);
  return mono;
}

// ---------------------------------------------------------------- VEM

function applyVemParams(eng, entry) {
  vem.vem_set_seed(eng, 12345);
  vem.vem_set_master(eng, master);
  vem.vem_set_gain(eng, 1);
  vem.vem_set_polyphonic(eng, entry.polyphonic ? 1 : 0);
  vem.vem_set_articulation(eng, ["arco", "marcato", "glissando", "spiccato", "pizzicato", "collegno", "tremolo"].indexOf(entry.art || "arco"));
  vem.vem_set_bow(eng, 0.5, 1);
  vem.vem_set_vibrato(eng, 0.4, 5.2);
  vem.vem_set_body(eng, BODY_MIX[entry.instrument] ?? 0.18, 0.6);
  vem.vem_set_bow_noise(eng, 1, 0.5);
  const melody = (entry.preset || preset) === "melody" || explicitEnv;
  const dx = (entry.preset || preset) === "melody" && !explicitEnv;
  if (melody) {
    vem.vem_set_envelope(
      eng,
      Number(opt("--attack-ms", dx ? 8 : 20.833)),
      Number(opt("--attack-vel-ms", dx ? 60 : 625)),
      Number(opt("--release-ms", dx ? 60 : 20.833)),
      Number(opt("--release-slow-ms", dx ? 120 : 208.333)),
      Number(opt("--bow-settle-ms", 208.333)),
      Number(opt("--vib-onset-ms", dx ? 120 : 0)),
      60
    );
  }
}

function loadVemData(eng, instrument) {
  let loaded = 0;
  for (const name of [`body-${instrument}.bin`, "noise.bin"]) {
    const p = join(dataDir, name);
    if (!existsSync(p)) continue;
    const bytes = readFileSync(p);
    const ptr = vem.vem_alloc(bytes.length);
    heapBytes(vem, ptr, bytes);
    vem.vem_set_patch(eng, ptr, bytes.length);
    vem.vem_free_mem(ptr, bytes.length);
    loaded++;
  }
  return loaded;
}

const engines = vemTracks.map((entry) => {
  const eng = vem.vem_new(SR, INSTRUMENT_INDEX[entry.instrument] ?? 0);
  applyVemParams(eng, entry);
  const loaded = loadVemData(eng, entry.instrument);
  console.log(`  VEM ${entry.label}: ${entry.instrument} ch=${entry.channel + 1} notes=${entry.minNote}..${entry.maxNote} poly=${!!entry.polyphonic} data=${loaded}`);
  return { eng, entry, stem: null };
});

function engineEvents(entry) {
  return events.filter((e) => e.ch === entry.channel && e.note >= entry.minNote && e.note <= entry.maxNote);
}

// 渲染一个引擎 → 单声道 stem（VEM 输出本来就是 L==R）
function renderEngine(engine) {
  const outL = vem.vem_alloc(BLOCK * 4);
  const outR = vem.vem_alloc(BLOCK * 4);
  const mono = new Float32Array(frames);
  const ev = engineEvents(engine.entry);
  let ei = 0;
  for (let pos = 0; pos < frames; pos += BLOCK) {
    const nowT = pos / SR;
    while (ei < ev.length && ev[ei].t <= nowT) {
      const e = ev[ei++];
      if (e.on) vem.vem_note_on(engine.eng, e.note, e.vel / 127);
      else vem.vem_note_off(engine.eng, e.note);
    }
    vem.vem_render(engine.eng, outL, outR, BLOCK);
    const sl = heapF32(vem, outL, BLOCK);
    const n = Math.min(BLOCK, frames - pos);
    for (let i = 0; i < n; i++) mono[pos + i] = sl[i];
  }
  vem.vem_free_mem(outL, BLOCK * 4);
  vem.vem_free_mem(outR, BLOCK * 4);
  engine.stem = mono;
  return mono;
}

const rmsOf = (arr) => {
  let s = 0;
  for (let i = 0; i < arr.length; i++) s += arr[i] * arr[i];
  return Math.sqrt(s / Math.max(1, arr.length));
};

console.log("渲染 VEM stems…");
for (const e of engines) renderEngine(e);

// 每通道电平校准：DX7 同通道 stem 的 RMS / VEM 合成 RMS
const gainByChannel = new Map();
const dx7Stems = new Map();
for (const ch of vemChannels) {
  const group = engines.filter((e) => e.entry.channel === ch);
  const sum = new Float32Array(frames);
  for (const e of group) {
    const s = e.stem;
    for (let i = 0; i < frames; i++) sum[i] += s[i];
  }
  const rmsVem = rmsOf(sum);
  const dx7 = msfaChannelStem(ch);
  dx7Stems.set(ch, dx7);
  const rmsDx7 = rmsOf(dx7);
  if (mixDb) {
    const gain = dbToGain(mixDb[String(ch)] ?? 0);
    gainByChannel.set(ch, gain);
    for (const e of group) e.entry.gain = gain;
    console.log(
      `  CH${ch + 1}: 使用 mix-defaults ${(mixDb[String(ch)] ?? 0).toFixed(1)} dB（DX7 rms=${rmsDx7.toFixed(5)} / VEM rms=${rmsVem.toFixed(5)}，不再按 RMS 校准）`
    );
    continue;
  }
  const gain = rmsVem > 1e-12 ? rmsDx7 / rmsVem : 1;
  gainByChannel.set(ch, gain);
  for (const e of group) e.entry.gain = gain;
  console.log(
    `  CH${ch + 1}: DX7 rms=${rmsDx7.toFixed(5)} | VEM rms=${rmsVem.toFixed(5)} | 增益 ${gain.toFixed(3)} (${(20 * Math.log10(gain)).toFixed(1)} dB)`
  );
}

if (has("--write-config")) {
  writeFileSync(configPath, JSON.stringify(vemTracks, null, 2) + "\n", "utf8");
  console.log(`已写回 ${configPath}`);
}

if (has("--stems")) {
  const dir = join(root, "out/m4-stems");
  mkdirSync(dir, { recursive: true });
  for (const e of engines) {
    const name = `vem-${e.entry.instrument}-ch${e.entry.channel + 1}.wav`;
    writeFloatWav(join(dir, name), e.stem, e.stem, SR);
  }
  for (const [ch, stem] of dx7Stems) {
    writeFloatWav(join(dir, `dx7-ch${ch + 1}.wav`), stem, stem, SR);
  }
  console.log(`stems → ${dir}`);
}

// ---------------------------------------------------------------- 混音

const otherChannels = new Set(tracks.map((t) => t.channel).filter((c) => !vemChannels.has(c)));
console.log(`渲染 DX7 其余通道（${[...otherChannels].map((c) => "CH" + (c + 1)).join(", ")}）并混音…`);
const mixL = new Float32Array(frames);
const mixR = new Float32Array(frames);
renderMsfa(makeMsfa(otherChannels), otherChannels, mixL, mixR, false);
const vemMono = new Float32Array(frames);
for (const e of engines) {
  const gain = e.entry.gain ?? gainByChannel.get(e.entry.channel) ?? 1;
  const s = e.stem;
  for (let i = 0; i < frames; i++) vemMono[i] += s[i] * gain;
}
for (let i = 0; i < frames; i++) {
  mixL[i] += vemMono[i];
  mixR[i] += vemMono[i];
}

let peak = 0;
for (let i = 0; i < frames; i++) {
  const a = Math.max(Math.abs(mixL[i]), Math.abs(mixR[i]));
  if (a > peak) peak = a;
}

writeFloatWav(outPath, mixL, mixR, SR);
console.log(`写出 ${outPath}`);
console.log(`  长度 ${dur.toFixed(2)}s | peak ${peak.toFixed(4)} (${(20 * Math.log10(peak || 1e-9)).toFixed(2)} dBFS) | rms ${rmsOf(mixL).toFixed(5)}`);

function writeFloatWav(path, l, r, sr) {
  const frames = Math.min(l.length, r.length);
  const buf = Buffer.alloc(44 + frames * 8);
  buf.write("RIFF", 0);
  buf.writeUInt32LE(36 + frames * 8, 4);
  buf.write("WAVE", 8);
  buf.write("fmt ", 12);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(3, 20);
  buf.writeUInt16LE(2, 22);
  buf.writeUInt32LE(sr, 24);
  buf.writeUInt32LE(sr * 8, 28);
  buf.writeUInt16LE(8, 32);
  buf.writeUInt16LE(32, 34);
  buf.write("data", 36);
  buf.writeUInt32LE(frames * 8, 40);
  for (let i = 0; i < frames; i++) {
    buf.writeFloatLE(l[i], 44 + i * 8);
    buf.writeFloatLE(r[i], 44 + i * 8 + 4);
  }
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, buf);
}
