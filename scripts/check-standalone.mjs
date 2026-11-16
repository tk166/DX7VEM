// 检查 standalone wasm 的 imports / exports / 体积
// 用法: node check-standalone.mjs <file.wasm> [--prefix msfa|vem]
//   - imports 必须为空（standalone，AudioWorklet 里没有胶水/环境）
//   - 关键导出必须齐全
import { readFileSync } from "fs";

const path = process.argv[2] || "vem/wasm/vem.wasm";
const prefixArg = process.argv.indexOf("--prefix");
const prefix = prefixArg >= 0 ? process.argv[prefixArg + 1] : path.toLowerCase().includes("vem") ? "vem" : "msfa";

const REQUIRED = {
  msfa: ["msfa_mixer_new", "msfa_mixer_render", "msfa_mixer_set_patch", "malloc", "free"],
  vem: ["vem_new", "vem_free", "vem_note_on", "vem_note_off", "vem_render", "vem_alloc", "vem_free_mem"],
}[prefix];
if (!REQUIRED) throw new Error(`未知 prefix: ${prefix}`);

const bytes = readFileSync(path);
if (bytes.readUInt32LE(0) !== 0x6d736100) throw new Error("not a wasm file");

let off = 8;
const u32 = () => {
  let r = 0, s = 0, b;
  do {
    b = bytes[off++];
    r |= (b & 0x7f) << s;
    s += 7;
  } while (b & 0x80);
  return r >>> 0;
};
const str = () => {
  const n = u32();
  const s = bytes.toString("utf8", off, off + n);
  off += n;
  return s;
};

const imports = [], exports = [];
while (off < bytes.length) {
  const id = bytes[off++];
  const size = u32();
  const end = off + size;
  if (id === 2) {
    const n = u32();
    for (let i = 0; i < n; i++) {
      const mod = str(), name = str(), kind = bytes[off++];
      if (kind === 0) u32();
      else if (kind === 1) {
        bytes[off++];
        u32();
        const fl = u32();
        if (fl & 1) u32();
      } else if (kind === 2) {
        const fl = u32();
        if (fl & 1) u32();
        u32();
      } else if (kind === 3) {
        bytes[off++];
        bytes[off++];
      }
      imports.push(`${mod}.${name}`);
    }
  } else if (id === 7) {
    const n = u32();
    for (let i = 0; i < n; i++) {
      const name = str(), kind = bytes[off++];
      u32();
      if (kind === 0 || kind === 3) exports.push(name);
      else exports.push(`${name}(mem)`);
    }
  }
  off = end;
}

const fn = exports.filter((e) => !e.endsWith("(mem)"));
console.log(`${path}: ${(bytes.length / 1024).toFixed(1)} KB`);
console.log(`imports (${imports.length}): ${imports.join(", ") || "无 ✅"}`);
console.log(`exports: ${exports.length} 个`);
console.log("  内存:", exports.filter((e) => e.endsWith("(mem)")).join(", ") || "无");
console.log("  关键函数:", fn.filter((n) => n.startsWith(prefix) || n === "malloc" || n === "free" || n === "_initialize").join(", "));

const missing = REQUIRED.filter((n) => !fn.includes(n));
let fail = false;
if (imports.length) {
  console.log(`❌ 存在 imports（standalone 要求为空）`);
  fail = true;
}
if (missing.length) {
  console.log("❌ 缺少:", missing.join(", "));
  fail = true;
}
if (!fail) console.log("✅ standalone 体检通过");
if (fail) process.exit(1);
