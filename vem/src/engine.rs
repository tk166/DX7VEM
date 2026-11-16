//! 引擎总装：voice 池（每音一个 Instrument）+ 控制率包络/弓位 + 参数平滑 + 总线。
//!
//! 控制率默认 100 采样 @48k（= 480 Hz），与参考实现的 `step%100==0` 对齐；
//! 包络在控制 tick 上用 raised-cosine 插值，因此同种子、同参数下可与
//! `ref-instrument` 逐窗口（甚至逐采样）对拍。

use crate::articulations::Articulation;
use crate::envelope::EnvelopeParams;
use crate::fft::FftCache;
use crate::instrument::{Instrument, InstrumentType};
use crate::random::Random;
use crate::smooth::{block_coef, smooth_to, soft_clip};
use crate::spectrum::TimbreParams;

pub const BLOCK: usize = 64;
pub const MAX_VOICES: usize = 8;

/// 与参考实现一致的默认种子（`ref-instrument --seed 12345`）。
pub const DEFAULT_SEED: u32 = 12345;

/// 单声部 pan = 0.25π，总线缩放 0.01/sqrt(count)（count=1）。
pub const DIRECTOR_SCALE: f32 = 0.01 * core::f32::consts::FRAC_1_SQRT_2;

#[derive(Copy, Clone, Debug)]
struct Trans {
    from: f32,
    to: f32,
    start: u64,
    end: u64,
}

impl Trans {
    /// raised-cosine，与参考实现 `0.5-0.5*cos(π*fraction)` 同式。
    #[inline]
    fn value(&self, step: u64) -> f32 {
        if step <= self.start {
            return self.from;
        }
        if step >= self.end {
            return self.to;
        }
        let fraction = (step - self.start) as f32 / (self.end - self.start) as f32;
        let weight2 = 0.5 - 0.5 * (fraction * core::f32::consts::PI).cos();
        let weight1 = 1.0 - weight2;
        weight1 * self.from + weight2 * self.to
    }
}

struct ControlCtx {
    bow: f32,
    vibrato_depth: f32,
    bend: f32,
    volume: f32,
    harmonics: bool,
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
    envelope: f32,
    env_trans: Option<Trans>,
    bow_shift: f32,
    bow_trans: Option<Trans>,
    freq: f32,
    note_on_step: u64,
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
            envelope: 0.0,
            env_trans: None,
            bow_shift: 0.0,
            bow_trans: None,
            freq: 440.0,
            note_on_step: 0,
            peak: 0.0,
            silence_ticks: 0,
        }
    }

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
    ) {
        self.active = true;
        self.note = note;
        self.velocity = velocity;
        self.age = age;
        self.releasing = false;
        self.note_on_step = step;
        self.peak = 0.0;
        self.silence_ticks = 0;
        self.freq = 440.0 * (2.0f32).powf((note - 69) as f32 / 12.0);

        self.inst.set_timbre(timbre);
        self.inst.set_frequency(self.freq * ctx.bend);
        self.inst.note_on(note, articulation);

        // 参考实现 Arco：非 legato 从 0 起音并把弓位从"贴马"归位；
        // legato 从当前包络的一半接上（换弓感），弓位不动。
        let start_env = if legato { 0.5 * self.envelope } else { 0.0 };
        if !legato {
            self.bow_shift = -ctx.bow;
            let settle = env.bow_settle_samples(ctx.sr).max(0) as u64;
            self.bow_trans = Some(Trans {
                from: -ctx.bow,
                to: 0.0,
                start: step,
                end: step + settle,
            });
        } else {
            self.bow_trans = None;
        }

        self.envelope = start_env;
        let attack = env.attack_samples(velocity, ctx.sr).max(0) as u64;
        self.env_trans = Some(Trans {
            from: start_env,
            to: 1.0,
            start: step,
            end: step + attack,
        });

        // 参考实现在 note_on 时立刻 update_transitions 一次
        self.inst.set_volume(start_env * ctx.volume);
        self.inst
            .set_bow_position((ctx.bow + self.bow_shift).max(0.0));
        self.inst
            .set_vibrato_amplitude(0.01 * ctx.vibrato_depth * start_env);
    }

    fn release(&mut self, step: u64, ctx: &ControlCtx, env: &EnvelopeParams, release_rate: f32) {
        if self.releasing {
            return;
        }
        self.releasing = true;
        let samples = env.release_samples(release_rate, ctx.sr).max(1) as u64;
        self.env_trans = Some(Trans {
            from: self.envelope,
            to: 0.0,
            start: step,
            end: step + samples,
        });
    }

    fn update_control(&mut self, step: u64, ctx: &ControlCtx) {
        if !self.active {
            return;
        }
        if let Some(t) = self.env_trans {
            self.envelope = t.value(step);
        }
        if let Some(t) = self.bow_trans {
            self.bow_shift = t.value(step);
        }
        let env = self.envelope;
        let vol = ctx.volume * env;
        self.inst.set_volume(vol);
        self.inst
            .set_bow_position((ctx.bow + self.bow_shift).max(0.0));
        self.inst.set_frequency(self.freq * ctx.bend);
        self.inst.set_harmonics(ctx.harmonics);

        // 揉弦：默认 onset=0 → 与参考实现完全一致
        let mut vibrato_scale = 1.0f32;
        if ctx.vibrato_onset_ms > 0.0 {
            let t_ms = (step - self.note_on_step) as f32 * 1000.0 / ctx.sr;
            let ramp = ctx.vibrato_onset_ramp_ms.max(1.0);
            vibrato_scale = ((t_ms - ctx.vibrato_onset_ms) / ramp).clamp(0.0, 1.0);
        }
        self.inst
            .set_vibrato_amplitude(0.01 * ctx.vibrato_depth * env * vibrato_scale);

        // 释放后彻底静音就回收槽位
        if self.releasing {
            if self.peak < 1e-5 {
                self.silence_ticks += 1;
            } else {
                self.silence_ticks = 0;
            }
            if self.silence_ticks >= 24 {
                self.active = false;
            }
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
            voices: core::array::from_fn(|_| Voice::new(0, sample_rate, instrument_type, false, 0)),
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
            harmonics: self.harmonics,
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
            // 参考实现默认单声部：所有音都进 division 0（同一 Instrument，换音时旧谱转尾巴）
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
        self.voices[slot].start(
            note,
            velocity,
            self.step,
            age,
            legato,
            &ctx,
            &self.envelope,
            self.articulation,
            self.timbre,
        );
    }

    pub fn note_off(&mut self, note: i32) {
        let ctx = self.ctx();
        let step = self.step;
        let release_rate = self.release_rate;
        let env = self.envelope;
        for v in self.voices.iter_mut() {
            if v.active && !v.releasing && v.note == note {
                v.release(step, &ctx, &env, release_rate);
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
                v.release(step, &ctx, &env, release_rate);
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
        let fft = &mut self.fft;
        for i in 0..n {
            self.step += 1;
            if self.step % control == 0 {
                for v in self.voices.iter_mut() {
                    v.update_control(self.step, &ctx);
                }
            }
            let mut sum = 0.0f32;
            for v in self.voices.iter_mut() {
                if v.active {
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
}
