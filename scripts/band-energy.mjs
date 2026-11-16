// 分频段能量对比（判断差异集中在低频/中频/高频）
// 用法: node band-energy.mjs <a.wav> <b.wav> <startSec> <durSec>
import { readFileSync } from "fs";
function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, dataOff = -1, dataLen = 0, fmt = null;
  while (off + 8 <= b.length) { const id = b.toString("ascii", off, off + 4); const len = b.readUInt32LE(off + 4);
    if (id === "fmt ") fmt = { channels: b.readUInt16LE(off + 10), sampleRate: b.readUInt32LE(off + 12), bits: b.readUInt16LE(off + 22) };
    if (id === "data") { dataOff = off + 8; dataLen = len; break; } off += 8 + len + (len & 1); }
  return { buf: b, ...fmt, dataOff, bps: fmt.bits / 8, frames: Math.floor(dataLen / (fmt.bits / 8 * fmt.channels)) };
}
function smp(buf, bits, o) { if (bits === 24) { let x = buf[o] | (buf[o+1]<<8) | (buf[o+2]<<16); if (x & 0x800000) x -= 0x1000000; return x/8388608; } if (bits === 16) return buf.readInt16LE(o)/32768; if (bits === 32) return buf.readFloatLE(o); return 0; }
function mono(w, tSec, n) { const s = Math.round(tSec * w.sampleRate), out = new Float64Array(n); for (let i = 0; i < n; i++) { const f = s + i; if (f < 0 || f >= w.frames) break; const base = w.dataOff + f * w.bps * w.channels; let v = 0; for (let c = 0; c < w.channels; c++) v += smp(w.buf, w.bits, base + c * w.bps); out[i] = v / w.channels; } return out; }
function fft(re, im) { const n = re.length; if (n <= 1) return; const h = n >> 1; const a=new Float64Array(h),b=new Float64Array(h),c=new Float64Array(h),d=new Float64Array(h); for (let i=0;i<h;i++){a[i]=re[2*i];b[i]=im[2*i];c[i]=re[2*i+1];d[i]=im[2*i+1];} fft(a,b); fft(c,d); for (let k=0;k<h;k++){const ang=(-2*Math.PI*k)/n,co=Math.cos(ang),si=Math.sin(ang);const tr=c[k]*co-d[k]*si,ti=c[k]*si+d[k]*co;re[k]=a[k]+tr;im[k]=b[k]+ti;re[k+h]=a[k]-tr;im[k+h]=b[k]-ti;} }
const N = 8192;
function bands(w, tSec, durSec) {
  const win = mono(w, tSec, Math.round(durSec * w.sampleRate));
  const freqs = [0, 120, 250, 500, 1000, 2000, 4000, 8000, 16000];
  const acc = freqs.map(() => 0);
  const step = Math.max(N, Math.floor((win.length - N) / 8) || N);
  for (let p = 0; p + N <= win.length; p += step) {
    const re = new Float64Array(N), im = new Float64Array(N);
    for (let i = 0; i < N; i++) re[i] = win[p + i] * (0.5 * (1 - Math.cos(2 * Math.PI * i / (N - 1))));
    fft(re, im);
    for (let k = 1; k < N / 2; k++) {
      const f = k * w.sampleRate / N;
      let bi = freqs.length - 2;
      for (let i = 0; i < freqs.length - 1; i++) if (f >= freqs[i] && f < freqs[i + 1]) { bi = i; break; }
      acc[bi] += re[k] * re[k] + im[k] * im[k];
    }
  }
  return acc.map((v) => Math.sqrt(v));
}
const [a, b, aStart, bStart, du] = process.argv.slice(2);
const A = bands(loadWav(a), Number(aStart), Number(du));
const B = bands(loadWav(b), Number(bStart), Number(du));
const edges = ["0-120", "120-250", "250-500", "500-1k", "1k-2k", "2k-4k", "4k-8k", "8k-16k"];
console.log("频段     | A 能量    | B 能量    | B/A(dB)");
for (let i = 0; i < edges.length; i++) {
  const db = 20 * Math.log10((B[i] + 1e-12) / (A[i] + 1e-12));
  console.log(`${edges[i].padEnd(8)} | ${A[i].toExponential(2).padEnd(9)} | ${B[i].toExponential(2).padEnd(9)} | ${db.toFixed(1)}`);
}
