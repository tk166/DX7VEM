// 演奏起伏（humanize）：时间/力度/乐句呼吸。确定性（固定种子），网页与离线共用。
//
// 事件格式与 notes.bin 一致（MFA1）：
//   0 "MFA1" | 4 u32 n | 8 f32 duration | 12.. 每事件 8B：f32 t | u8 ch | u8 note | u8 vel | u8 on
//
// 做法：
//   - 每个音一个很小的时值抖动（开/关用同一个偏移，保持音长）
//   - 两条慢速正弦（约 10–15s 与 3–5s）叠加成"乐句呼吸"的 tempo 偏移
//   - 同一组慢速正弦（不同相位）调制力度，配合每音抖动
// 不改变音高（音高微差在引擎的 per-note 表情里做）。

function mulberry32(seed) {
  let a = seed >>> 0;
  return function () {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function parseEvents(buffer) {
  const b = new Uint8Array(buffer);
  if (String.fromCharCode(b[0], b[1], b[2], b[3]) !== "MFA1") throw new Error("notes 魔数不对");
  const dv = new DataView(buffer);
  const n = dv.getUint32(4, true);
  const duration = dv.getFloat32(8, true);
  const events = [];
  for (let i = 0; i < n; i++) {
    const o = 12 + i * 8;
    events.push({
      t: dv.getFloat32(o, true),
      ch: dv.getUint8(o + 4),
      note: dv.getUint8(o + 5),
      vel: dv.getUint8(o + 6),
      on: dv.getUint8(o + 7) !== 0,
    });
  }
  return { events, duration };
}

export function encodeEvents(events, duration) {
  const buffer = new ArrayBuffer(12 + events.length * 8);
  const dv = new DataView(buffer);
  for (let i = 0; i < 4; i++) dv.setUint8(i, "MFA1".charCodeAt(i));
  dv.setUint32(4, events.length, true);
  dv.setFloat32(8, duration, true);
  for (let i = 0; i < events.length; i++) {
    const e = events[i];
    const o = 12 + i * 8;
    dv.setFloat32(o, e.t, true);
    dv.setUint8(o + 4, e.ch);
    dv.setUint8(o + 5, e.note);
    dv.setUint8(o + 6, e.vel);
    dv.setUint8(o + 7, e.on ? 1 : 0);
  }
  return buffer;
}

/**
 * @param {{events:Array,duration:number}} song
 * @param {Object} opts enabled/seed/timingMs/velocity/phraseTempoMs/phraseDynamics
 */
export function humanizeEvents(song, opts = {}) {
  if (opts.enabled === false) return song;
  const seed = opts.seed ?? 20261117;
  const timingMs = opts.timingMs ?? 12;
  const velAmount = opts.velocity ?? 6;
  const phraseTempoMs = opts.phraseTempoMs ?? 22;
  const phraseDynamics = opts.phraseDynamics ?? 0.06;
  const pitchCents = opts.pitchCents ?? 4;
  const bowJitter = opts.bowJitter ?? 0.05;
  const vibJitter = opts.vibratoJitter ?? 0.12;
  const pressJitter = opts.pressureJitter ?? 0.05;
  const attackJitter = opts.attackJitter ?? 0.1;

  const rand = mulberry32(seed);
  // 两条慢速呼吸：周期与相位随机，但由种子固定
  const p1 = 9 + rand() * 6;   // 9–15 s
  const p2 = 3 + rand() * 2.5; // 3–5.5 s
  const ph1 = rand() * Math.PI * 2;
  const ph2 = rand() * Math.PI * 2;
  const ph3 = rand() * Math.PI * 2;

  // 时间偏移（秒）：两条正弦叠加，保证整体仍单调（幅度远小于曲长）
  const warp = (t) =>
    (phraseTempoMs / 1000) * (Math.sin((2 * Math.PI * t) / p1 + ph1) * 0.7 +
      Math.sin((2 * Math.PI * t) / p2 + ph2) * 0.3);
  const dyn = (t) =>
    1 + phraseDynamics * Math.sin((2 * Math.PI * t) / (p1 * 0.8) + ph3);

  // 1) 每个 note-on 一个抖动，配对的 note-off 用同一个偏移
  //    同时生成每音表情（弓位偏移 / 弓压/揉弦/起音倍率 / 音准 cents）
  const pending = new Map();
  const dt = new Array(song.events.length).fill(0);
  const exprByIndex = new Array(song.events.length).fill(null);
  for (let i = 0; i < song.events.length; i++) {
    const e = song.events[i];
    const key = `${e.ch}:${e.note}`;
    if (e.on) {
      const jitter = ((rand() * 2 - 1) * timingMs) / 1000;
      dt[i] = warp(e.t) + jitter;
      exprByIndex[i] = [
        (rand() * 2 - 1) * bowJitter,
        1 + (rand() * 2 - 1) * vibJitter,
        1 + (rand() * 2 - 1) * pressJitter,
        1 + (rand() * 2 - 1) * attackJitter,
        (rand() * 2 - 1) * pitchCents,
      ];
      if (!pending.has(key)) pending.set(key, []);
      pending.get(key).push(dt[i]);
    } else {
      const q = pending.get(key);
      dt[i] = q && q.length ? q.shift() : warp(e.t);
    }
  }

  // 2) 力度：乐句动态 + 每音抖动
  const out = song.events.map((e, i) => {
    let vel = e.vel;
    if (e.on) {
      vel = Math.round(vel * dyn(e.t) + (rand() * 2 - 1) * velAmount);
      vel = Math.max(1, Math.min(127, vel));
    }
    return { ...e, t: Math.max(0, e.t + dt[i]), vel, expr: exprByIndex[i] };
  });
  out.sort((a, b) => a.t - b.t || (a.on === b.on ? 0 : a.on ? 1 : -1));

  // 表情数组与排序后的事件一一对应（供二进制通道使用）
  const exprs = new Float32Array(out.length * 5);
  for (let i = 0; i < out.length; i++) {
    if (out[i].expr) exprs.set(out[i].expr, i * 5);
  }
  return { events: out, duration: song.duration, exprs };
}

export function humanizeBuffer(buffer, opts = {}) {
  const song = parseEvents(buffer);
  const h = humanizeEvents(song, opts);
  return { buffer: encodeEvents(h.events, h.duration), exprs: h.exprs };
}
