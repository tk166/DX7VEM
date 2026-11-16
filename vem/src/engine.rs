//! 引擎总装：voice 槽位池 / 事件 / 混音 / 参数平滑 / 总线 FX。
//!
//! M0：单声道正弦占位 + 增益平滑 + 软限幅，用来打通
//!      "Rust → 渲染 CLI → 对拍脚本" 这条管线。
//! M1：把 Instrument 换成谱合成；M3：接琴体 IR / 压缩 / 混响。

use crate::articulations::Articulation;
use crate::instrument::Instrument;
use crate::random::Random;
use crate::smooth::{block_coef, smooth_to, soft_clip};

/// 内部渲染块长（与 AudioWorklet 的 64 采样一致）。
pub const BLOCK: usize = 64;
/// 复音槽位数。VEM 用 4 个 division；单声部旋律 8 个足够，顺带兼容和声。
pub const MAX_VOICES: usize = 8;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InstrumentType {
    Violin,
    Viola,
    Cello,
    Bass,
}

impl InstrumentType {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Viola,
            2 => Self::Cello,
            3 => Self::Bass,
            _ => Self::Violin,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Violin => "violin",
            Self::Viola => "viola",
            Self::Cello => "cello",
            Self::Bass => "bass",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "violin" | "vln" => Self::Violin,
            "viola" | "vla" => Self::Viola,
            "cello" | "vc" => Self::Cello,
            "bass" | "cb" => Self::Bass,
            _ => return None,
        })
    }

    pub fn lowest_note(&self) -> i32 {
        match self {
            Self::Violin => 55,
            Self::Viola => 48,
            Self::Cello => 36,
            Self::Bass => 24,
        }
    }

    pub fn highest_note(&self) -> i32 {
        match self {
            Self::Violin => 96,
            Self::Viola => 88,
            Self::Cello => 79,
            Self::Bass => 67,
        }
    }
}

pub struct Engine {
    pub sample_rate: f32,
    pub instrument_type: InstrumentType,
    voices: [Instrument; MAX_VOICES],
    next_age: u64,
    gain: f32,
    gain_target: f32,
    gain_coef: f32,
    pub master: f32,
    pub vibrato_depth: f32,
    pub vibrato_rate: f32,
    pub bow_position: f32,
    pub bow_pressure: f32,
    pub articulation: Articulation,
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
        Self {
            sample_rate,
            instrument_type,
            voices: core::array::from_fn(|_| Instrument::new()),
            next_age: 0,
            gain: 0.0,
            gain_target: 1.0,
            gain_coef: block_coef(12.0, sample_rate, BLOCK),
            master: 1.0,
            vibrato_depth: 0.4,
            vibrato_rate: 5.2,
            bow_position: 0.5,
            bow_pressure: 0.8,
            articulation: Articulation::Arco,
            body_mix: 0.18,
            body_size: 0.6,
            reverb_mix: 0.0,
            comp_on: false,
            bow_noise_mode: 0,
            bow_noise_amount: 0.5,
            rng: Random::with_seed(0x5645_4d01),
        }
    }

    pub fn reset(&mut self) {
        for v in &mut self.voices {
            v.reset();
        }
        self.gain = 0.0;
        self.next_age = 0;
        self.rng.reset(0x5645_4d01);
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }

    pub fn note_on(&mut self, note: i32, velocity: f32) {
        // 1) 空槽 → 2) 正在释放的最旧槽 → 3) 最旧槽
        let mut slot = None;
        for (i, v) in self.voices.iter().enumerate() {
            if !v.is_active() {
                slot = Some(i);
                break;
            }
        }
        if slot.is_none() {
            let mut best = (u64::MAX, 0usize);
            for (i, v) in self.voices.iter().enumerate() {
                if v.is_releasing() && v.age < best.0 {
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
        self.voices[slot.unwrap()].note_on(note, velocity, self.sample_rate, age);
    }

    pub fn note_off(&mut self, note: i32) {
        for v in &mut self.voices {
            if v.is_active() && !v.is_releasing() && v.note == note {
                v.note_off();
            }
        }
    }

    pub fn note_off_all(&mut self) {
        for v in &mut self.voices {
            if v.is_active() {
                v.note_off();
            }
        }
    }

    pub fn set_gain(&mut self, gain: f32) {
        self.gain_target = gain.max(0.0);
    }

    /// 渲染到给定的 L/R 缓冲（长度必须相同；内部按 BLOCK 分块）。
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        out_l[..frames].fill(0.0);
        out_r[..frames].fill(0.0);
        let mut base = 0;
        while base < frames {
            let n = BLOCK.min(frames - base);
            self.render_block(&mut out_l[base..base + n], &mut out_r[base..base + n]);
            base += n;
        }
    }

    fn render_block(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        let mut scratch = [0.0f32; BLOCK];
        let scratch = &mut scratch[..n];
        for v in &mut self.voices {
            v.render_add(scratch, self.sample_rate, self.articulation);
        }
        self.gain = smooth_to(self.gain, self.gain_target, self.gain_coef);
        let amp = self.gain * self.master;
        for i in 0..n {
            let s = soft_clip(scratch[i] * amp);
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
        e.note_on(69, 0.8);
        let l = render_seconds(&mut e, 0.5);
        let peak = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.01, "peak={peak}");
        assert!(l.iter().all(|v| v.is_finite()));
        e.note_off(69);
        let l = render_seconds(&mut e, 2.0);
        assert_eq!(e.active_voices(), 0);
        let tail_peak = l[l.len() / 2..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(tail_peak < 1e-3, "tail={tail_peak}");
    }

    #[test]
    fn voice_stealing_keeps_pool_bounded() {
        let mut e = Engine::new(48000.0, InstrumentType::Violin);
        for n in 60..80 {
            e.note_on(n, 0.7);
        }
        assert_eq!(e.active_voices(), MAX_VOICES);
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
}
