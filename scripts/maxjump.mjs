// 在时间窗口内列出最大的样本跳变（定位咔哒）
// 用法: node maxjump.mjs <wav> <start> <end> [topN=10]
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
const [path, sArg, eArg, nArg] = process.argv.slice(2);
const w = loadWav(path);
const s0 = Math.round(Number(sArg) * w.sampleRate), e0 = Math.round(Number(eArg) * w.sampleRate);
const top = [];
let prev = smp(w.buf, w.bits, w.dataOff + s0 * w.bps * w.channels);
for (let f = s0 + 1; f < Math.min(e0, w.frames); f++) {
  const v = smp(w.buf, w.bits, w.dataOff + f * w.bps * w.channels);
  const j = Math.abs(v - prev);
  top.push({ f, t: f / w.sampleRate, j, v, prev });
  prev = v;
}
top.sort((a, b) => b.j - a.j);
console.log(`${path} ${sArg}~${eArg}s 最大跳变 top ${nArg ?? 10}:`);
for (const x of top.slice(0, Number(nArg ?? 10))) {
  console.log(`  ${x.t.toFixed(4)}s frame ${x.f} |Δ|=${x.j.toFixed(4)}  ${x.prev.toFixed(4)} → ${x.v.toFixed(4)}`);
}
