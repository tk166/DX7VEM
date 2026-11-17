// VEM 单声部 CPU 实测（Node/wasm，48 kHz）
//
// 用法: node scripts/vem-cpu.mjs [--seconds 60] [--data out/vem-data]
//
// 输出：渲染 seconds 秒音频的墙钟时间 → 实时占比；以及单声部（1 voice）的估算。

import { readFileSync, existsSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : d; };

const SR = 48000;
const BLOCK = 64;
const seconds = Number(opt("--seconds", 60));
const dataDir = opt("--data", join(root, "out/vem-data"));
const noBody = argv.includes("--no-body");
const noNoise = argv.includes("--no-noise");
const ovMode = Number(opt("--oversample", 0)); // 0 自动 1 开 2 关

const wasm = readFileSync(join(root, "vem/wasm/vem.wasm"));
const { instance } = await WebAssembly.instantiate(wasm, {});
const E = instance.exports;
if (E._initialize) E._initialize();

const eng = E.vem_new(SR, 0);
E.vem_set_seed(eng, 12345);
E.vem_set_gain(eng, 1);
E.vem_set_master(eng, 1);
E.vem_set_envelope(eng, 8, 60, 60, 120, 208.33, 120, 60);
E.vem_set_oversample(eng, ovMode);
if (noBody) E.vem_set_body(eng, 0, 0.6);
if (noNoise) E.vem_set_bow_noise(eng, 0, 0.5);

let dataFiles = 0;
for (const name of ["body-violin.bin", "noise.bin"]) {
  const p = join(dataDir, name);
  if (!existsSync(p)) continue;
  const bytes = new Uint8Array(readFileSync(p));
  const ptr = E.vem_alloc(bytes.length);
  new Uint8Array(E.memory.buffer, ptr, bytes.length).set(bytes);
  E.vem_set_patch(eng, ptr, bytes.length);
  E.vem_free_mem(ptr, bytes.length);
  dataFiles++;
}

// 用 notes.bin 的 CH3 事件做真实密度
const nb = readFileSync(join(root, "web/data/notes.bin"));
const n = nb.readUInt32LE(4);
const ev = [];
for (let i = 0; i < n; i++) {
  const o = 12 + i * 8;
  if (nb.readUInt8(o + 4) !== 2) continue;
  ev.push({ t: nb.readFloatLE(o), note: nb.readUInt8(o + 5), vel: nb.readUInt8(o + 6), on: nb.readUInt8(o + 7) !== 0 });
}
ev.sort((a, b) => a.t - b.t);

const outL = E.vem_alloc(BLOCK * 4);
const outR = E.vem_alloc(BLOCK * 4);
const frames = Math.ceil(seconds * SR);

let ei = 0;
let peak = 0;
const t0 = process.hrtime.bigint();
for (let pos = 0; pos < frames; pos += BLOCK) {
  const nowT = pos / SR;
  while (ei < ev.length && ev[ei].t <= nowT) {
    const e = ev[ei++];
    if (e.on) E.vem_note_on(eng, e.note, e.vel / 127);
    else E.vem_note_off(eng, e.note);
  }
  E.vem_render(eng, outL, outR, BLOCK);
  if (pos % (SR * 1) === 0) {
    const l = new Float32Array(E.memory.buffer, outL, BLOCK);
    for (let i = 0; i < BLOCK; i++) peak = Math.max(peak, Math.abs(l[i]));
  }
}
const elapsed = Number(process.hrtime.bigint() - t0) / 1e9;

console.log(`VEM 单声部：渲染 ${seconds}s @${SR}Hz，数据 ${dataFiles} 个文件`);
console.log(`  wall ${elapsed.toFixed(2)}s -> ${((elapsed / seconds) * 100).toFixed(2)}% realtime (1 voice, body+noise)`);
console.log(`  peak ${peak.toFixed(4)}`);
console.log(`  RESULT realtime_pct=${((elapsed / seconds) * 100).toFixed(2)} target=3.0 ${elapsed / seconds <= 0.03 ? "PASS" : "FAIL"}`);
