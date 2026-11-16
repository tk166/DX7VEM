// VEM 验收指标（M1 门槛）：基频误差 / 频谱余弦（线性 + 1/3 倍频程）/ 包络相关 / 电平差
//
// 用法:
//   node scripts/vem-metrics.mjs <ref.wav> <ours.wav> [--from 0.5] [--to 3.5] [--note 69] [--gate]
//
// 说明:
//   - 20 ms RMS 包络相关对齐"同音同力度"的两个渲染；
//   - 基频用归一化自相关 + 抛物线插值（比整数 lag 精确两个数量级）；
//   - 频谱余弦对"多帧平均幅度谱"计算，避免单帧快照对起音相位过敏。
import { readFileSync } from "fs";

function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, fmt = null, dataOff = -1, dataLen = 0;
  while (off + 8 <= b.length) {
    const id = b.toString("ascii", off, off + 4);
    const len = b.readUInt32LE(off + 4);
    if (id === "fmt ")
      fmt = {
        tag: b.readUInt16LE(off + 8),
        channels: b.readUInt16LE(off + 10),
        sampleRate: b.readUInt32LE(off + 12),
        bits: b.readUInt16LE(off + 22),
      };
    if (id === "data") {
      dataOff = off + 8;
      dataLen = len;
      break;
    }
    off += 8 + len + (len & 1);
  }
  if (!fmt || dataOff < 0) throw new Error(`无法解析 WAV: ${path}`);
  return {
    buf: b,
    ...fmt,
    dataOff,
    bps: fmt.bits / 8,
    frames: Math.floor(dataLen / ((fmt.bits / 8) * fmt.channels)),
  };
}

function sample(buf, bits, o) {
  if (bits === 24) {
    let x = buf[o] | (buf[o + 1] << 8) | (buf[o + 2] << 16);
    if (x & 0x800000) x -= 0x1000000;
    return x / 8388608;
  }
  if (bits === 16) return buf.readInt16LE(o) / 32768;
  if (bits === 32) return buf.readFloatLE(o);
  throw new Error(`不支持的位深 ${bits}`);
}

// 取单声道切片（多声道取平均）
function mono(w, startSec, durSec) {
  const s0 = Math.max(0, Math.round(startSec * w.sampleRate));
  const n = Math.max(0, Math.min(w.frames - s0, Math.round(durSec * w.sampleRate)));
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    const base = w.dataOff + (s0 + i) * w.bps * w.channels;
    let v = 0;
    for (let c = 0; c < w.channels; c++) v += sample(w.buf, w.bits, base + c * w.bps);
    out[i] = v / w.channels;
  }
  return out;
}

function rms(x) {
  let s = 0;
  for (let i = 0; i < x.length; i++) s += x[i] * x[i];
  return Math.sqrt(s / Math.max(1, x.length));
}

function db(x) {
  return 20 * Math.log10(Math.max(1e-12, x));
}

// 基频：归一化自相关 + 抛物线插值
function detectF0(x, sr, fmin = 20, fmax = 4000) {
  const n = x.length;
  let mean = 0;
  for (const v of x) mean += v;
  mean /= n;
  const y = new Float64Array(n);
  let energy = 0;
  for (let i = 0; i < n; i++) {
    y[i] = x[i] - mean;
    energy += y[i] * y[i];
  }
  if (energy < 1e-12) return null;
  const lagMin = Math.max(2, Math.floor(sr / fmax));
  const lagMax = Math.min(n - 1, Math.floor(sr / fmin));
  const acf = new Float64Array(lagMax + 1);
  // 只用前 2/3 的长度算 ACF，减少窗口边缘的偏差
  const m = Math.floor(n * 0.66);
  for (let lag = lagMin; lag <= lagMax; lag++) {
    let s = 0;
    for (let i = 0; i + lag < m; i++) s += y[i] * y[i + lag];
    acf[lag] = s / (m - lagMin);
  }
  // 1) 全局最大峰；2) 再取“第一个 >= 85% 全局峰”的局部峰，避免倍周期（低八度）误判
  let globalMax = -Infinity, globalLag = -1;
  for (let lag = lagMin + 1; lag < lagMax; lag++) {
    if (acf[lag] > acf[lag - 1] && acf[lag] >= acf[lag + 1] && acf[lag] > globalMax) {
      globalMax = acf[lag];
      globalLag = lag;
    }
  }
  if (globalLag < 0) return null;
  let bestLag = globalLag;
  for (let lag = lagMin + 1; lag < lagMax; lag++) {
    if (acf[lag] > acf[lag - 1] && acf[lag] >= acf[lag + 1] && acf[lag] >= 0.85 * globalMax) {
      bestLag = lag;
      break;
    }
  }
  // 抛物插值
  const a = acf[bestLag - 1], b = acf[bestLag], c = acf[bestLag + 1];
  const denom = a - 2 * b + c;
  const delta = denom !== 0 ? (0.5 * (a - c)) / denom : 0;
  const lag = bestLag + Math.max(-0.5, Math.min(0.5, delta));
  return sr / lag;
}

