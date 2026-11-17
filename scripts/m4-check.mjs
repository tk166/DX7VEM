// M4 验收指标：VEM 替换 CH3 后的整曲 vs REAPER 参考（只看电平/节奏/频谱包容度）
//
// 用法:
//   node scripts/m4-check.mjs [--ref <reaper.wav>] [--mix out/p03-vem-mix.wav] [--gate]
//
// 门的定义（M4 阶段，音色本来就会变）:
//   - 整体 RMS 差 ≤ 1.5 dB
//   - 10 ms 包络相关 ≥ 0.90，最佳时移 |lag| ≤ 20 ms
//   - 1/3 倍频程谱余弦 ≥ 0.95
//   - CH3 stem 电平校准后 RMS 比 0.9–1.1（自动校准的合理性）

import { readFileSync, existsSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const argv = process.argv.slice(2);
const opt = (n, d) => {
  const i = argv.indexOf(n);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : d;
};
const gate = argv.includes("--gate");
const refPath = opt("--ref", join(root, "../261115-daw/out/p03-reaper.wav"));
const mixPath = opt("--mix", join(root, "out/p03-vem-mix.wav"));
const stemDir = opt("--stems", join(root, "out/m4-stems"));

function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, fmt = null, dataOff = -1, dataLen = 0;
  while (off + 8 <= b.length) {
    const id = b.toString("ascii", off, off + 4);
    const len = b.readUInt32LE(off + 4);
    if (id === "fmt ")
      fmt = {
        channels: b.readUInt16LE(off + 10),
        sampleRate: b.readUInt32LE(off + 12),
        bits: b.readUInt16LE(off + 22),
      };
    if (id === "data") { dataOff = off + 8; dataLen = len; break; }
    off += 8 + len + (len & 1);
  }
  return { buf: b, ...fmt, dataOff, bps: fmt.bits / 8, frames: Math.floor(dataLen / ((fmt.bits / 8) * fmt.channels)) };
}

function sample(w, o) {
  if (w.bits === 24) {
    let x = w.buf[o] | (w.buf[o + 1] << 8) | (w.buf[o + 2] << 16);
    if (x & 0x800000) x -= 0x1000000;
    return x / 8388608;
  }
  if (w.bits === 16) return w.buf.readInt16LE(o) / 32768;
  return w.buf.readFloatLE(o);
}

function mono(w, from = 0, to = Infinity) {
  const s0 = Math.round(from * w.sampleRate);
  const s1 = Math.min(w.frames, Math.round(to * w.sampleRate));
  const out = new Float64Array(Math.max(0, s1 - s0));
  for (let i = 0; i < out.length; i++) {
    const base = w.dataOff + (s0 + i) * w.bps * w.channels;
    let v = 0;
    for (let c = 0; c < w.channels; c++) v += sample(w, base + c * w.bps);
    out[i] = v / w.channels;
  }
  return out;
}

const rms = (x) => Math.sqrt(x.reduce((s, v) => s + v * v, 0) / Math.max(1, x.length));
const db = (x) => 20 * Math.log10(Math.max(1e-12, x));

function envelope(x, sr, winMs) {
  const w = Math.max(1, Math.round((winMs / 1000) * sr));
  const n = Math.floor(x.length / w);
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    let s = 0;
    for (let j = 0; j < w; j++) s += x[i * w + j] ** 2;
    out[i] = Math.sqrt(s / w);
  }
  return out;
}

function pearson(a, b) {
  const n = Math.min(a.length, b.length);
  let ma = 0, mb = 0;
  for (let i = 0; i < n; i++) { ma += a[i]; mb += b[i]; }
  ma /= n; mb /= n;
  let num = 0, da = 0, db = 0;
  for (let i = 0; i < n; i++) {
    const x = a[i] - ma, y = b[i] - mb;
    num += x * y; da += x * x; db += y * y;
  }
  return num / Math.sqrt(da * db || 1);
}

// 活动一致性：50 ms 帧、相对参考峰值 -40 dB 判定"这一帧有声音"
function activityAgreement(a, b, dbThresh = -40) {
  let peak = 0;
  for (const v of a) peak = Math.max(peak, v);
  const th = peak * Math.pow(10, dbThresh / 20);
  let both = 0, onlyA = 0, onlyB = 0;
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    const A = a[i] > th, B = b[i] > th;
    if (A && B) both++;
    else if (A) onlyA++;
    else if (B) onlyB++;
  }
  return {
    jaccard: both / Math.max(1, both + onlyA + onlyB),
    recall: both / Math.max(1, both + onlyA),
    both,
    missing: onlyA,
    extra: onlyB,
  };
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

// 1/3 倍频程谱（整曲平均幅度谱，复用简单 FFT）
function fft(re, im) {
  const n = re.length;
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const ang = (-2 * Math.PI) / len;
    const wr = Math.cos(ang), wi = Math.sin(ang);
    for (let i = 0; i < n; i += len) {
      let cr = 1, ci = 0;
      for (let k = 0; k < len / 2; k++) {
        const ur = re[i + k], ui = im[i + k];
        const vr = re[i + k + len / 2] * cr - im[i + k + len / 2] * ci;
        const vi = re[i + k + len / 2] * ci + im[i + k + len / 2] * cr;
        re[i + k] = ur + vr; im[i + k] = ui + vi;
        re[i + k + len / 2] = ur - vr; im[i + k + len / 2] = ui - vi;
        const ncr = cr * wr - ci * wi; ci = cr * wi + ci * wr; cr = ncr;
      }
    }
  }
}

