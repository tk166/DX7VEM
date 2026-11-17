//! 引擎总装：voice 池（每音一个 Instrument）+ 控制率包络/弓位/频率 + 参数平滑 + 总线。
//!
//! 控制率默认 100 采样 @48k（= 480 Hz），与参考实现的 `step%100==0` 对齐；
//! 7 种弓法的 transition 时序（起音/弓位归位/滑音/跳弓/拨弦/tremolo）逐条照抄
//! `Division::note_on` / `update_transitions` 的行为，因此在默认参数下可与参考实现
//! 逐窗口（甚至逐采样）对拍。

use crate::articulations::Articulation;
use crate::envelope::EnvelopeParams;
use crate::fft::FftCache;
use crate::instrument::{Instrument, InstrumentType};
use crate::random::Random;
use crate::smooth::{block_coef, smooth_to, soft_clip};
use crate::spectrum::{coef_for, TimbreParams};

pub const BLOCK: usize = 64;
pub const MAX_VOICES: usize = 8;
const MAX_TRANS: usize = 8;

/// 与参考实现一致的默认种子（`ref-instrument --seed 12345`）。
pub const DEFAULT_SEED: u32 = 12345;

/// 单声部 pan = 0.25π，总线缩放 0.01/sqrt(count)（count=1）。
pub const DIRECTOR_SCALE: f32 = 0.01 * core::f32::consts::FRAC_1_SQRT_2;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum TransKind {
    Env,
    Freq,
    Bow,
}

#[derive(Copy, Clone, Debug)]
struct Trans {
    kind: TransKind,
    from: f32,
    to: f32,
    start: u64,
    end: u64,
}

impl Trans {
    /// raised-cosine，与参考实现 `0.5-0.5*cos(π*fraction)` 同式。
    /// 返回 `None` 表示这条 transition 还没开始（参考实现此时不改写参数）。
    #[inline]
    fn value(&self, step: u64) -> Option<f32> {
        if step < self.start {
            return None;
        }
        if step >= self.end {
            return Some(self.to);
        }
        let fraction = (step - self.start) as f32 / (self.end - self.start) as f32;
        let weight2 = 0.5 - 0.5 * (fraction * core::f32::consts::PI).cos();
        let weight1 = 1.0 - weight2;
        Some(weight1 * self.from + weight2 * self.to)
    }
}

struct ControlCtx {
    bow: f32,
    vibrato_depth: f32,
    bend: f32,
    /// = (0.05 + 0.95*volume) * bow_pressure
    volume: f32,
    /// 原始 0..1（tremolo 的公式用这个）
    volume_raw: f32,
    harmonics: bool,
    articulation: Articulation,
    vibrato_onset_ms: f32,
    vibrato_onset_ramp_ms: f32,
    sr: f32,
}

pub struct Voice {
    inst: Instrument,
    active: bool,
    pub note: i32,
    pub velocity: f32,
    pub age: u64,
    releasing: bool,
    articulation: Articulation,
    envelope: f32,
    freq: f32,
    bow_shift: f32,
    trans: [Option<Trans>; MAX_TRANS],
    note_on_step: u64,
    has_sounded: bool,
    tremolo_start: i64,
    tremolo_end: i64,
    tremolo_volume: f32,
    tremolo_down_bow: bool,
    peak: f32,
    silence_ticks: u32,
}

impl Voice {
    fn new(index: usize, sr: f32, itype: InstrumentType, oversample: bool, seed: u32) -> Self {
        Self {
            inst: Instrument::new(itype, index, sr, oversample, seed),
            active: false,
            note: -1,
            velocity: 0.0,
            age: 0,
            releasing: false,
            articulation: Articulation::Arco,
            envelope: 0.0,
            freq: 440.0,
            bow_shift: 0.0,
            trans: [None; MAX_TRANS],
            note_on_step: 0,
            has_sounded: false,
            tremolo_start: 0,
            tremolo_end: 0,
            tremolo_volume: 1.0,
            tremolo_down_bow: true,
            peak: 0.0,
            silence_ticks: 0,
        }
    }

    fn push_trans(&mut self, t: Trans) {
        for slot in self.trans.iter_mut() {
            if slot.is_none() {
                *slot = Some(t);
                return;
            }
        }
        // 理论上不会满；应付一下，替换最后一个
        self.trans[MAX_TRANS - 1] = Some(t);
    }

