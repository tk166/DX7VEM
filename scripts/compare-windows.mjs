// 整曲对拍（窗口平均频谱 + RMS），比 compare-p02 的单快照稳定得多
// 用法: node compare-windows.mjs <ref.wav> <wasm.wav> [窗口秒数=10] [步长=10]
import { readFileSync } from "fs";

function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, dataOff = -1, dataLen = 0, fmt = null;
  while (off + 8 <= b.length) { const id = b.toString("ascii", off, off + 4); const len = b.readUInt32LE(off + 4);
    if (id === "fmt ") fmt = { channels: b.readUInt16LE(off + 10), sampleRate: b.readUInt32LE(off + 12), bits: b.readUInt16LE(off + 22) };
    if (id === "data") { dataOff = off + 8; dataLen = len; break; } off += 8 + len + (len & 1); }
  return { buf: b, ...fmt, dataOff, dataLen, bps: fmt.bits / 8, frames: Math.floor(dataLen / (fmt.bits / 8 * fmt.channels)) };
}
function smp(buf, bits, o) { if (bits === 24) { let x = buf[o] | (buf[o+1]<<8) | (buf[o+2]<<16); if (x & 0x800000) x -= 0x1000000; return x/8388608; } if (bits === 16) return buf.readInt16LE(o)/32768; if (bits === 32) return buf.readFloatLE(o); return 0; }
function mono(w, tSec, n) { const s = Math.round(tSec * w.sampleRate), out = new Float64Array(n); for (let i = 0; i < n; i++) { const f = s + i; if (f < 0 || f >= w.frames) break; const base = w.dataOff + f * w.bps * w.channels; let v = 0; for (let c = 0; c < w.channels; c++) v += smp(w.buf, w.bits, base + c * w.bps); out[i] = v / w.channels; } return out; }
function fft(re, im) { const n = re.length; if (n <= 1) return; const h = n >> 1; const a=new Float64Array(h),b=new Float64Array(h),c=new Float64Array(h),d=new Float64Array(h); for (let i=0;i<h;i++){a[i]=re[2*i];b[i]=im[2*i];c[i]=re[2*i+1];d[i]=im[2*i+1];} fft(a,b); fft(c,d); for (let k=0;k<h;k++){const ang=(-2*Math.PI*k)/n,co=Math.cos(ang),si=Math.sin(ang);const tr=c[k]*co-d[k]*si,ti=c[k]*si+d[k]*co;re[k]=a[k]+tr;im[k]=b[k]+ti;re[k+h]=a[k]-tr;im[k+h]=b[k]-ti;} }
const N = 4096;
function features(w, startSec, durSec) {
  const win = mono(w, startSec, Math.round(durSec * w.sampleRate));
  let sum = 0; for (const v of win) sum += v * v;
  const rms = Math.sqrt(sum / Math.max(1, win.length));
  const mag = new Float64Array(N / 2);
  const step = Math.max(1024, Math.floor((win.length - N) / 16) || N);
  let count = 0;
  for (let p = 0; p + N <= win.length; p += step) {
    const re = new Float64Array(N), im = new Float64Array(N);
    for (let i = 0; i < N; i++) re[i] = win[p + i] * (0.5 * (1 - Math.cos(2 * Math.PI * i / (N - 1))));
    fft(re, im);
    for (let k = 1; k < N / 2; k++) mag[k] += Math.hypot(re[k], im[k]);
    count++;
  }
  if (count) for (let k = 0; k < mag.length; k++) mag[k] /= count;
  return { rms, mag };
}
function cosine(a, b) { let d = 0, na = 0, nb = 0; for (let k = 2; k < Math.min(a.length, b.length); k++) { d += a[k]*b[k]; na += a[k]*a[k]; nb += b[k]*b[k]; } return d / Math.sqrt(na * nb || 1); }

const [refPath, wasmPath] = process.argv.slice(2);
const dur = Number(process.argv[4] ?? 10), step = Number(process.argv[5] ?? 10);
const REF = loadWav(refPath), WAS = loadWav(wasmPath);
const total = Math.min(REF.frames / REF.sampleRate, WAS.frames / WAS.sampleRate);
console.log(`REF ${refPath}: ${(REF.frames / REF.sampleRate).toFixed(1)}s | WASM ${wasmPath}: ${(WAS.frames / WAS.sampleRate).toFixed(1)}s\n`);
console.log("时间 | REF rms | WAS rms | RMS比 | 余弦");
const cs = [], rs = [];
for (let t = 0; t + dur <= total; t += step) {
  const r = features(REF, t, dur), w = features(WAS, t, dur);
  const c = cosine(r.mag, w.mag), ratio = w.rms / Math.max(1e-9, r.rms);
  cs.push(c); rs.push(ratio);
  console.log(`${String(t).padStart(4)}s | ${r.rms.toFixed(4)} | ${w.rms.toFixed(4)} | ${ratio.toFixed(2)} | ${c.toFixed(3)}${c < 0.9 ? " <<" : ""}`);
}
const avg = (a) => a.reduce((x, y) => x + y, 0) / a.length;
console.log(`\n平均余弦 ${avg(cs).toFixed(3)} | 余弦最低 ${Math.min(...cs).toFixed(3)} | RMS 比均值 ${avg(rs).toFixed(2)}（范围 ${Math.min(...rs).toFixed(2)}–${Math.max(...rs).toFixed(2)}）`);
console.log(`余弦<0.9 的窗口: ${cs.map((c, i) => [i * step, c]).filter(([, c]) => c < 0.9).map(([t, c]) => `${t}s(${c.toFixed(2)})`).join(", ") || "无"}`);