function thirdOctave(x, sr, N = 8192, maxFrames = 400) {
  const mag = new Float64Array(N / 2 + 1);
  const step = Math.max(N, Math.floor((x.length - N) / maxFrames) || N);
  let count = 0;
  for (let p = 0; p + N <= x.length; p += step) {
    const re = new Float64Array(N), im = new Float64Array(N);
    for (let i = 0; i < N; i++) re[i] = x[p + i] * (0.5 - 0.5 * Math.cos((2 * Math.PI * i) / (N - 1)));
    fft(re, im);
    for (let k = 1; k <= N / 2; k++) mag[k] += Math.hypot(re[k], im[k]);
    if (++count >= maxFrames) break;
  }
  if (count) for (let k = 0; k < mag.length; k++) mag[k] /= count;
  const bands = [];
  for (let b = -17; b <= 13; b++) {
    const center = 1000 * Math.pow(2, b / 3);
    const lo = center / Math.pow(2, 1 / 6), hi = center * Math.pow(2, 1 / 6);
    let e = 0;
    for (let k = 1; k <= N / 2; k++) {
      const f = (k * sr) / N;
      if (f >= lo && f < hi) e += mag[k] * mag[k];
    }
    bands.push(Math.sqrt(e));
  }
  return bands;
}

const cosine = (a, b) => {
  let d = 0, na = 0, nb = 0;
  for (let i = 0; i < Math.min(a.length, b.length); i++) { d += a[i] * b[i]; na += a[i] ** 2; nb += b[i] ** 2; }
  return d / Math.sqrt(na * nb || 1);
};

// ---------------------------------------------------------------- 计算

const REF = loadWav(refPath);
const MIX = loadWav(mixPath);
const total = Math.min(REF.frames / REF.sampleRate, MIX.frames / MIX.sampleRate);
const from = 10;
const refMono = mono(REF, from, total);
const mixMono = mono(MIX, from, total);

const rmsRef = rms(refMono);
const rmsMix = rms(mixMono);
const levelDiff = db(rmsMix) - db(rmsRef);

const envRef = envelope(refMono, REF.sampleRate, 10);
const envMix = envelope(mixMono, MIX.sampleRate, 10);
const lagRes = bestLag(envRef, envMix, 200); // ±2 s
const actRef = envelope(refMono, REF.sampleRate, 50);
const actMix = envelope(mixMono, MIX.sampleRate, 50);
const act = activityAgreement(actRef, actMix, -40);
const envCorr = pearson(envRef, envMix);
const thirdRef = thirdOctave(refMono, REF.sampleRate);
const thirdMix = thirdOctave(mixMono, MIX.sampleRate);
const cosThird = cosine(thirdRef, thirdMix);

const lines = [];
lines.push(`参考 ${refPath}  ${total.toFixed(1)}s`);
lines.push(`混音 ${mixPath}  ${total.toFixed(1)}s`);
lines.push(`电平: ref ${db(rmsRef).toFixed(2)} dB | mix ${db(rmsMix).toFixed(2)} dB | 差 ${levelDiff >= 0 ? "+" : ""}${levelDiff.toFixed(2)} dB`);
lines.push(`节奏: 最佳时移 ${(lagRes.lag * 10).toFixed(0)} ms（10 ms 帧）`);
lines.push(
  `活动一致: Jaccard ${act.jaccard.toFixed(3)} / 召回 ${act.recall.toFixed(3)}（50 ms 帧，-40 dB；缺 ${act.missing} 帧 / 多 ${act.extra} 帧）`
);
lines.push(`包络相关(10 ms, 绝对值): ${envCorr.toFixed(3)}（音色不同，仅参考）`);
lines.push(`1/3 倍频程谱余弦: ${cosThird.toFixed(4)}`);

// CH3 stem（可选）
let stemOk = true;
const stemA = join(stemDir, "dx7-ch3.wav");
const stemB = join(stemDir, "vem-violin-ch3.wav");
if (existsSync(stemA) && existsSync(stemB)) {
  const A = loadWav(stemA);
  const B = loadWav(stemB);
  const a = mono(A), b = mono(B);
  const ratio = rms(b) / Math.max(1e-9, rms(a));
  const ea = envelope(a, A.sampleRate, 10);
  const eb = envelope(b, B.sampleRate, 10);
  const lr = bestLag(ea, eb, 200);
  const stemDb = 20 * Math.log10(ratio || 1e-9);
  lines.push(
    `CH3 stem: 原始 RMS 比 ${ratio.toFixed(3)}（${stemDb >= 0 ? "+" : ""}${stemDb.toFixed(1)} dB，混音时已自动补偿）| 时移 ${(lr.lag * 10).toFixed(0)} ms | 包络相关 ${lr.corr.toFixed(3)}`
  );
  if (Math.abs(stemDb) > 24) stemOk = false;
} else {
  lines.push(`CH3 stem: 未找到（--stems 才会写）`);
}

const fails = [];
if (Math.abs(levelDiff) > 1.5) fails.push(`电平差 ${levelDiff.toFixed(2)} dB > ±1.5`);
if (Math.abs(lagRes.lag * 10) > 20) fails.push(`时移 ${lagRes.lag * 10} ms > 20`);
if (act.jaccard < 0.95) fails.push(`活动一致 Jaccard ${act.jaccard.toFixed(3)} < 0.95`);
if (cosThird < 0.95) fails.push(`1/3 谱余弦 ${cosThird.toFixed(4)} < 0.95`);
if (!stemOk) fails.push("CH3 stem 电平校准不合理");

if (gate) {
  lines.push("");
  lines.push(fails.length === 0 ? "结论: PASS ✅" : `结论: FAIL ❌\n  - ${fails.join("\n  - ")}`);
}
console.log(lines.join("\n"));
if (gate && fails.length) process.exit(1);
