// AudioWorklet：调度音符 + 调用 msfa wasm（引擎 + DSP）实时渲染
// 与主线程的协议：
//   in : {type:'init', wasmBinary, patches:ArrayBuffer, tracks, events:ArrayBuffer, master, fx, duration}
//        {type:'play'} {type:'pause'} {type:'seek', time} {type:'master', value}
//        {type:'trackGain', index, value} {type:'fx', fx} {type:'bypass', on}
//   out: {type:'ready'} {type:'pos', time} {type:'level', peak}

const BLOCK = 64;

class MsfaProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.port.onmessage = (e) => this.onMessage(e.data);
    this.ready = false;
    this.playing = false;
    this.bypass = false;
    this.pos = 0;          // 已渲染的帧数（相对 seek 起点）
    this.offset = 0;       // seek 起点的秒数
    this.duration = 0;
    this.fx = { reverb: { mix: 0, size: 0.6, damp: 0.45, width: 1 }, comp: { on: 1, thr: -12, ratio: 2, atk: 15, rel: 200, makeup: 1 }, chorus: { on: 1, rate: 0.6, depth: 0.5, mix: 0.7, spread: 180 } };
    this.trackGain = [];
    this.lastLevelReport = 0;
    this.fade = 0;
  }

  onMessage(msg) {
    if (msg.type === "init") {
      this.init(msg);
    } else if (msg.type === "play") {
      this.playing = true;
      this.fade = 0;
    } else if (msg.type === "pause") {
      this.playing = false;
    } else if (msg.type === "seek") {
      this.seek(msg.time);
    } else if (msg.type === "master") {
      if (this.ready) this.E._msfa_mixer_set_master(this.mixer, msg.value);
    } else if (msg.type === "trackGain") {
      this.trackGain[msg.index] = msg.value;
      if (this.ready) this.E._msfa_mixer_set_gain(this.mixer, this.tracks[msg.index].channel, msg.value);
    } else if (msg.type === "fx") {
      this.fx = msg.fx;
      this.applyFx();
    } else if (msg.type === "bypass") {
      this.bypass = msg.on;
      this.applyFx();
    }
  }

  async init(msg) {
    try {
      await this.initInner(msg);
    } catch (e) {
      this.port.postMessage({ type: "error", message: String((e && e.stack) || e) });
    }
  }

  async initInner(msg) {
    const imports = { env: { emscripten_notify_memory_growth: () => {} } };
    const { instance } = await WebAssembly.instantiate(msg.wasmBinary, imports);
    // standalone wasm 的导出名没有前导下划线（emscripten 胶水才会加）。
    // instance.exports 是 frozen 对象，不能直接加别名 → 复制一层。
    const raw = instance.exports;
    const E = {};
    for (const k of Object.keys(raw)) {
      E[k] = raw[k];
      if (!k.startsWith("_") && typeof raw[k] === "function") E["_" + k] = raw[k];
    }
    this.E = E;
    const initFn = raw._initialize || raw.initialize;
    if (initFn) initFn();

    this.sr = sampleRate;
    this.mixer = this.E._msfa_mixer_new(this.sr);
    this.patches = new Uint8Array(msg.patches);
    this.tracks = msg.tracks;
    this.duration = msg.duration;
    this.trackGain = this.tracks.map((t) => t.volume);

    // 每个通道的音色（malloc 一次，seek 时复用）
    this.patchPtrs = [];
    for (let i = 0; i < this.tracks.length; i++) {
      const p = this.E.malloc(156);
      this.patchPtrs.push(p);
      new Uint8Array(this.E.memory.buffer, p, 156).set(this.patches.subarray(i * 156, i * 156 + 156));
      this.E._msfa_mixer_set_patch(this.mixer, this.tracks[i].channel, p);
      this.E._msfa_mixer_set_gain(this.mixer, this.tracks[i].channel, this.tracks[i].volume);
    }
    this.E._msfa_mixer_set_master(this.mixer, msg.master);
    this.outL = this.E.malloc(BLOCK * 4);
    this.outR = this.E.malloc(BLOCK * 4);

    // 事件（兼容 ArrayBuffer / TypedArray）
    const evBytes = msg.events instanceof ArrayBuffer
      ? new Uint8Array(msg.events)
      : new Uint8Array(msg.events.buffer, msg.events.byteOffset || 0, msg.events.byteLength);
    const dv = new DataView(evBytes.buffer, evBytes.byteOffset, evBytes.byteLength);
    const n = dv.getUint32(4, true);
    this.events = new Float64Array(n * 2);      // t, packed(code)
    this.evData = new Uint8Array(n * 4);        // ch,note,vel,on
    const base = 12;
    for (let i = 0; i < n; i++) {
      const o = base + i * 8;
      this.events[i * 2] = dv.getFloat32(o, true);
      this.events[i * 2 + 1] = i;
      this.evData[i * 4] = dv.getUint8(o + 4);
      this.evData[i * 4 + 1] = dv.getUint8(o + 5);
      this.evData[i * 4 + 2] = dv.getUint8(o + 6);
      this.evData[i * 4 + 3] = dv.getUint8(o + 7);
    }
    this.evIndex = 0;
    this.fx = msg.fx;
    this.ready = true;
    this.applyFx();
    this.port.postMessage({ type: "ready", sampleRate: this.sr, duration: this.duration, events: n });
  }

  // 把 FX 参数下发给 wasm（bypass 时全部旁通）
  applyFx() {
    if (!this.ready) return;
    const E = this.E, m = this.mixer;
    const b = this.bypass;
    const r = this.fx.reverb, c = this.fx.comp, ch = this.fx.chorus;
    E._msfa_mixer_set_reverb(m, b ? 0 : r.mix, r.size, r.damp, r.width);
    E._msfa_mixer_set_comp(m, b ? 0 : (c.on ? 1 : 0), c.thr, c.ratio, c.atk, c.rel, c.makeup);
    // 合唱只作用于 CH3（channel 2）
    for (const t of this.tracks) {
      if (t.chorus) E._msfa_mixer_set_chorus(m, t.channel, (!b && ch.on) ? 1 : 0, ch.rate, ch.depth, ch.mix, ch.spread);
    }
  }

  seek(time) {
    if (!this.ready) return;
    this.E._msfa_mixer_reset(this.mixer);
    // reset 会销毁实例，重新下发音色与增益（复用已分配的 patch 指针）
    for (let i = 0; i < this.tracks.length; i++) {
      this.E._msfa_mixer_set_patch(this.mixer, this.tracks[i].channel, this.patchPtrs[i]);
      this.E._msfa_mixer_set_gain(this.mixer, this.tracks[i].channel, this.trackGain[i] ?? this.tracks[i].volume);
    }
    this.applyFx();
    this.offset = Math.max(0, Math.min(time, this.duration));
    this.pos = 0;
    // 事件指针：跳到该时间点（不从 0 追算，避免 seek 卡顿）
    let i = 0;
    while (i < this.events.length / 2 && this.events[i * 2] < this.offset) i++;
    this.evIndex = i;
    this.fade = 0; // 重新淡入，避免瞬态咔哒
  }

  process(inputs, outputs) {
    const out = outputs[0];
    if (!this.ready || !out || out.length < 2) return true;
    const L = out[0], R = out[1];
    const frames = L.length;

    if (!this.playing) {
      // 暂停时输出静音但保持节点存活
      L.fill(0); R.fill(0);
      return true;
    }

    for (let base = 0; base < frames; base += BLOCK) {
      // 触发到本块为止的事件
      const nowT = this.offset + (this.pos + base) / this.sr;
      while (this.evIndex < this.events.length / 2) {
        const t = this.events[this.evIndex * 2];
        if (t > nowT) break;
        const d = this.evIndex * 4;
        const ch = this.evData[d], note = this.evData[d + 1], vel = this.evData[d + 2], on = this.evData[d + 3];
        if (on) this.E._msfa_mixer_note_on(this.mixer, ch, note, vel);
        else this.E._msfa_mixer_note_off(this.mixer, ch, note);
        this.evIndex++;
      }
      this.E._msfa_mixer_render(this.mixer, this.outL, this.outR, BLOCK);
      const sl = new Float32Array(this.E.memory.buffer, this.outL, BLOCK);
      const sr = new Float32Array(this.E.memory.buffer, this.outR, BLOCK);
      const lim = Math.min(BLOCK, frames - base);
      for (let i = 0; i < lim; i++) {
        // 2ms 淡入（seek 后）/ 2ms 淡出（到达结尾）
        if (this.fade < 1) this.fade = Math.min(1, this.fade + 1 / (0.002 * this.sr));
        L[base + i] = sl[i] * this.fade;
        R[base + i] = sr[i] * this.fade;
      }
    }
    this.pos += frames;

    // 进度/电平回报（约 20Hz）
    const now = currentTime;
    if (now - this.lastLevelReport > 0.05) {
      this.lastLevelReport = now;
      let peak = 0;
      for (let i = 0; i < frames; i++) peak = Math.max(peak, Math.abs(L[i]), Math.abs(R[i]));
      this.port.postMessage({ type: "pos", time: this.offset + this.pos / this.sr, peak, duration: this.duration });
    }
    if (this.offset + this.pos / this.sr >= this.duration + 2) {
      this.playing = false;
      this.port.postMessage({ type: "ended" });
    }
    return true;
  }
}

registerProcessor("msfa-processor", MsfaProcessor);