    fn add_trans(
        &mut self,
        kind: TransKind,
        delay: u64,
        duration: u64,
        from: f32,
        to: f32,
        step: u64,
    ) {
        let start = step + delay;
        self.push_trans(Trans {
            kind,
            from,
            to,
            start,
            end: start + duration,
        });
    }

    /// 参考实现的 `add_envelope_transition`：先删除所有包络 transition。
    fn add_env_transition(&mut self, duration: u64, to: f32, step: u64) {
        let from = self.envelope;
        for t in self.trans.iter_mut() {
            if matches!(t, Some(t) if t.kind == TransKind::Env) {
                *t = None;
            }
        }
        self.add_trans(TransKind::Env, 0, duration, from, to, step);
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        note: i32,
        velocity: f32,
        step: u64,
        age: u64,
        legato: bool,
        ctx: &ControlCtx,
        env: &EnvelopeParams,
        articulation: Articulation,
        timbre: TimbreParams,
        rng: &mut Random,
    ) {
        let prev_note = self.note;
        let prev_freq = self.freq;

        self.active = true;
        self.note = note;
        self.velocity = velocity;
        self.age = age;
        self.releasing = false;
        self.articulation = articulation;
        self.note_on_step = step;
        self.has_sounded = false;
        self.peak = 0.0;
        self.silence_ticks = 0;
        self.trans = [None; MAX_TRANS];
        self.freq = midi_to_frequency(note);

        let s48 = |samples: i64| -> u64 {
            (samples as f32 * ctx.sr / 48000.0).round().max(0.0) as u64
        };

        self.inst.set_timbre(timbre);
        self.inst.set_frequency(self.freq * ctx.bend);
        self.inst.note_on(note, articulation);

        match articulation {
            Articulation::Arco | Articulation::Glissando => {
                if legato && articulation == Articulation::Glissando {
                    // 滑音：从上一个音高滑到新音高
                    let interval = (prev_note - note).abs().max(7) as f32;
                    let slide_time = 200 + (interval * 2000.0 * (1.0 - velocity)) as i64;
                    let end_frequency = self.freq;
                    self.freq = prev_freq;
                    self.inst.set_frequency(self.freq * ctx.bend);
                    self.add_trans(
                        TransKind::Freq,
                        0,
                        s48(slide_time),
                        prev_freq,
                        end_frequency,
                        step,
                    );
                }
                let attack_time = env.attack_samples(velocity, ctx.sr).max(0) as u64;
                let start_env = 0.5 * self.envelope;
                self.envelope = start_env;
                self.add_trans(TransKind::Env, 0, attack_time, start_env, 1.0, step);
                if !legato {
                    self.bow_shift = -ctx.bow;
                    let settle = env.bow_settle_samples(ctx.sr).max(0) as u64;
                    self.add_trans(TransKind::Bow, 0, settle, -ctx.bow, 0.0, step);
                }
            }
            Articulation::Marcato => {
                let attack_time = 1000 + (5000.0 * (1.0 - velocity)) as i64;
                let peak = 1.0 + 3.0 * velocity;
                self.add_env_transition(s48(attack_time), peak, step);
                self.add_trans(
                    TransKind::Env,
                    s48(attack_time),
                    s48(2 * attack_time),
                    peak,
                    1.0,
                    step,
                );
                if !legato {
                    self.bow_shift = -ctx.bow;
                    let settle = env.bow_settle_samples(ctx.sr).max(0) as u64;
                    self.add_trans(TransKind::Bow, 0, settle, -ctx.bow, 0.0, step);
                }
            }
            Articulation::Spiccato => {
                let hold_time = 2750 + (rng.get_int() as i64) % 500;
                let peak = 0.05 + 4.0 * velocity;
                self.add_env_transition(0, peak, step);
                self.envelope = peak;
                self.add_trans(TransKind::Env, s48(hold_time), 0, peak, 0.0, step);

                // 弓击弦造成瞬时音高偏移
                let end_frequency = self.freq;
                let start_frequency = 1.02 * end_frequency;
                self.freq = start_frequency;
                self.add_trans(
                    TransKind::Freq,
                    0,
                    s48(2000),
                    start_frequency,
                    end_frequency,
                    step,
                );
            }
            Articulation::Pizzicato | Articulation::ColLegno => {
                let peak = 1.0 + 15.0 * velocity;
                self.add_env_transition(0, peak, step);
                self.envelope = peak;
                let end_frequency = self.freq;
                let period = 48000.0 / end_frequency;
                let hold_time = 101i64.max((2.0 * period) as i64);
                self.add_trans(TransKind::Env, s48(hold_time), 0, peak, 0.0, step);

                let start_frequency = 1.015 * end_frequency;
                self.freq = start_frequency;
                self.add_trans(
                    TransKind::Freq,
                    0,
                    s48(hold_time),
                    start_frequency,
                    end_frequency,
                    step,
                );
            }
            Articulation::Tremolo => {
                self.add_env_transition(0, 1.0, step);
                self.envelope = 1.0;
                let coef = coef_for(self.inst.instrument_type);
                self.tremolo_start = step as i64;
                self.tremolo_end = step as i64
                    + s48(coef.tremolo_length as i64) as i64
                    + (rng.get_int() as i64) % 500;
                self.tremolo_volume = 1.0 + 0.5 * rng.get_uniform();
                self.tremolo_down_bow = true;
            }
        }

        // 参考实现在 note_on 末尾会立刻 update_transitions 一次
        self.update_control(step, ctx, env, rng);
    }

