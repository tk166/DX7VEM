// 单块渲染耗时统计（判断是否会在 AudioWorklet 的渲染量子内爆表）
//
// 用法: node scripts/vem-blocktime.mjs [--seconds 20] [--sr 48000]
// 输出：mean / p99 / max（单位 ms），以及 128 采样量子的预算与余量。

import { readFileSync, existsSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : d; };
const SR = Number(opt("--sr", 48000));
const seconds = Number(opt("--seconds", 20));
const BLOCK = 64;
const dataDir = opt("--data", join(root, "out/vem-data"));

const wasm = readFileSync(join(root, "vem/wasm/vem.wasm"));
const { instance } = await WebAssembly.instantiate(wasm, {});
const E = instance.exports;
if (E._initialize) E._initialize();

const eng = E.vem_new(SR, 0);
E.vem_set_seed(eng, 12345);
E.vem_set_gain(eng, 1);
E.vem_set_master(eng, 1);
E.vem_set_envelope(eng, 8, 60, 60, 120, 208.33, 120, 60);
for (const name of ["body-violin.bin", "noise.bin"]) {
  const p = join(dataDir, name);
  if (!existsSync(p)) continue;
  const bytes = new Uint8Array(readFileSync(p));
  const ptr = E.vem_alloc(bytes.length);
  new Uint8Array(E.memory.buffer, ptr, bytes.length).set(bytes);
  E.vem_set_patch(eng, ptr, bytes.length);
  E.vem_free_mem(ptr, bytes.length);
}

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
const times = [];
for (let pos = 0; pos < frames; pos += BLOCK) {
  const nowT = pos / SR;
  while (ei < ev.length && ev[ei].t <= nowT) {
    const e = ev[ei++];
    if (e.on) E.vem_note_on(eng, e.note, e.vel / 127);
    else E.vem_note_off(eng, e.note);
  }
  const t0 = process.hrtime.bigint();
  E.vem_render(eng, outL, outR, BLOCK);
  times.push(Number(process.hrtime.bigint() - t0) / 1e6);
}
times.sort((a, b) => a - b);
const mean = times.reduce((a, b) => a + b, 0) / times.length;
const p99 = times[Math.floor(times.length * 0.99)];
const max = times[times.length - 1];
const quantumMs = (128 / SR) * 1000;
console.log(`blocks=${times.length} sr=${SR} 量子预算=${quantumMs.toFixed(3)}ms（128 采样）`);
console.log(`mean=${mean.toFixed(4)}ms p99=${p99.toFixed(4)}ms max=${max.toFixed(4)}ms`);
console.log(`余量: max 占预算 ${((max / quantumMs) * 100).toFixed(1)}% → ${max < quantumMs * 0.5 ? "PASS" : "注意"}`);
