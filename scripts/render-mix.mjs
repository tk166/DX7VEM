// 离线整曲混音：DX7(msfa) 9 轨 + VEM(CH3 旋律) → float32 WAV
//
// 用法:
//   node scripts/render-mix.mjs out/p03-vem-mix.wav [选项]
// 选项:
//   --sr 44100                 采样率（默认与 p03-reaper 一致）
//   --notes web/data/notes.bin
//   --tracks web/data/tracks.json
//   --patches web/data/patches.bin
//   --mix web/data/mix.json
//   --vem-channel 2            VEM 接管的通道（0 基，CH3 = 2）
//   --vem-gain auto|<数值>      CH3 电平校准（auto = 对齐 DX7 的 CH3 stem RMS）
//   --data out/vem-data        琴体 IR / 弓噪（不存在则不加）
//   --art arco|pizzicato|...
//   --dur 秒                    渲染长度（默认 mix.duration + 2）
//   --stems                     额外写出三个 stem 便于排查
//
// 注意：msfa 侧 FX 全关（混响/压缩/合唱），只比干声；VEM 的总线也关。
// 这样叠加结果可以直接和 REAPER 的干混（p03-reaper.wav）比电平/节奏。

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
const art = opt("--art", "arco");
const vemChannel = Number(opt("--vem-channel", 2));
const outPath = argv.find((a) => a.endsWith(".wav")) || join(root, "out/p03-vem-mix.wav");

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
const master = mix.masterVolume ?? 1;
const dur = Number(opt("--dur", Math.max(notesDuration, mix.duration ?? 0) + 2));
const frames = Math.ceil(dur * SR);
console.log(
  `输入: ${events.length} 事件 / ${dur.toFixed(1)}s @${SR}Hz | master=${master.toFixed(3)} | VEM=CH${vemChannel + 1}`
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
    msfa.msfa_mixer_set_gain(mx, ch, tracks[i].volume ?? 1);
    msfa.msfa_mixer_set_chorus(mx, ch, 0, 0.6, 0.5, 0.7, 180);
  }
  return { mx, ptrs };
}

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
    for (let i = 0; i < n; i++) {
      if (add) {
        targetL[pos + i] += sl[i];
        targetR[pos + i] += sr[i];
      } else {
        targetL[pos + i] = sl[i];
        targetR[pos + i] = sr[i];
      }
    }
  }
  msfa.free(outL);
  msfa.free(outR);
}

// ---------------------------------------------------------------- VEM

const eng = vem.vem_new(SR, 0);
vem.vem_set_seed(eng, 12345);
vem.vem_set_gain(eng, 1);
vem.vem_set_master(eng, master);
vem.vem_set_reverb(eng, 0, 0.6, 0.45, 1);
vem.vem_set_comp(eng, 0, -12, 2, 15, 200, 1);
vem.vem_set_articulation(eng, ["arco", "marcato", "glissando", "spiccato", "pizzicato", "collegno", "tremolo"].indexOf(art));

// 包络/演奏参数
//   --preset melody（默认）：收紧起音/释放，适配《梁祝》这种大量 30 ms 短音
//   --preset fidelity        ：完全用参考实现的默认包络
const preset = opt("--preset", "melody");
const explicitEnv = ["--attack-ms", "--attack-vel-ms", "--release-ms", "--release-slow-ms", "--bow-settle-ms", "--vib-onset-ms"].some((k) => argv.includes(k));
if (preset === "melody" || explicitEnv) {
  const dx = preset === "melody" && !explicitEnv;
  vem.vem_set_envelope(
    eng,
    Number(opt("--attack-ms", dx ? 8 : 20.833)),
    Number(opt("--attack-vel-ms", dx ? 60 : 625)),
    Number(opt("--release-ms", dx ? 60 : 20.833)),
    Number(opt("--release-slow-ms", dx ? 120 : 208.333)),
    Number(opt("--bow-settle-ms", 208.333)),
    Number(opt("--vib-onset-ms", dx ? 120 : 0)),
    Number(opt("--vib-onset-ramp-ms", 60))
  );
  console.log(`包络: ${dx ? "melody 预设" : "自定义"}`);
}
if (argv.includes("--vibrato")) {
  vem.vem_set_vibrato(eng, Number(opt("--vibrato", 0.4)), 5.2);
}

if (existsSync(dataDir)) {
  let loaded = 0;
  for (const name of ["body-violin.bin", "noise.bin"]) {
    const p = join(dataDir, name);
    if (!existsSync(p)) continue;
    const bytes = readFileSync(p);
    const ptr = vem.vem_alloc(bytes.length);
    heapBytes(vem, ptr, bytes);
    vem.vem_set_patch(eng, ptr, bytes.length);
    vem.vem_free_mem(ptr, bytes.length);
    loaded++;
  }
  console.log(`VEM 数据: ${dataDir} (${loaded} 个文件)`);
}