// 迭代基 2 FFT（原地）
function fft(re, im) {
  const n = re.length;
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) {
      [re[i], re[j]] = [re[j], re[i]];
      [im[i], im[j]] = [im[j], im[i]];
    }
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
        re[i + k] = ur + vr;
        im[i + k] = ui + vi;
        re[i + k + len / 2] = ur - vr;
        im[i + k + len / 2] = ui - vi;
        const ncr = cr * wr - ci * wi;
        ci = cr * wi + ci * wr;
        cr = ncr;
      }
    }
  }
}

// 多帧平均幅度谱
function avgMagnitude(x, N = 8192, maxFrames = 32) {
  const mag = new Float64Array(N / 2 + 1);
  if (x.length < N) return mag;
  const step = Math.max(1, Math.floor((x.length - N) / maxFrames));
  let count = 0;
  for (let p = 0; p + N <= x.length; p += step) {
    const re = new Float64Array(N), im = new Float64Array(N);
    for (let i = 0; i < N; i++) {
      const w = 0.5 * (1 - Math.cos((2 * Math.PI * i) / (N - 1)));
      re[i] = x[p + i] * w;
    }
    fft(re, im);
    for (let k = 1; k <= N / 2; k++) mag[k] += Math.hypot(re[k], im[k]);
    count++;
    if (count >= maxFrames) break;
  }
  if (count) for (let k = 0; k < mag.length; k++) mag[k] /= count;
  return mag;
}

function cosine(a, b) {
  let d = 0, na = 0, nb = 0;
  for (let k = 2; k < Math.min(a.length, b.length); k++) {
    d += a[k] * b[k];
    na += a[k] * a[k];
    nb += b[k] * b[k];
  }
  return d / Math.sqrt(na * nb || 1);
}

// 1/3 倍频程（20 Hz … 20 kHz）
function thirdOctave(mag, sr, N) {
  const bands = [];
  for (let b = -17; b <= 13; b++) {
    const center = 1000 * Math.pow(2, b / 3);
    const lo = center / Math.pow(2, 1 / 6);
    const hi = center * Math.pow(2, 1 / 6);
    let e = 0;
    for (let k = 1; k <= N / 2; k++) {
      const f = (k * sr) / N;
      if (f >= lo && f < hi) e += mag[k] * mag[k];
    }
    bands.push(Math.sqrt(e));
  }
  return bands;
}

// 20 ms RMS 包络
function envelope(x, sr, winMs = 20) {
  const w = Math.max(1, Math.round((winMs / 1000) * sr));
  const n = Math.floor(x.length / w);
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    let s = 0;
    for (let j = 0; j < w; j++) {
      const v = x[i * w + j];
      s += v * v;
    }
    out[i] = Math.sqrt(s / w);
  }
  return out;
}

function pearson(a, b) {
  const n = Math.min(a.length, b.length);
  if (n < 2) return 0;
  let ma = 0, mb = 0;
  for (let i = 0; i < n; i++) {
    ma += a[i];
    mb += b[i];
  }
  ma /= n;
  mb /= n;
  let num = 0, da = 0, db = 0;
  for (let i = 0; i < n; i++) {
    const x = a[i] - ma, y = b[i] - mb;
    num += x * y;
    da += x * x;
    db += y * y;
  }
  return num / Math.sqrt(da * db || 1);
}

// ---------------- 参数 ----------------
const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 && argv[i + 1] !== undefined ? Number(argv[i + 1]) : def;
};
const [refPath, ourPath] = argv.filter((a) => a.endsWith(".wav"));
if (!refPath || !ourPath) {
  console.error("用法: node vem-metrics.mjs <ref.wav> <ours.wav> [--from 0.5] [--to 3.5] [--note 69] [--gate]");
  process.exit(2);
}

const REF = loadWav(refPath);
const OUR = loadWav(ourPath);
const totalSec = Math.min(REF.frames / REF.sampleRate, OUR.frames / OUR.sampleRate);
const from = opt("from", Math.min(0.5, totalSec * 0.1));
const to = opt("to", totalSec);
const dur = Math.max(0.05, to - from);
const note = argv.includes("--note") ? opt("note", 69) : null;
const gate = argv.includes("--gate");

const refMono = mono(REF, from, dur);
const ourMono = mono(OUR, from, dur);