    fn release(
        &mut self,
        step: u64,
        ctx: &ControlCtx,
        env: &EnvelopeParams,
        release_rate: f32,
        rng: &mut Random,
    ) {
        if self.releasing {
            return;
        }
        self.releasing = true;
        match self.articulation {
            // 这些弓法在 note_on 时已经排好了衰减，松键不再改动
            Articulation::Spiccato | Articulation::Pizzicato | Articulation::ColLegno => {}
            _ => {
                let release_time = env.release_samples(release_rate, ctx.sr).max(1) as u64;
                self.add_env_transition(release_time, 0.0, step);
                self.update_control(step, ctx, env, rng);
            }
        }
    }

    fn update_control(
        &mut self,
        step: u64,
        ctx: &ControlCtx,
        _env: &EnvelopeParams,
        rng: &mut Random,
    ) {
        if !self.active {
            return;
        }

        let mut volume_changed = false;
        let mut freq_changed = false;
        let mut bow_changed = false;
        for t in self.trans.iter().flatten() {
            let Some(v) = t.value(step) else { continue };
            match t.kind {
                TransKind::Env => {
                    self.envelope = v;
                    volume_changed = true;
                }
                TransKind::Freq => {
                    self.freq = v;
                    freq_changed = true;
                }
                TransKind::Bow => {
                    self.bow_shift = v;
                    bow_changed = true;
                }
            }
        }

        let tremolo = ctx.articulation == Articulation::Tremolo;
        if tremolo {
            volume_changed = true;
            freq_changed = true;
            bow_changed = true;
        }

        let coef = coef_for(self.inst.instrument_type);
        let stepi = step as i64;

        // ---- volume（含 tremolo 的 3 段式） ----
        if volume_changed {
            let mut vol = ctx.volume * self.envelope;
            if tremolo {
                if stepi > self.tremolo_end {
                    vol = 0.0;
                    self.tremolo_volume =
                        0.5 * (self.tremolo_volume + 1.0 + 0.5 * rng.get_uniform());
                    let space = (coef.tremolo_space as f32 * ctx.sr / 48000.0) as i64;
                    if !self.tremolo_down_bow {
                        self.tremolo_start = stepi + (0.8 * space as f32) as i64;
                    } else {
                        self.tremolo_start = stepi + space;
                    }
                    let length = (coef.tremolo_length as f32 * ctx.sr / 48000.0) as i64;
                    let extra = (800.0 * ctx.volume_raw) as i64;
                    self.tremolo_end =
                        self.tremolo_start + length + extra + (rng.get_int() as i64) % 500;
                    self.tremolo_down_bow = !self.tremolo_down_bow;
                } else if stepi < self.tremolo_start {
                    vol = 0.0;
                } else {
                    let x = (stepi - self.tremolo_start) as f32
                        / (self.tremolo_end - self.tremolo_start) as f32;
                    vol *= self.tremolo_volume
                        * 3.0
                        * (x - x * x)
                        * if self.tremolo_down_bow { 1.0 } else { 0.8 };
                }
            }
            self.inst.set_volume(vol);

            let mut vib_scale = 1.0f32;
            if ctx.vibrato_onset_ms > 0.0 {
                let t_ms = (step - self.note_on_step) as f32 * 1000.0 / ctx.sr;
                let ramp = ctx.vibrato_onset_ramp_ms.max(1.0);
                vib_scale = ((t_ms - ctx.vibrato_onset_ms) / ramp).clamp(0.0, 1.0);
            }
            self.inst
                .set_vibrato_amplitude(0.01 * ctx.vibrato_depth * self.envelope * vib_scale);
        }

        // ---- frequency ----
        if freq_changed {
            let mut freq = self.freq * ctx.bend;
            if tremolo {
                let freq_delta = 0.014 * freq * ctx.volume_raw;
                let low_freq = freq - 0.5 * freq_delta;
                if stepi < self.tremolo_start {
                    let x = (self.tremolo_start - stepi) as f32 / coef.tremolo_space as f32;
                    freq = low_freq + x * freq_delta;
                } else {
                    let x = (stepi - self.tremolo_start) as f32
                        / (self.tremolo_end - self.tremolo_start) as f32;
                    freq = low_freq + x * freq_delta;
                }
            }
            self.inst.set_frequency(freq);
        }

        // ---- bow position ----
        if bow_changed {
            let mut pos = (ctx.bow + self.bow_shift).max(0.0);
            if tremolo && stepi > self.tremolo_start && self.tremolo_down_bow {
                let x = (stepi - self.tremolo_start) as f32
                    / (self.tremolo_end - self.tremolo_start) as f32;
                pos *= x;
            }
            self.inst.set_bow_position(pos);
        }

        self.inst.set_harmonics(ctx.harmonics);

        // 释放后彻底静音就回收槽位
        if self.releasing && self.peak < 1e-5 {
            self.silence_ticks += 1;
            if self.silence_ticks >= 24 {
                self.active = false;
            }
        } else if self.peak >= 1e-5 {
            self.silence_ticks = 0;
        }
        self.peak = 0.0;
    }
}

