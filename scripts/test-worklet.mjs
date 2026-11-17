// 在 Node 里用假的 AudioWorklet 环境跑 web/worklet.js，验证：
//  - standalone wasm 能实例化（msfa 唯一 import 是 emscripten_notify_memory_growth；vem 无 import）
//  - 事件调度/渲染出非零声音（DX7 9 轨 + VEM CH3）
//  - seek / 暂停 / 参数下发不崩
//  - CPU 占用（--engine dx7|vem|both）
import { readFileSync, existsSync } from "fs";
import { pathToFileURL } from "url";

const SR = 48000, FRAMES = 128;
const argv = process.argv.slice(2);
const opt = (n, d) => { const i = argv.indexOf(n); return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : d; };
const engine = opt("--engine", "both"); // dx7 | vem | both

let fakeNow = 0;
Object.defineProperty(globalThis, "sampleRate", { value: SR, configurable: true });
Object.defineProperty(globalThis, "currentTime", { get: () => fakeNow, configurable: true });

let Processor = null;
globalThis.registerProcessor = (name, cls) => { Processor = cls; console.log(`registerProcessor: ${name}`); };
globalThis.AudioWorkletProcessor = class {
  constructor() {
    const self = this;
    this.__msgs = [];
    this.port = {
      postMessage: (m) => self.__msgs.push(m),
      set onmessage(fn) { self.__onmsg = fn; },
      get onmessage() { return self.__onmsg; },
    };
  }
};

await import(pathToFileURL("web/worklet.js").href);
if (!Processor) throw new Error("worklet 没有注册 processor");

const proc = new Processor();
const outL = new Float32Array(FRAMES), outR = new Float32Array(FRAMES);
const outputs = [[outL, outR]];

const wasmBinary = new Uint8Array(readFileSync("web/msfa.wasm"));
const patches = new Uint8Array(readFileSync("web/data/patches.bin"));
const events = new Uint8Array(readFileSync("web/data/notes.bin"));
const tracks = JSON.parse(readFileSync("web/data/tracks.json", "utf8"));
const mix = JSON.parse(readFileSync("web/data/mix.json", "utf8"));

let vem = null;
if (engine !== "dx7" && existsSync("web/vem.wasm")) {
  const wasm = new Uint8Array(readFileSync("web/vem.wasm"));
  const data = [];
  for (const f of ["web/data/vem/body-violin.bin", "web/data/vem/noise.bin"]) {
    if (existsSync(f)) data.push(new Uint8Array(readFileSync(f)).buffer);
  }
  const ch3 = tracks.find((t) => t.channel === 2);
  vem = {
    wasmBinary: wasm,
    channel: 2,
    gain: (ch3?.volume ?? 1) * 0.906,
    calibration: 0.906,
    data,
    params: {
      art: 0, bow: 0.5, pressure: 1, vibrato: 0.4, vibratoRate: 5.2,
      attackMs: 8, attackVelMs: 60, releaseMs: 60, releaseSlowMs: 120,
      bowSettleMs: 208.33, vibOnsetMs: 120, releaseRate: 0.5,
      bodyMix: 0.18, bowNoiseOn: 1, bowNoise: 0.5,
    },
  };
  console.log(`VEM: ${(wasm.byteLength / 1024).toFixed(1)} KB，数据 ${data.length} 个文件`);
}

const send = (m) => proc.port.onmessage({ data: m });
const take = () => proc.__msgs.splice(0, proc.__msgs.length);

console.log(`初始化：${tracks.length} 轨，${(events.byteLength / 1024).toFixed(1)} KB 事件，msfa ${(wasmBinary.byteLength / 1024).toFixed(1)} KB，SR=${SR}`);
send({
  type: "init", wasmBinary, patches, tracks, events,
  master: mix.masterVolume, duration: mix.duration,
  vem,
  fx: {
    reverb: { mix: 0.18, size: 0.6, damp: 0.45, width: 1 },
    comp: { on: 1, thr: -18, ratio: 3, atk: 10, rel: 150, makeup: 2 },
    chorus: { on: 1, rate: 0.6, depth: 0.5, mix: 0.7, spread: 180 },
  },
});

for (let i = 0; i < 300 && !proc.ready; i++) {
  const err = proc.__msgs.find((m) => m.type === "error");
  if (err) throw new Error("worklet init 失败: " + err.message);
  await new Promise((r) => setTimeout(r, 10));
}
if (!proc.ready) throw new Error("init 未完成：没有任何 ready/error 消息");
console.log("ready:", take().find((m) => m.type === "ready"));

// 播放 3 秒
send({ type: "play" });
let peak = 0, sum = 0, posMsgs = 0;
const t0 = Date.now();
const seconds = 3;
for (let b = 0; b < (seconds * SR) / FRAMES; b++) {
  proc.process([], outputs);
  fakeNow += FRAMES / SR;
  for (let i = 0; i < FRAMES; i++) { peak = Math.max(peak, Math.abs(outL[i]), Math.abs(outR[i])); sum += outL[i] * outL[i] + outR[i] * outR[i]; }
}
const cpu = (Date.now() - t0) / 1000;
for (const m of take()) if (m.type === "pos") posMsgs++;
console.log(`播放 ${seconds}s：peak ${peak.toFixed(4)} | rms ${Math.sqrt(sum / (2 * seconds * SR)).toFixed(4)} | pos 消息 ${posMsgs} | CPU ${cpu.toFixed(2)}s = ${((cpu / seconds) * 100).toFixed(1)}% 实时`);
if (peak <= 0.001) throw new Error("没有声音");

// seek 到 200s 后继续播放
send({ type: "seek", time: 200 });
for (let b = 0; b < (1 * SR) / FRAMES; b++) { proc.process([], outputs); fakeNow += FRAMES / SR; }
const pos = take().filter((m) => m.type === "pos").pop();
console.log("seek 200s 后位置:", pos ? pos.time.toFixed(2) : "无 pos 消息");
if (!pos || Math.abs(pos.time - 201) > 0.5) throw new Error("seek 位置不对");

// 参数下发
send({ type: "master", value: 0.5 });
send({ type: "trackGain", index: 2, value: 1.2 });
send({ type: "bypass", on: true });
send({ type: "bypass", on: false });
send({ type: "vemParam", params: { art: 3, bow: 0.3, vibrato: 0.8, bodyMix: 0.1, bowNoise: 0.2 } });
send({ type: "fx", fx: { reverb: { mix: 0.3, size: 0.8, damp: 0.2, width: 0.5 }, comp: { on: 0, thr: -24, ratio: 6, atk: 5, rel: 100, makeup: 0 }, chorus: { on: 0, rate: 1, depth: 0.3, mix: 0.5, spread: 90 } } });
for (let b = 0; b < (0.5 * SR) / FRAMES; b++) { proc.process([], outputs); fakeNow += FRAMES / SR; }
console.log("参数下发 OK");

// 暂停
send({ type: "pause" });
proc.process([], outputs);
const silent = outL.every((v) => v === 0);
console.log("暂停静音:", silent);
if (!silent) throw new Error("暂停后还在出声");
console.log("\n全部通过 ✅");
process.exit(0);
