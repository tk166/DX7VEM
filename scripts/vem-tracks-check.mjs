// 每条 VEM 声部 vs 对应 DX7 通道 stem 的对齐检查（活动一致 / 时移 / 相对电平）
//
// 用法:
//   node scripts/render-mix.mjs out/p03-vem-mix.wav --stems --write-config
//   node scripts/vem-tracks-check.mjs [--gate]
//
// 说明：音色不同，绝对包络相关没有意义；这里看的是
//   - 活动一致（50 ms 帧、相对峰值 -40 dB 判"有声"）
//   - 最佳时移
//   - 校准前的 RMS 比（混音时用它的倒数做电平对齐）

import { readFileSync, existsSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : d; };
const gate = argv.includes("--gate");
const stemDir = opt("--stems", join(root, "out/m4-stems"));
const configPath = opt("--vem-tracks", join(root, "web/data/vem-tracks.json"));
const cfg = JSON.parse(readFileSync(configPath, "utf8"));

function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, fmt = null, dataOff = -1, dataLen = 0;
  while (off + 8 <= b.length) {
    const id = b.toString("ascii", off, off + 4);
    const len = b.readUInt32LE(off + 4);
    if (id === "fmt ")
      fmt = { channels: b.readUInt16LE(off + 10), sampleRate: b.readUInt32LE(off + 12), bits: b.readUInt16LE(off + 22) };
    if (id === "data") { dataOff = off + 8; dataLen = len; break; }
    off += 8 + len + (len & 1);
  }
  return { buf: b, ...fmt, dataOff, bps: fmt.bits / 8, frames: Math.floor(dataLen / ((fmt.bits / 8) * fmt.channels)) };
}
function mono(w) {
  const out = new Float64Array(w.frames);
  for (let i = 0; i < w.frames; i++) {
    const base = w.dataOff + i * w.bps * w.channels;
    let v = 0;
    for (let c = 0; c < w.channels; c++) {
      const o = base + c * w.bps;
      if (w.bits === 32) v += w.buf.readFloatLE(o);
      else if (w.bits === 16) v += w.buf.readInt16LE(o) / 32768;
      else {
        let x = w.buf[o] | (w.buf[o + 1] << 8) | (w.buf[o + 2] << 16);
        if (x & 0x800000) x -= 0x1000000;
        v += x / 8388608;
      }
    }
    out[i] = v / w.channels;
  }
  return out;
}
const rms = (a) => Math.sqrt(a.reduce((s, v) => s + v * v, 0) / Math.max(1, a.length));
function envelope(x, sr, ms) {
  const w = Math.max(1, Math.round((ms / 1000) * sr));
  const n = Math.floor(x.length / w);
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    let s = 0;
    for (let j = 0; j < w; j++) s += x[i * w + j] ** 2;
    out[i] = Math.sqrt(s / w);
  }
  return out;
}
function agreement(a, b, db = -40) {
  let peak = 0;
  for (const v of a) peak = Math.max(peak, v);
  const th = peak * Math.pow(10, db / 20);
  let both = 0, onlyA = 0, onlyB = 0;
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    const A = a[i] > th, B = b[i] > th;
    if (A && B) both++;
    else if (A) onlyA++;
    else if (B) onlyB++;
  }
  return { jaccard: both / Math.max(1, both + onlyA + onlyB), recall: both / Math.max(1, both + onlyA), missing: onlyA, extra: onlyB };
}
function bestLag(a, b, maxFrames) {
  let best = -Infinity, bestLag = 0;
  for (let lag = -maxFrames; lag <= maxFrames; lag++) {
    const i0 = Math.max(0, -lag);
    const i1 = Math.min(a.length, b.length - lag);
    const n = i1 - i0;
    if (n < 50) continue;
    let ma = 0, mb = 0;
    for (let i = i0; i < i1; i++) { ma += a[i]; mb += b[i + lag]; }
    ma /= n; mb /= n;
    let num = 0, da = 0, db = 0;
    for (let i = i0; i < i1; i++) {
      const x = a[i] - ma, y = b[i + lag] - mb;
      num += x * y; da += x * x; db += y * y;
    }
    const c = num / Math.sqrt(da * db || 1);
    if (c > best) { best = c; bestLag = lag; }
  }
  return { corr: best, lag: bestLag };
}

const channels = [...new Set(cfg.map((c) => c.channel))].sort((a, b) => a - b);
const rows = [];
const fails = [];
for (const ch of channels) {
  const group = cfg.filter((c) => c.channel === ch);
  const a = mono(loadWav(join(stemDir, `dx7-ch${ch + 1}.wav`)));
  const sum = new Float64Array(a.length);
  for (const entry of group) {
    const f = join(stemDir, `vem-${entry.instrument}-ch${ch + 1}.wav`);
    if (!existsSync(f)) { fails.push(`缺少 ${f}`); continue; }
    const b = mono(loadWav(f));
    const n = Math.min(sum.length, b.length);
    for (let i = 0; i < n; i++) sum[i] += b[i];
  }
  const ratio = rms(sum) / Math.max(1e-9, rms(a));
  const act = agreement(envelope(a, 44100, 50), envelope(sum, 44100, 50), -40);
  const lag = bestLag(envelope(a, 44100, 10), envelope(sum, 44100, 10), 200);
  rows.push({
    channel: `CH${ch + 1}`,
    voices: group.map((g) => g.instrument).join("+"),
    rawRmsRatio: ratio.toFixed(3),
    jaccard: act.jaccard.toFixed(3),
    recall: act.recall.toFixed(3),
    lagMs: (lag.lag * 10).toFixed(0),
  });
  if (act.recall < 0.95) fails.push(`CH${ch + 1} 召回 ${act.recall.toFixed(3)} < 0.95（有 DX7 有声音而我们没覆盖的帧）`);
}

console.log("通道 | VEM 声部 | 校准前 RMS 比 | 活动一致 | 召回 | 时移（仅供参考）");
for (const r of rows) {
  console.log(`${r.channel} | ${r.voices} | ${r.rawRmsRatio} | ${r.jaccard} | ${r.recall} | ${r.lagMs} ms`);
}
if (gate) {
  console.log(fails.length ? `\nFAIL:\n  - ${fails.join("\n  - ")}` : "\nPASS ✅");
  if (fails.length) process.exit(1);
}