pub struct Engine {
    pub sample_rate: f32,
    pub instrument_type: InstrumentType,
    voices: [Voice; MAX_VOICES],
    fft: FftCache,
    next_age: u64,
    step: u64,
    control_samples: u64,
    gain: f32,
    gain_target: f32,
    gain_coef: f32,
    pub master: f32,
    /// 总线缩放：默认 = 参考实现 Director 的 0.01×pan；raw 对拍时设 1.0
    pub output_gain: f32,
    pub safe_clip: bool,
    pub vibrato_depth: f32,
    pub vibrato_rate: f32,
    pub bow_position: f32,
    pub bow_pressure: f32,
    pub volume: f32,
    pub bend: f32,
    pub release_rate: f32,
    pub articulation: Articulation,
    pub envelope: EnvelopeParams,
    pub polyphonic: bool,
    pub harmonics: bool,
    pub timbre: TimbreParams,
    pub oversample: bool,
    oversample_mode: i32,
    pub seed: u32,
    pub body_mix: f32,
    pub body_size: f32,
    pub reverb_mix: f32,
    pub comp_on: bool,
    pub bow_noise_mode: i32,
    pub bow_noise_amount: f32,
    pub rng: Random,
}

impl Engine {
    pub fn new(sample_rate: f32, instrument_type: InstrumentType) -> Self {
        let oversample = instrument_type == InstrumentType::Violin;
        let mut e = Self {
            sample_rate,
            instrument_type,
            voices: core::array::from_fn(|_| {
                Voice::new(0, sample_rate, instrument_type, false, 0)
            }),
            fft: FftCache::new(),
            next_age: 0,
            step: 0,
            control_samples: (100.0 * sample_rate / 48000.0).round().max(1.0) as u64,
            gain: 1.0,
            gain_target: 1.0,
            gain_coef: block_coef(12.0, sample_rate, BLOCK),
            master: 1.0,
            output_gain: DIRECTOR_SCALE,
            safe_clip: false,
            vibrato_depth: 0.4,
            vibrato_rate: 5.2,
            bow_position: 0.5,
            bow_pressure: 1.0,
            volume: 1.0,
            bend: 1.0,
            release_rate: 0.5,
            articulation: Articulation::Arco,
            envelope: EnvelopeParams::default(),
            polyphonic: false,
            harmonics: false,
            timbre: TimbreParams::default(),
            oversample,
            oversample_mode: 0,
            seed: DEFAULT_SEED,
            body_mix: 0.18,
            body_size: 0.6,
            reverb_mix: 0.0,
            comp_on: false,
            bow_noise_mode: 0,
            bow_noise_amount: 0.5,
            rng: Random::with_seed(DEFAULT_SEED),
        };
        e.rebuild_voices();
        // 与参考实现 Division 的随机序列对齐：初始化时先取一次 uniform（分配噪声起点用）
        e.rng.reset(e.seed);
        e.rng.get_uniform();
        e
    }

