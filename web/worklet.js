// AudioWorklet：调度音符 + 调用 msfa wasm（DX7 9 轨）+ vem wasm（CH3 弦乐），实时渲染
// 与主线程的协议：
//   in : {type:'init', wasmBinary, patches, tracks, events, master, fx, duration,
//         vem:{wasmBinary, channel, gain, data:[ArrayBuffer], params}}
//        {type:'play'} {type:'pause'} {type:'seek', time} {type:'master', value}
//        {type:'trackGain', index, value} {type:'fx', fx} {type:'bypass', on}
//        {type:'vemParam', params}
//   out: {type:'ready'} {type:'pos', time} {type:'level', peak}

const BLOCK = 64;

// 末端软限幅：保护 0 dBFS（与引擎里的曲线一致）
function softClip(x) {
  const t = 0.98, inv = 1 - t;
  if (x > t) return t + inv * Math.tanh((x - t) / inv);
  if (x < -t) return -t - inv * Math.tanh((-x - t) / inv);
  return x;
}

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
    this.vemEngines = null;
    this.vemAccL = new Float32Array(BLOCK);
    this.vemAccR = new Float32Array(BLOCK);
    this._views = new Map();
  }

  /// 缓存 WASM 内存的 Float32 视图（Safari 上每块都 new 会造成 GC 抖动）
  view(E, ptr, len) {
    const key = ptr + ":" + len;
    let v = this._views.get(key);
    if (!v || v.buffer !== E.memory.buffer) {
      v = new Float32Array(E.memory.buffer, ptr, len);
      this._views.set(key, v);
    }
    return v;
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
      if (!this.ready) return;
      this.master = msg.value;
      this.E._msfa_mixer_set_master(this.mixer, msg.value);
      if (this.vemEngines) for (const e of this.vemEngines) e.E.vem_set_master(e.eng, msg.value);
    } else if (msg.type === "trackGain") {
      this.trackGain[msg.index] = msg.value;
      if (!this.ready) return;
      const ch = this.tracks[msg.index].channel;
      if (this.vemEngines && this.vemChannels.has(ch)) {
        for (const e of this.vemEngines) {
          if (e.entry.channel === ch) {
            e.entry.gain = msg.value;
            e.E.vem_set_gain(e.eng, msg.value);
          }
        }
      } else {
        this.E._msfa_mixer_set_gain(this.mixer, ch, msg.value);
      }
    } else if (msg.type === "fx") {
      this.fx = msg.fx;
      this.applyFx();
    } else if (msg.type === "bypass") {
      this.bypass = msg.on;
      this.applyFx();
    } else if (msg.type === "vemParam") {
      if (!this.vemEngines) return;
      for (const e of this.vemEngines) {
        Object.assign(e.params, msg.params || {});
        this.applyVemEngine(e);
      }
    } else if (msg.type === "vemTrack") {
      if (!this.vemEngines) return;
      const e = this.vemEngines[msg.index];
      if (!e) return;
      Object.assign(e.params, msg.params || {});
      this.applyVemEngine(e);
    } else if (msg.type === "events") {
      this.replaceSong(msg.events, msg.exprs);
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
    this.master = msg.master;
    this.trackGain = this.tracks.map((t) => t.volume);

    // ---- VEM（多条弦乐轨，共享一个 wasm 实例）----
    if (msg.vem) {
      const vraw = (await WebAssembly.instantiate(msg.vem.wasmBinary, {})).instance.exports;
      const vE = vraw;
      const defaults = {
        art: 0,
        bow: 0.5,
        pressure: 1,
        vibrato: 0.4,
        vibratoRate: 5.2,
        attackMs: 8,
        attackVelMs: 60,
        releaseMs: 60,
        releaseSlowMs: 120,
        bowSettleMs: 208.33,
        vibOnsetMs: 120,
        releaseRate: 0.5,
        bodyMix: 0.18,
        bowNoise: 0.5,
      };
      this.vemChannels = new Set();
      this.vemEngines = (msg.vem.tracks || []).map((entry, idx) => {
        const instIndex = { violin: 0, viola: 1, cello: 2, bass: 3 }[entry.instrument] ?? 0;
        const eng = vE.vem_new(this.sr, instIndex);
        // 每个引擎独立种子（同一段旋律的四把琴不能同相漂移）
        vE.vem_set_seed(eng, entry.seed ?? 12345 + idx * 7919);
        // 数据（琴体 IR / 弓噪）
        for (const buf of entry.data || []) {
          const bytes = new Uint8Array(buf);
          if (!bytes.byteLength) continue;
          const ptr = vE.vem_alloc(bytes.byteLength);
          new Uint8Array(vE.memory.buffer, ptr, bytes.byteLength).set(bytes);
          vE.vem_set_patch(eng, ptr, bytes.byteLength);
          vE.vem_free_mem(ptr, bytes.byteLength);
        }
        const e = {
          idx,
          E: vE,
          eng,
          entry,
          params: Object.assign({}, defaults, entry.params || {}),
        };
        vE.vem_set_master(eng, this.master);
        vE.vem_set_polyphonic(eng, entry.polyphonic ? 1 : 0);
        vE.vem_set_gain(eng, entry.gain ?? 1);
        this.applyVemEngine(e);
        this.vemChannels.add(entry.channel);
        return e;
      });
      this.vemL = vE.vem_alloc(BLOCK * 4);
      this.vemR = vE.vem_alloc(BLOCK * 4);
      // 轨道推子：VEM 通道的初始值取校准后的绝对增益
      if (this.vemEngines.length) {
        for (let i = 0; i < this.tracks.length; i++) {
          const ch = this.tracks[i].channel;
          const eng = this.vemEngines.find((e) => e.entry.channel === ch);
          if (eng && eng.entry.gain !== undefined) this.trackGain[i] = eng.entry.gain;
        }
      }
    }

    // 每个通道的音色（malloc 一次，seek 时复用）
    this.patchPtrs = [];
    for (let i = 0; i < this.tracks.length; i++) {
      const ch = this.tracks[i].channel;
      if (this.vemChannels && this.vemChannels.has(ch)) {
        this.patchPtrs.push(0);
        continue; // 该通道交给 VEM，不再创建 DX7 实例
      }
      const p = this.E.malloc(156);
      this.patchPtrs.push(p);
      new Uint8Array(this.E.memory.buffer, p, 156).set(this.patches.subarray(i * 156, i * 156 + 156));
      this.E._msfa_mixer_set_patch(this.mixer, ch, p);
      this.E._msfa_mixer_set_gain(this.mixer, ch, this.tracks[i].volume);
    }
    this.E._msfa_mixer_set_master(this.mixer, msg.master);
    this.outL = this.E.malloc(BLOCK * 4);
    this.outR = this.E.malloc(BLOCK * 4);

    // 初始推子（dB→线性由主线程算好）：VEM 通道是引擎绝对增益
    if (msg.gains) {
      for (let i = 0; i < this.tracks.length; i++) {
        const ch = this.tracks[i].channel;
        const g = msg.gains[i];
        if (g === undefined) continue;
        this.trackGain[i] = g;
        if (this.vemChannels && this.vemChannels.has(ch)) {
          for (const e of this.vemEngines) {
            if (e.entry.channel === ch) {
              e.entry.gain = g;
              e.E.vem_set_gain(e.eng, g);
            }
          }
        } else {
          this.E._msfa_mixer_set_gain(this.mixer, ch, g);
        }
      }
    }

    // 事件（兼容 ArrayBuffer / TypedArray）
    this.parseSong(msg.events);
    this.exprs = msg.exprs ? new Float32Array(msg.exprs) : null;
    this.fx = msg.fx;
    this.ready = true;
    this.applyFx();
    this.port.postMessage({ type: "ready", sampleRate: this.sr, duration: this.duration, events: this.eventCount, vem: !!(this.vemEngines && this.vemEngines.length) });
  }

  parseSong(events) {
    const evBytes = events instanceof ArrayBuffer
      ? new Uint8Array(events)
      : new Uint8Array(events.buffer, events.byteOffset || 0, events.byteLength);
    const dv = new DataView(evBytes.buffer, evBytes.byteOffset, evBytes.byteLength);
    const n = dv.getUint32(4, true);
    this.events = new Float64Array(n * 2);      // t, index
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
    this.eventCount = n;
    this.evIndex = 0;
    // 每个 note-on 到"同通道下一个 note-on"的时间（ms），用于换音前渐弱
    this.leadMs = new Float32Array(n);
    const nextOn = new Array(16).fill(-1);
    for (let i = n - 1; i >= 0; i--) {
      const ch = this.evData[i * 4];
      const isOn = this.evData[i * 4 + 3];
      const t = this.events[i * 2];
      if (isOn) {
        this.leadMs[i] = nextOn[ch] >= 0 ? (nextOn[ch] - t) * 1000 : 0;
        nextOn[ch] = t;
      }
    }
  }

  /** 替换事件（humanize 参数变化时），保留播放位置，但重置发声状态避免卡音。 */
  replaceSong(events, exprs) {
    if (!this.ready) return;
    this.parseSong(events);
    this.exprs = exprs ? new Float32Array(exprs) : null;
    this.E._msfa_mixer_reset(this.mixer);
    for (let i = 0; i < this.tracks.length; i++) {
      const ch = this.tracks[i].channel;
      if (this.vemChannels && this.vemChannels.has(ch)) continue;
      this.E._msfa_mixer_set_patch(this.mixer, ch, this.patchPtrs[i]);
      this.E._msfa_mixer_set_gain(this.mixer, ch, this.trackGain[i] ?? this.tracks[i].volume);
    }
    if (this.vemEngines) {
      for (const e of this.vemEngines) {
        e.E.vem_reset(e.eng);
        e.E.vem_set_master(e.eng, this.master);
        e.E.vem_set_gain(e.eng, e.entry.gain ?? 1);
        this.applyVemEngine(e);
      }
    }
    this.applyFx();
    const now = this.offset + this.pos / this.sr;
    let i = 0;
    while (i < this.events.length / 2 && this.events[i * 2] < now) i++;
    this.evIndex = i;
    this.fade = 0;
  }

  applyVemEngine(e) {
    const { E, eng, params: p } = e;
    // 老版本 wasm 可能没有新导出：缺失就跳过，不能让 AudioWorklet 崩掉
    const call = (fn, ...args) => {
      if (typeof fn === "function") fn(...args);
    };
    call(E.vem_set_articulation, eng, p.art | 0);
    call(E.vem_set_bow, eng, p.bow, p.pressure);
    call(E.vem_set_vibrato, eng, p.vibrato, p.vibratoRate);
    call(
      E.vem_set_envelope,
      eng,
      p.attackMs,
      p.attackVelMs,
      p.releaseMs,
      p.releaseSlowMs,
      p.bowSettleMs,
      p.vibOnsetMs,
      60
    );
    call(E.vem_set_release_rate, eng, p.releaseRate);
    call(E.vem_set_body, eng, p.bodyMix, 0.6);
    call(E.vem_set_bow_noise, eng, p.bowNoiseOn === 0 ? 0 : 1, p.bowNoise);
    call(E.vem_set_legato, eng, p.legatoStart ?? 0.5, p.legatoRampMs ?? 0, p.legatoAccent ?? 0, p.legatoBoost ?? 0, p.legatoBoostMs ?? 25);
    call(E.vem_set_phrase, eng, p.accent ?? 0, p.accentMs ?? 45, p.bodyMs ?? 120, p.taper ?? 0, p.taperMs ?? 130);
  }

  // 把 FX 参数下发给 wasm（bypass 时全部旁通）
  applyFx() {
    if (!this.ready) return;
    const E = this.E, m = this.mixer;
    const b = this.bypass;
    const r = this.fx.reverb, c = this.fx.comp, ch = this.fx.chorus;
    E._msfa_mixer_set_reverb(m, b ? 0 : r.mix, r.size, r.damp, r.width);
    E._msfa_mixer_set_comp(m, b ? 0 : (c.on ? 1 : 0), c.thr, c.ratio, c.atk, c.rel, c.makeup);
    if (this.vemEngines) {
      for (const e of this.vemEngines) {
        e.E.vem_set_reverb(e.eng, b ? 0 : r.mix, r.size, r.damp, r.width);
        e.E.vem_set_comp(e.eng, b ? 0 : (c.on ? 1 : 0), c.thr, c.ratio, c.atk, c.rel, c.makeup);
      }
    }
    // 合唱只作用于 CH3（若 CH3 走 VEM 则没有合唱）
    for (const t of this.tracks) {
      if (t.chorus && !(this.vemChannels && this.vemChannels.has(t.channel))) {
        E._msfa_mixer_set_chorus(m, t.channel, (!b && ch.on) ? 1 : 0, ch.rate, ch.depth, ch.mix, ch.spread);
      }
    }
  }

  seek(time) {
    if (!this.ready) return;
    this.E._msfa_mixer_reset(this.mixer);
    // reset 会销毁实例，重新下发音色与增益（复用已分配的 patch 指针）
    for (let i = 0; i < this.tracks.length; i++) {
      const ch = this.tracks[i].channel;
      if (this.vemChannels && this.vemChannels.has(ch)) continue;
      this.E._msfa_mixer_set_patch(this.mixer, ch, this.patchPtrs[i]);
      this.E._msfa_mixer_set_gain(this.mixer, ch, this.trackGain[i] ?? this.tracks[i].volume);
    }
    if (this.vemEngines) {
      for (const e of this.vemEngines) {
        e.E.vem_reset(e.eng);
        e.E.vem_set_master(e.eng, this.master);
        e.E.vem_set_gain(e.eng, e.entry.gain ?? 1);
        this.applyVemEngine(e);
      }
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
        if (this.vemChannels && this.vemChannels.has(ch)) {
          const idx = this.evIndex;
          for (const e of this.vemEngines) {
            if (e.entry.channel !== ch) continue;
            if (note < e.entry.minNote || note > e.entry.maxNote) continue;
            if (on) {
              if (this.exprs) {
                if (typeof e.E.vem_set_expr === "function") {
                  const b = idx * 5;
                  const p = e.params;
                  const bow = Math.min(0.95, Math.max(0.05, (p.bow ?? 0.5) + this.exprs[b]));
                  const vib = Math.max(0, (p.vibrato ?? 0.4) * this.exprs[b + 1]);
                  const press = Math.max(0.2, (p.pressure ?? 1) * this.exprs[b + 2]);
                  const atk = Math.max(1, (p.attackMs ?? 8) * this.exprs[b + 3]);
                  const lead = this.leadMs ? this.leadMs[idx] : 0;
                  e.E.vem_set_expr(e.eng, bow, press, vib, atk, this.exprs[b + 4], lead);
                } else if (!this.exprWarned) {
                  this.exprWarned = true;
                  this.port.postMessage({ type: "warn", message: "vem.wasm 缺少 vem_set_expr（需要重新 build-wasm）" });
                }
              }
              e.E.vem_note_on(e.eng, note, vel / 127);
            } else {
              e.E.vem_note_off(e.eng, note);
            }
          }
        } else if (on) this.E._msfa_mixer_note_on(this.mixer, ch, note, vel);
        else this.E._msfa_mixer_note_off(this.mixer, ch, note);
        this.evIndex++;
      }

      this.E._msfa_mixer_render(this.mixer, this.outL, this.outR, BLOCK);
      const sl = this.view(this.E, this.outL, BLOCK);
      const sr = this.view(this.E, this.outR, BLOCK);

      let vl = null, vr = null;
      if (this.vemEngines && this.vemEngines.length) {
        vl = this.vemAccL;
        vr = this.vemAccR;
        vl.fill(0);
        vr.fill(0);
        for (const e of this.vemEngines) {
          e.E.vem_render(e.eng, this.vemL, this.vemR, BLOCK);
          const el = this.view(e.E, this.vemL, BLOCK);
          const er = this.view(e.E, this.vemR, BLOCK);
          for (let i = 0; i < BLOCK; i++) {
            vl[i] += el[i];
            vr[i] += er[i];
          }
        }
      }

      const lim = Math.min(BLOCK, frames - base);
      for (let i = 0; i < lim; i++) {
        // 2ms 淡入（seek 后）/ 2ms 淡出（到达结尾）
        if (this.fade < 1) this.fade = Math.min(1, this.fade + 1 / (0.002 * this.sr));
        const l = sl[i] + (vl ? vl[i] : 0);
        const r = sr[i] + (vr ? vr[i] : 0);
        L[base + i] = softClip(l * this.fade);
        R[base + i] = softClip(r * this.fade);
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