const velCurve = opt("--vel-curve", "off");
function renderVem(targetL, targetR) {
  const outL = vem.vem_alloc(BLOCK * 4);
  const outR = vem.vem_alloc(BLOCK * 4);
  const ev = events.filter((e) => e.ch === vemChannel);
  let ei = 0;
  for (let pos = 0; pos < frames; pos += BLOCK) {
    const nowT = pos / SR;
    while (ei < ev.length && ev[ei].t <= nowT) {
      const e = ev[ei++];
      if (e.on) {
        if (velCurve !== "off") {
          const v = Math.max(0, Math.min(1, e.vel / 127));
          vem.vem_set_volume(eng, Math.max(0.15, Math.min(1, 0.15 + 0.85 * Math.pow(v, 1.0))));
        }
        vem.vem_note_on(eng, e.note, e.vel / 127);
      } else {
        vem.vem_note_off(eng, e.note);
      }
    }
    vem.vem_render(eng, outL, outR, BLOCK);
    const sl = heapF32(vem, outL, BLOCK);
    const sr = heapF32(vem, outR, BLOCK);
    const n = Math.min(BLOCK, frames - pos);
    for (let i = 0; i < n; i++) {
      targetL[pos + i] = sl[i];
      targetR[pos + i] = sr[i];
    }
  }
  vem.vem_free_mem(outL, BLOCK * 4);
  vem.vem_free_mem(outR, BLOCK * 4);
}

// ---------------------------------------------------------------- 电平校准

const rms = (l, r) => {
  let s = 0;
  for (let i = 0; i < l.length; i++) s += l[i] * l[i] + r[i] * r[i];
  return Math.sqrt(s / (2 * l.length));
};

const otherChannels = new Set(tracks.map((t) => t.channel).filter((c) => c !== vemChannel));

console.log("渲染 DX7 CH3 stem（仅用于电平校准）…");
const ch3L = new Float32Array(frames);
const ch3R = new Float32Array(frames);
renderMsfa(makeMsfa(new Set([vemChannel])), new Set([vemChannel]), ch3L, ch3R, false);
const rmsCh3 = rms(ch3L, ch3R);

console.log("渲染 VEM CH3 stem…");
const vemL = new Float32Array(frames);
const vemR = new Float32Array(frames);
renderVem(vemL, vemR);
const rmsVem = rms(vemL, vemR);

let vemGain;
if (opt("--vem-gain", "auto") === "auto") {
  vemGain = rmsVem > 1e-12 ? rmsCh3 / rmsVem : 1;
} else {
  vemGain = Number(opt("--vem-gain", "1"));
}
console.log(
  `电平: DX7 CH3 rms=${rmsCh3.toFixed(5)} | VEM rms=${rmsVem.toFixed(5)} | VEM 增益 ${vemGain.toFixed(3)} (${(20 * Math.log10(vemGain)).toFixed(1)} dB)`
);

if (has("--stems")) {
  const dir = join(root, "out/m4-stems");
  mkdirSync(dir, { recursive: true });
  writeFloatWav(join(dir, "ch3-dx7.wav"), ch3L, ch3R, SR);
  writeFloatWav(join(dir, "ch3-vem.wav"), vemL, vemR, SR);
  console.log(`stems → ${dir}`);
}

// ---------------------------------------------------------------- 混音

console.log("渲染 DX7 其余通道 + 混音…");
const mixL = new Float32Array(frames);
const mixR = new Float32Array(frames);
renderMsfa(makeMsfa(otherChannels), otherChannels, mixL, mixR, false);
let peak = 0;
for (let i = 0; i < frames; i++) {
  mixL[i] += vemL[i] * vemGain;
  mixR[i] += vemR[i] * vemGain;
  const a = Math.max(Math.abs(mixL[i]), Math.abs(mixR[i]));
  if (a > peak) peak = a;
}

writeFloatWav(outPath, mixL, mixR, SR);
console.log(`写出 ${outPath}`);
console.log(
  `  长度 ${dur.toFixed(2)}s | peak ${peak.toFixed(4)} (${(20 * Math.log10(peak || 1e-9)).toFixed(2)} dBFS) | rms ${rms(mixL, mixR).toFixed(5)}`
);

// ---------------------------------------------------------------- WAV

function writeFloatWav(path, l, r, sr) {
  writeFloatWavBuffer(path, l, r, sr);
}

function writeFloatWavBuffer(path, l, r, sr) {
  const frames = Math.min(l.length, r.length);
  const buf = Buffer.alloc(44 + frames * 8);
  buf.write("RIFF", 0);
  buf.writeUInt32LE(36 + frames * 8, 4);
  buf.write("WAVE", 8);
  buf.write("fmt ", 12);
  buf.writeUInt32LE(16, 16);
  buf.writeUInt16LE(3, 20); // float
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
