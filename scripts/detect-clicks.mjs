// 检测 WAV 里的咔哒声/削顶：逐样本跳变 + 饱和样本统计
// 用法: node detect-clicks.mjs <wav> [起始秒] [结束秒] [跳变阈值系数=8]
import { readFileSync } from "fs";

function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, dataOff = -1, dataLen = 0, fmt = null;
  while (off + 8 <= b.length) {
    const id = b.toString("ascii", off, off + 4), len = b.readUInt32LE(off + 4);
    if (id === "fmt ") fmt = { channels: b.readUInt16LE(off + 10), sampleRate: b.readUInt32LE(off + 12), bits: b.readUInt16LE(off + 22) };
    if (id === "data") { dataOff = off + 8; dataLen = len; break; }
    off += 8 + len + (len & 1);
  }
  return { buf: b, ...fmt, dataOff, bps: fmt.bits / 8, frames: Math.floor(dataLen / (fmt.bits / 8 * fmt.channels)) };
}
function smp(buf, bits, o) {
  if (bits === 24) { let x = buf[o] | (buf[o + 1] << 8) | (buf[o + 2] << 16); if (x & 0x800000) x -= 0x1000000; return x / 8388608; }
  if (bits === 16) return buf.readInt16LE(o) / 32768;
  if (bits === 32) return buf.readFloatLE(o);
  return 0;
}
function channel(w, c) {
  const out = new Float32Array(w.frames);
  for (let f = 0; f < w.frames; f++) out[f] = smp(w.buf, w.bits, w.dataOff + f * w.bps * w.channels + c * w.bps);
  return out;
}

const [path, sArg, eArg, kArg] = process.argv.slice(2);
const K = Number(kArg ?? 8);
const w = loadWav(path);
const chans = [];
for (let c = 0; c < w.channels; c++) chans.push(channel(w, c));
const dur = w.frames / w.sampleRate;
const s0 = Math.max(0, Number(sArg ?? 0)), e0 = Math.min(dur, Number(eArg ?? dur));
console.log(`${path}: ${w.bits}bit ${w.channels}ch ${dur.toFixed(1)}s | 分析 ${s0}~${e0}s | 跳变阈值 ${K}×局部RMS`);

let clipCount = 0, clipRuns = [], runStart = -1;
const clicks = [];
const frame = Math.round(0.005 * w.sampleRate);
for (const d of chans) {
  for (let i = 1; i < d.length; i++) {
    const t = i / w.sampleRate;
    const a = Math.abs(d[i]);
    if (a >= 0.9995) { clipCount++; if (runStart < 0) runStart = t; }
    else if (runStart >= 0) { if (t - runStart >= 0.0005) clipRuns.push([runStart, t]); runStart = -1; }
  }
}
console.log(`饱和样本(|x|>=0.9995): ${clipCount}`);
const showRuns = clipRuns.filter(([a, b]) => b >= s0 && a <= e0).slice(0, 20);
if (showRuns.length) console.log("削顶段:", showRuns.map(([a, b]) => `${a.toFixed(3)}-${b.toFixed(3)}s`).join(", "));

// 局部 RMS 门限找跳变
for (let c = 0; c < chans.length; c++) {
  const d = chans[c];
  for (let i = frame; i < d.length - frame; i++) {
    const t = i / w.sampleRate;
    if (t < s0 || t > e0) continue;
    const jump = Math.abs(d[i] - d[i - 1]);
    let sq = 0;
    for (let k = i - frame; k < i; k += 4) sq += d[k] * d[k];
    const rms = Math.sqrt(sq / (frame / 4)) + 1e-9;
    if (jump > K * rms && jump > 0.03) clicks.push({ t, ch: c, jump, rms });
  }
}
// 合并邻近（<5ms）为一次事件
const merged = [];
for (const c of clicks) {
  const last = merged.at(-1);
  if (last && c.t - last.t < 0.005 && c.ch === last.ch) { last.jump = Math.max(last.jump, c.jump); continue; }
  merged.push({ ...c });
}
console.log(`跳变事件: ${merged.length} 次`);
for (const m of merged.slice(0, 40)) console.log(`  ${m.t.toFixed(3)}s ch${m.ch} 跳变 ${m.jump.toFixed(3)}（局部RMS ${m.rms.toFixed(3)}，${(m.jump / m.rms).toFixed(1)}×）`);