    fn rebuild_voices(&mut self) {
        let it = self.instrument_type;
        let sr = self.sample_rate;
        let ov = self.oversample;
        let seed = self.seed;
        self.voices = core::array::from_fn(|i| Voice::new(i, sr, it, ov, seed));
    }

    pub fn reset(&mut self) {
        self.step = 0;
        self.next_age = 0;
        self.gain = 1.0;
        self.rng.reset(self.seed);
        self.rng.get_uniform();
        self.rebuild_voices();
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    pub fn set_oversample(&mut self, mode: i32) {
        self.oversample_mode = mode;
        self.oversample = match mode {
            1 => true,
            2 => false,
            _ => self.instrument_type == InstrumentType::Violin,
        };
        self.rebuild_voices();
    }

    pub fn oversample_mode(&self) -> i32 {
        self.oversample_mode
    }

    pub fn control_samples(&self) -> u64 {
        self.control_samples
    }

    fn ctx(&self) -> ControlCtx {
        ControlCtx {
            bow: self.bow_position,
            vibrato_depth: self.vibrato_depth,
            bend: self.bend,
            volume: (0.05 + 0.95 * self.volume) * self.bow_pressure,
            volume_raw: self.volume,
            harmonics: self.harmonics,
            articulation: self.articulation,
            vibrato_onset_ms: self.envelope.vibrato_onset_ms,
            vibrato_onset_ramp_ms: self.envelope.vibrato_onset_ramp_ms,
            sr: self.sample_rate,
        }
    }

    pub fn note_on(&mut self, note: i32, velocity: f32) {
        let velocity = velocity.clamp(0.0, 1.0);
        let mut slot = None;
        let mut legato = false;
        if !self.polyphonic {
            slot = Some(0);
            legato = self.voices[0].active && !self.voices[0].releasing;
        }
        if slot.is_none() {
            for (i, v) in self.voices.iter().enumerate() {
                if !v.active {
                    slot = Some(i);
                    break;
                }
            }
        }
        if slot.is_none() {
            let mut best = (u64::MAX, 0usize);
            for (i, v) in self.voices.iter().enumerate() {
                if v.releasing && v.age < best.0 {
                    best = (v.age, i);
                }
            }
            if best.0 == u64::MAX {
                let mut oldest = (u64::MAX, 0usize);
                for (i, v) in self.voices.iter().enumerate() {
                    if v.age < oldest.0 {
                        oldest = (v.age, i);
                    }
                }
                best = oldest;
            }
            slot = Some(best.1);
        }
        let age = self.next_age;
        self.next_age += 1;
        let ctx = self.ctx();
        let slot = slot.unwrap();
        let env = self.envelope;
        let art = self.articulation;
        let timbre = self.timbre;
        self.voices[slot].start(
            note,
            velocity,
            self.step,
            age,
            legato,
            &ctx,
            &env,
            art,
            timbre,
            &mut self.rng,
        );
    }

    pub fn note_off(&mut self, note: i32) {
        let ctx = self.ctx();
        let step = self.step;
        let release_rate = self.release_rate;
        let env = self.envelope;
        for v in self.voices.iter_mut() {
            if v.active && !v.releasing && v.note == note {
                v.release(step, &ctx, &env, release_rate, &mut self.rng);
            }
        }
    }

    pub fn note_off_all(&mut self) {
        let ctx = self.ctx();
        let step = self.step;
        let release_rate = self.release_rate;
        let env = self.envelope;
        for v in self.voices.iter_mut() {
            if v.active && !v.releasing {
                v.release(step, &ctx, &env, release_rate, &mut self.rng);
            }
        }
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain_target = gain.max(0.0);
    }

    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        let mut base = 0;
        while base < frames {
            let n = BLOCK.min(frames - base);
            self.render_block(&mut out_l[base..base + n], &mut out_r[base..base + n]);
            base += n;
        }
    }

    fn render_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        self.gain = smooth_to(self.gain, self.gain_target, self.gain_coef);
        let amp = self.gain * self.master * self.output_gain;
        let control = self.control_samples;
        let ctx = self.ctx();
        let env = self.envelope;
        let fft = &mut self.fft;
        let rng = &mut self.rng;
        for i in 0..n {
            self.step += 1;
            if self.step % control == 0 {
                for v in self.voices.iter_mut() {
                    v.update_control(self.step, &ctx, &env, rng);
                }
            }
            let mut sum = 0.0f32;
            for v in self.voices.iter_mut() {
                if v.active {
                    // 参考实现 Division::generate：音量为 0 时直接跳过，不推进乐器内部状态
                    if !v.has_sounded {
                        if v.inst.get_volume() > 0.0 {
                            v.has_sounded = true;
                        } else {
                            continue;
                        }
                    }
                    let s = v.inst.generate(fft);
                    let a = s.abs();
                    if a > v.peak {
                        v.peak = a;
                    }
                    sum += s;
                }
            }
            let mut s = sum * amp;
            if self.safe_clip {
                s = soft_clip(s);
            }
            out_l[i] = s;
            out_r[i] = s;
        }
    }
}