const sr = REF.sampleRate;
const rmsRef = rms(refMono);
const rmsOur = rms(ourMono);
const dbDiff = db(rmsOur) - db(rmsRef);

const f0Ref = detectF0(refMono, sr);
const f0Our = detectF0(ourMono, sr);
let centErr = null;
if (f0Ref && f0Our) centErr = 1200 * Math.log2(f0Our / f0Ref);
let noteErr = null;
if (note !== null && f0Our) {
  const expect = 440 * Math.pow(2, (note - 69) / 12);
  noteErr = 1200 * Math.log2(f0Our / expect);
}

const N = 8192;
const magRef = avgMagnitude(refMono, N);
const magOur = avgMagnitude(ourMono, N);
const cosLin = cosine(magRef, magOur);
const cosThird = cosine(thirdOctave(magRef, sr, N), thirdOctave(magOur, sr, N));

const envRef = envelope(refMono, sr, 20);
const envOur = envelope(ourMono, sr, 20);
const envCorr = pearson(envRef, envOur);
// 最大逐帧电平差（dB）
let envMaxDiff = 0;
for (let i = 0; i < Math.min(envRef.length, envOur.length); i++) {
  if (envRef[i] > 1e-5 || envOur[i] > 1e-5) {
    envMaxDiff = Math.max(envMaxDiff, Math.abs(db(envOur[i]) - db(envRef[i])));
  }
}

// 逐采样差（同种子同参数时应当极小）
let diffPeak = 0;
let diffSum = 0;
const nDiff = Math.min(refMono.length, ourMono.length);
for (let i = 0; i < nDiff; i++) {
  const d = ourMono[i] - refMono[i];
  if (Math.abs(d) > diffPeak) diffPeak = Math.abs(d);
  diffSum += d * d;
}
const diffRms = Math.sqrt(diffSum / Math.max(1, nDiff));
const snrDb = diffRms > 0 ? db(rmsRef / diffRms) : Infinity;

const lines = [];
lines.push(`窗口: ${from.toFixed(2)}s → ${(from + dur).toFixed(2)}s（${dur.toFixed(2)}s，${sr} Hz）`);
lines.push(`长度: ref ${(REF.frames / REF.sampleRate).toFixed(3)}s | ours ${(OUR.frames / OUR.sampleRate).toFixed(3)}s`);
lines.push(
  `基频: ref ${f0Ref ? f0Ref.toFixed(2) : "—"} Hz | ours ${f0Our ? f0Our.toFixed(2) : "—"} Hz` +
    (centErr !== null ? ` | 差 ${centErr >= 0 ? "+" : ""}${centErr.toFixed(2)} cent` : "") +
    (noteErr !== null ? ` | 相对 note ${note} ${noteErr >= 0 ? "+" : ""}${noteErr.toFixed(2)} cent` : "")
);
lines.push(
  `电平: ref ${db(rmsRef).toFixed(2)} dB | ours ${db(rmsOur).toFixed(2)} dB | 差 ${dbDiff >= 0 ? "+" : ""}${dbDiff.toFixed(2)} dB`
);
lines.push(`频谱余弦(线性)   : ${cosLin.toFixed(4)}`);
lines.push(`频谱余弦(1/3 oct): ${cosThird.toFixed(4)}`);
lines.push(`包络相关(20 ms)  : ${envCorr.toFixed(4)} | 逐帧最大差 ${envMaxDiff.toFixed(1)} dB`);
lines.push(
  `逐采样差         : 峰值 ${diffPeak.toExponential(2)} | RMS ${diffRms.toExponential(2)} | SNR ${Number.isFinite(snrDb) ? snrDb.toFixed(1) + " dB" : "∞"}`
);

const fails = [];
if (Math.abs(centErr ?? 999) > 1) fails.push(`基频误差 ${centErr?.toFixed(2)} cent > 1`);
if (Math.abs(dbDiff) > 1) fails.push(`电平差 ${dbDiff.toFixed(2)} dB > ±1`);
if (cosLin < 0.99) fails.push(`线性谱余弦 ${cosLin.toFixed(4)} < 0.99`);
if (cosThird < 0.98) fails.push(`1/3 倍频程余弦 ${cosThird.toFixed(4)} < 0.98`);
if (envCorr <= 0.98) fails.push(`包络相关 ${envCorr.toFixed(4)} ≤ 0.98`);

if (gate) {
  lines.push("");
  lines.push(fails.length === 0 ? "结论: PASS ✅" : `结论: FAIL ❌\n  - ${fails.join("\n  - ")}`);
}
console.log(lines.join("\n"));
if (gate && fails.length) process.exit(1);
