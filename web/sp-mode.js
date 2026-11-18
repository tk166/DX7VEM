// ---------------------------------------------------------------------------
// Firefox Android 兼容路径：ScriptProcessorNode(4096) + 同样的 wasm 引擎
// 仅当 URL 带 ?sp=1 时启用；其它浏览器与默认路径完全不受影响。
// ---------------------------------------------------------------------------
// 与 worklet.js 末端软限幅完全一致，保护 0 dBFS
function softClip(x) {
  const t = 0.98, inv = 1 - t;
  if (x > t) return t + inv * Math.tanh((x - t) / inv);
  if (x < -t) return -t - inv * Math.tanh((-x - t) / inv);
  return x;
}
function aliasExports(exports) {
  const E = {};
  for (const k of Object.keys(exports)) {
    E[k] = exports[k];
    if (!k.startsWith("_") && typeof exports[k] === "function") E["_" + k] = exports[k];
  }
  return E;
}

async function setupSPMode(msg) {
  try {
    const sr = ctx.sampleRate;
    const M = aliasExports(
      (await WebAssembly.instantiate(msg.wasmBinary, {
        env: { emscripten_notify_memory_growth: () => {} },
      })).instance.exports
    );
    const V = msg.vem
      ? aliasExports((await WebAssembly.instantiate(msg.vem.wasmBinary, {})).instance.exports)
      : null;
    const mixer = M._msfa_mixer_new(sr);
    const patchesU8 = new Uint8Array(msg.patches);
    M._msfa_mixer_set_master(mixer, msg.master);
    const patchPtrs = [];
    const vemTracksCfg = msg.vem ? msg.vem.tracks : [];
    const vemChannels = new Set(vemTracksCfg.map((t) => t.channel));

    for (let i = 0; i < msg.tracks.length; i++) {
      const ch = msg.tracks[i].channel;
      if (vemChannels.has(ch)) {
        patchPtrs.push(0);
        continue;
      }
      const ptr = M.malloc(156);
      new Uint8Array(M.memory.buffer, ptr, 156).set(patchesU8.subarray(i * 156, i * 156 + 156));
      patchPtrs.push(ptr);
      M._msfa_mixer_set_patch(mixer, ch, ptr);
      M._msfa_mixer_set_gain(mixer, ch, msg.gains[i] ?? msg.tracks[i].volume);
      M._msfa_mixer_set_chorus(mixer, ch, 0, 0.6, 0.5, 0.7, 180);
    }

    const engs = [];
    if (V) {
      // 与 worklet.js 的 defaults 完全一致，保证两种路径参数一一对应
      const defaults = {
        art: 0, bow: 0.5, pressure: 1, vibrato: 0.4, vibratoRate: 5.2,
        attackMs: 8, attackVelMs: 60, releaseMs: 60, releaseSlowMs: 120,
        bowSettleMs: 208.33, vibOnsetMs: 120, releaseRate: 0.5,
        bodyMix: 0.18, bowNoise: 0.5,
      };
      for (let idx = 0; idx < vemTracksCfg.length; idx++) {
        const entry = vemTracksCfg[idx];
        const itype = { violin: 0, viola: 1, cello: 2, bass: 3 }[entry.instrument] ?? 0;
        const eng = V.vem_new(sr, itype);
        V.vem_set_seed(eng, entry.seed ?? 12345 + idx * 7919);
        for (const bytes of entry.data || []) {
          const u8 = new Uint8Array(bytes);
          if (!u8.byteLength) continue;
          const ptr = V.vem_alloc(u8.byteLength);
          new Uint8Array(V.memory.buffer, ptr, u8.byteLength).set(u8);
          V.vem_set_patch(eng, ptr, u8.byteLength);
          V.vem_free_mem(ptr, u8.byteLength);
        }
        // msg.gains 按轨道下标索引，不是按 MIDI channel；先按 channel 找到轨道下标
        const ti = msg.tracks.findIndex((t) => t.channel === entry.channel);
        const gain = msg.gains?.[ti] ?? entry.gain ?? 1;
        const g = { E: V, eng, entry, params: Object.assign({}, defaults, entry.params || {}) };
        V.vem_set_master(eng, msg.master);
        V.vem_set_polyphonic(eng, entry.polyphonic ? 1 : 0);
        V.vem_set_gain(eng, gain);
        applyVemParamsSP(g);
        engs.push(g);
      }
    }

    const applyFxSP = (off) => {
      const r = fx.reverb, c = fx.comp, ch = fx.chorus;
      M._msfa_mixer_set_reverb(mixer, off ? 0 : r.mix, r.size, r.damp, r.width);
      M._msfa_mixer_set_comp(mixer, off ? 0 : (c.on ? 1 : 0), c.thr, c.ratio, c.atk, c.rel, c.makeup);
      if (V) {
        for (const g of engs) {
          V.vem_set_reverb(g.eng, off ? 0 : r.mix, r.size, r.damp, r.width);
          V.vem_set_comp(g.eng, off ? 0 : (c.on ? 1 : 0), c.thr, c.ratio, c.atk, c.rel, c.makeup);
        }
      }
      // 合唱：与 worklet 一致，只作用于带 chorus 标记且非 VEM 的通道
      for (const t of msg.tracks) {
        if (t.chorus && !vemChannels.has(t.channel)) {
          M._msfa_mixer_set_chorus(mixer, t.channel, (!off && ch.on) ? 1 : 0, ch.rate, ch.depth, ch.mix, ch.spread);
        }
      }
    };
    applyFxSP(false);

    const song = humanizeApi ? humanizeApi.parseEvents(msg.events) : { events: [], duration: msg.duration };
    let events = song.events;
    let exprs = msg.exprs ? new Float32Array(msg.exprs) : null;
    const computeLeads = (arr) => {
      const out = new Float32Array(arr.length);
      const nextOn = {};
      for (let i = arr.length - 1; i >= 0; i--) {
        const e = arr[i];
        if (e.on) {
          out[i] = nextOn[e.ch] !== undefined ? (nextOn[e.ch] - e.t) * 1000 : 0;
          nextOn[e.ch] = e.t;
        }
      }
      return out;
    };
    let leads = computeLeads(events);

    const state = {
      playing: false,
      pos: 0,
      offset: 0,
      evIndex: 0,
      fade: 0,
      lastReport: 0,
    };
    // 与 worklet 的 trackGain[] 一致：推子实时改，seek 后能恢复当前值
    const trackGains = msg.tracks.map((t, i) => msg.gains?.[i] ?? t.volume);

    const appliedMaster = { value: msg.master };
    const resetSP = () => {
      M._msfa_mixer_reset(mixer);
      for (let i = 0; i < msg.tracks.length; i++) {
        const ch = msg.tracks[i].channel;
        if (vemChannels.has(ch)) continue;
        M._msfa_mixer_set_patch(mixer, ch, patchPtrs[i]);
        M._msfa_mixer_set_gain(mixer, ch, trackGains[i]);
      }
      if (V) {
        for (const g of engs) {
          V.vem_reset(g.eng);
          V.vem_set_master(g.eng, appliedMaster.value);
          V.vem_set_gain(g.eng, g.entry.gain ?? 1);
          applyVemParamsSP(g);
        }
      }
      M._msfa_mixer_set_master(mixer, appliedMaster.value);
      applyFxSP(bypass);
    };

    const outL = M.malloc(64 * 4), outR = M.malloc(64 * 4);
    const voutL = V ? V.vem_alloc(64 * 4) : 0, voutR = V ? V.vem_alloc(64 * 4) : 0;
    const vsumL = new Float32Array(64), vsumR = new Float32Array(64);
    let ml = null, mr = null, vvL = null, vvR = null;
    const getView = (E, ptr, old) =>
      old && old.buffer === E.memory.buffer ? old : new Float32Array(E.memory.buffer, ptr, 64);

    const nodeSP = ctx.createScriptProcessor(4096, 1, 2);
    nodeSP.onaudioprocess = (e) => {
      const L = e.outputBuffer.getChannelData(0);
      const R = e.outputBuffer.getChannelData(1);
      if (!state.playing) {
        L.fill(0);
        R.fill(0);
        return;
      }
      ml = getView(M, outL, ml);
      mr = getView(M, outR, mr);
      for (let base = 0; base < L.length; base += 64) {
        const nowT = state.offset + (state.pos + base) / sr;
        while (state.evIndex < events.length && events[state.evIndex].t <= nowT) {
          const ei = state.evIndex++;
          const ev = events[ei];
          if (vemChannels.has(ev.ch)) {
            for (const g of engs) {
              if (g.entry.channel !== ev.ch) continue;
              if (ev.note < g.entry.minNote || ev.note > g.entry.maxNote) continue;
              if (ev.on) {
                const p = g.params;
                let bow = p.bow ?? 0.5, vib = p.vibrato ?? 0.4;
                let press = p.pressure ?? 1, atk = p.attackMs ?? 8, cents = 0;
                if (exprs) {
                  const b = ei * 5;
                  bow = Math.min(0.95, Math.max(0.05, bow + exprs[b]));
                  vib = Math.max(0, vib * exprs[b + 1]);
                  press = Math.max(0.2, press * exprs[b + 2]);
                  atk = Math.max(1, atk * exprs[b + 3]);
                  cents = exprs[b + 4];
                  if (typeof V.vem_set_expr === "function") {
                    V.vem_set_expr(g.eng, bow, press, vib, atk, cents, leads[ei] || 0);
                  }
                }
                V.vem_note_on(g.eng, ev.note, ev.vel / 127);
              } else {
                V.vem_note_off(g.eng, ev.note);
              }
            }
          } else if (ev.on) {
            M._msfa_mixer_note_on(mixer, ev.ch, ev.note, ev.vel);
          } else {
            M._msfa_mixer_note_off(mixer, ev.ch, ev.note);
          }
        }
        M._msfa_mixer_render(mixer, outL, outR, 64);
        vsumL.fill(0);
        vsumR.fill(0);
        if (V) {
          for (const g of engs) {
            V.vem_render(g.eng, voutL, voutR, 64);
            vvL = getView(V, voutL, vvL);
            vvR = getView(V, voutR, vvR);
            for (let i = 0; i < 64; i++) {
              vsumL[i] += vvL[i];
              vsumR[i] += vvR[i];
            }
          }
        }
        const lim = Math.min(64, L.length - base);
        for (let i = 0; i < lim; i++) {
          if (state.fade < 1) state.fade = Math.min(1, state.fade + 1 / (0.002 * sr));
          L[base + i] = softClip((ml[i] + vsumL[i]) * state.fade);
          R[base + i] = softClip((mr[i] + vsumR[i]) * state.fade);
        }
      }
      state.pos += L.length;
      cur = state.offset + state.pos / sr;
      const now = ctx.currentTime;
      if (now - state.lastReport > 0.05) {
        state.lastReport = now;
        let peak = 0;
        for (let i = 0; i < L.length; i++) peak = Math.max(peak, Math.abs(L[i]), Math.abs(R[i]));
        el.fill.style.width = `${Math.min(100, (cur / duration) * 100)}%`;
        el.knob.style.left = `${Math.min(100, (cur / duration) * 100)}%`;
        el.tcur.textContent = fmt(Math.min(cur, duration));
        el.meter.style.height = `${Math.min(100, peak * 100)}%`;
      }
      if (cur >= duration + 2) {
        state.playing = false;
        playing = false;
        el.play.textContent = "▶";
        el.play.classList.remove("on");
        setStatus("播放结束 · 按播放从头开始", "ok");
      }
    };
    nodeSP.connect(ctx.destination);
    // 某些实现只有在输入端有连接时才稳定触发 onaudioprocess；挂一个静音源保证被 pull
    try {
      const silent = ctx.createOscillator();
      const silentGain = ctx.createGain();
      silentGain.gain.value = 0;
      silent.connect(silentGain).connect(nodeSP);
      silent.start();
    } catch (e) {
      // 连接 destination 已足够时忽略
    }

    sp = {
      sr,
      _node: nodeSP, // 仅供自动化冒烟测试手动触发 onaudioprocess
      handle(cmd) {
        switch (cmd.type) {
          case "play":
            state.playing = true;
            state.fade = 0;
            break;
          case "pause":
            state.playing = false;
            break;
          case "seek":
            resetSP();
            state.offset = Math.max(0, Math.min(cmd.time, duration));
            state.pos = 0;
            let i = 0;
            while (i < events.length && events[i].t < state.offset) i++;
            state.evIndex = i;
            state.fade = 0;
            break;
          case "trackGain": {
            trackGains[cmd.index] = cmd.value;
            const ch = msg.tracks[cmd.index].channel;
            if (vemChannels.has(ch)) {
              for (const g of engs) if (g.entry.channel === ch) {
                g.entry.gain = cmd.value;
                V.vem_set_gain(g.eng, cmd.value);
              }
            } else {
              M._msfa_mixer_set_gain(mixer, ch, cmd.value);
            }
            break;
          }
          case "master":
            appliedMaster.value = cmd.value;
            M._msfa_mixer_set_master(mixer, cmd.value);
            if (V) for (const g of engs) V.vem_set_master(g.eng, cmd.value);
            break;
          case "fx":
            fx = cmd.fx;
            applyFxSP(bypass);
            break;
          case "bypass":
            bypass = cmd.on;
            applyFxSP(bypass);
            break;
          case "vemParam":
            for (const g of engs) {
              Object.assign(g.params, cmd.params || {});
              applyVemParamsSP(g);
            }
            break;
          case "vemTrack": {
            const g = engs[cmd.index];
            if (g) {
              Object.assign(g.params, cmd.params || {});
              applyVemParamsSP(g);
            }
            break;
          }
          case "events": {
            const s = humanizeApi.parseEvents(cmd.events);
            events = s.events;
            exprs = cmd.exprs ? new Float32Array(cmd.exprs) : null;
            leads = computeLeads(events);
            let i = 0;
            const t = state.offset + state.pos / sr;
            while (i < events.length && events[i].t < t) i++;
            state.evIndex = i;
            break;
          }
          default:
            break;
        }
      },
    };

    ready = true;
    el.play.disabled = false;
    if (vemTracksCfg.length) buildVemUI();
    el.meta.textContent = `ScriptProcessor(4096) 兼容模式 · ${V ? `${engs.length} 个 VEM 声部 · ` : ""}${sr} Hz`;
    setStatus(`就绪(SP) · ${sr} Hz · ${events.length} 事件`, "ok");
  } catch (e) {
    console.error(e);
    setStatus("SP 模式初始化失败：" + e.message, "");
    el.status.classList.add("err");
  }
}

function applyVemParamsSP(g) {
  const { E, eng, params: p } = g;
  const call = (fn, ...args) => { if (typeof fn === "function") fn(...args); };
  call(E.vem_set_articulation, eng, p.art | 0);
  call(E.vem_set_bow, eng, p.bow, p.pressure);
  call(E.vem_set_vibrato, eng, p.vibrato, p.vibratoRate);
  call(E.vem_set_envelope, eng, p.attackMs, p.attackVelMs, p.releaseMs, p.releaseSlowMs, p.bowSettleMs, p.vibOnsetMs, 60);
  call(E.vem_set_release_rate, eng, p.releaseRate);
  call(E.vem_set_body, eng, p.bodyMix, 0.6);
  call(E.vem_set_bow_noise, eng, p.bowNoiseOn === 0 ? 0 : 1, p.bowNoise);
  call(E.vem_set_legato, eng, p.legatoStart ?? 0.5, p.legatoRampMs ?? 0, p.legatoAccent ?? 0, p.legatoBoost ?? 0, p.legatoBoostMs ?? 25);
  call(E.vem_set_phrase, eng, p.accent ?? 0, p.accentMs ?? 45, p.bodyMs ?? 120, p.taper ?? 0, p.taperMs ?? 130);
}