#[inline]
pub fn midi_to_frequency(note: i32) -> f32 {
    440.0 * (2.0f32).powf((note - 69) as f32 / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_seconds(e: &mut Engine, seconds: f32) -> Vec<f32> {
        let frames = (seconds * e.sample_rate) as usize;
        let mut l = vec![0.0f32; frames];
        let mut r = vec![0.0f32; frames];
        e.render(&mut l, &mut r);
        l
    }

    #[test]
    fn note_on_produces_audio_and_note_off_decays() {
        let mut e = Engine::new(48000.0, InstrumentType::Violin);
        e.output_gain = 1.0;
        e.note_on(69, 0.8);
        let l = render_seconds(&mut e, 0.5);
        let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 1e-3, "peak={peak}");
        assert!(l.iter().all(|v| v.is_finite()));
        e.note_off(69);
        let l = render_seconds(&mut e, 2.0);
        assert_eq!(e.active_voices(), 0);
        let tail_peak = l[l.len() * 3 / 4..]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(tail_peak < peak * 1e-4, "tail={tail_peak} peak={peak}");
    }

    #[test]
    fn voice_stealing_keeps_pool_bounded() {
        let mut e = Engine::new(48000.0, InstrumentType::Violin);
        e.polyphonic = true;
        for n in 60..80 {
            e.note_on(n, 0.7);
        }
        assert_eq!(e.active_voices(), MAX_VOICES);
    }

    #[test]
    fn monophonic_reuses_one_voice() {
        let mut e = Engine::new(48000.0, InstrumentType::Violin);
        for n in 60..70 {
            e.note_on(n, 0.7);
        }
        assert_eq!(e.active_voices(), 1);
    }

    #[test]
    fn same_seed_same_output() {
        let mut a = Engine::new(48000.0, InstrumentType::Cello);
        let mut b = Engine::new(48000.0, InstrumentType::Cello);
        a.note_on(48, 0.9);
        b.note_on(48, 0.9);
        let la = render_seconds(&mut a, 0.2);
        let lb = render_seconds(&mut b, 0.2);
        assert_eq!(la, lb);
    }

    #[test]
    fn different_seed_different_output() {
        let mut a = Engine::new(48000.0, InstrumentType::Violin);
        let mut b = Engine::new(48000.0, InstrumentType::Violin);
        b.seed = 777;
        b.reset();
        a.note_on(69, 0.8);
        b.note_on(69, 0.8);
        let la = render_seconds(&mut a, 0.2);
        let lb = render_seconds(&mut b, 0.2);
        assert_ne!(la, lb);
    }

    #[test]
    fn all_articulations_produce_sound() {
        for &art in &[
            Articulation::Arco,
            Articulation::Marcato,
            Articulation::Glissando,
            Articulation::Spiccato,
            Articulation::Pizzicato,
            Articulation::ColLegno,
            Articulation::Tremolo,
        ] {
            let mut e = Engine::new(48000.0, InstrumentType::Violin);
            e.output_gain = 1.0;
            e.articulation = art;
            e.note_on(69, 0.8);
            let l = render_seconds(&mut e, 0.5);
            let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 1e-4, "{} peak={peak}", art.name());
            assert!(l.iter().all(|v| v.is_finite()), "{} 有 NaN", art.name());
        }
    }

    #[test]
    fn spiccato_and_pizzicato_decay_without_note_off() {
        for &art in &[Articulation::Spiccato, Articulation::Pizzicato] {
            let mut e = Engine::new(48000.0, InstrumentType::Violin);
            e.output_gain = 1.0;
            e.articulation = art;
            e.note_on(69, 0.8);
            let l = render_seconds(&mut e, 2.0);
            let head = l[..4800].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let tail = l[l.len() - 4800..]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(head > 1e-3, "{} head={head}", art.name());
            assert!(tail < head * 1e-3, "{} tail={tail} head={head}", art.name());
        }
    }
}
