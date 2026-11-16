// 统计两份渲染的峰值/超 0dBFS 采样数（判断是否在导出环节削顶）
// 用法: node peak-report.mjs <wav> [wav...]
import { readFileSync } from "fs";
function loadWav(path) {
  const b = readFileSync(path);
  let off = 12, dataOff = -1, dataLen = 0, fmt = null;
  while (off + 8 <= b.length) {
    const id = b.toString("ascii", off, off + 4), len = b.readUInt32LE(off + 4);
    if (id === "fmt ") fmt = { channels: b.readUInt16LE(off + 10), sampleRate: b.readUInt32LE(off + 12), bits: b.readUInt16LE(off + 22), tag: b.readUInt16LE(off + 8) };
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
for (const path of process.argv.slice(2)) {
  const w = loadWav(path);
  let peak = 0, over = 0, sat = 0;
  for (let f = 0; f < w.frames; f++) {
    let v = 0;
    for (let c = 0; c < w.channels; c++) v += smp(w.buf, w.bits, w.dataOff + (f * w.channels + c) * w.bps);
    v /= w.channels;
    const a = Math.abs(v);
    if (a > peak) peak = a;
    if (a > 1.0) over++;
    if (a >= 0.9995) sat++;
  }
  console.log(`${path}: ${w.bits}bit ${w.channels}ch | peak ${peak.toFixed(4)} (${(20 * Math.log10(peak)).toFixed(2)} dBFS) | >1.0: ${over} | >=0.9995: ${sat}`);
}
