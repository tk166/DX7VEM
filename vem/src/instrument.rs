//! 单声部弦乐乐器：谱激励 + 谐波阻尼 + 周期重合成（独立实现）。
//!
//! 与参考实现（Viola Ex Machina 的 `Instrument`）的行为一一对应：
//! 每过一个"周期"重新生成一次半谱（累加式），做一次实数 IFFT，得到一个周期的波形并循环播放；
//! 换音时旧谱被移进 `DecayingNote` 独立衰减；小提琴内部 2× 过采样。
//!
//! 为便于逐采样对拍，随机数消费的**顺序**也与参考一致（同种子 → 同随机序列）。

use core::f32::consts::PI;

use crate::articulations::Articulation;
use crate::fft::FftCache;
use crate::filter::Lowpass;
use crate::random::Random;
use crate::spectrum::{coef_for, TimbreParams, VoiceCoef};

pub const MAX_DECAYING: usize = 8;

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

pub struct Instrument {
    pub instrument_type: InstrumentType,
    coef: VoiceCoef,
    timbre: TimbreParams,

    // ---- 参数（引擎每控制周期写一次）----
    volume: f32,
    frequency: f32,
    bow_position: f32,
    harmonics: bool,
    vibrato_amplitude: f32,

    // ---- 状态 ----
    vibrato_frequency_drift_amplitude: f32,
    vibrato_amplitude_drift_amplitude: f32,
    vibrato_phase: f32,
    vibrato_amplitude_drift: f32,
    frequency_drift_amplitude: f32,
    frequency_drift: f32,
    internal_sr: f32,
    output_sr: f32,
    spectrum_re: Vec<f32>,
    spectrum_im: Vec<f32>,
    output_buffer: Vec<f32>,
    spectrum_size: usize,
    output_size: usize,
    output_position: usize,
    period: f32,
    period_offset: f32,
    random: Random,
    lowpass: Lowpass,
    decaying: [DecayingNote; MAX_DECAYING],
    decaying_seq: u64,
    start_new_note: bool,
    last_note: i32,
    last_articulation: Articulation,
}

impl Instrument {
    pub fn new(
        instrument_type: InstrumentType,
        index: usize,
        output_sr: f32,
        oversample: bool,
        seed: u32,
    ) -> Self {
        let coef = coef_for(instrument_type);
        let factor = if oversample { coef.oversample } else { 1.0 };
        let internal_sr = output_sr * factor;
        let mut random = Random::with_seed(seed);
        // 顺序必须与参考一致：先 vibrato_amplitude_drift，再 frequency_drift
        let vibrato_amplitude_drift = random.get_normal();
        let frequency_drift = random.get_normal();
        Self {
            instrument_type,
            coef,
            timbre: TimbreParams::default(),
            volume: 1.0,
            frequency: 440.0,
            bow_position: 0.5,
            harmonics: false,
            vibrato_amplitude: 0.0,
            vibrato_frequency_drift_amplitude: 0.1,
            vibrato_amplitude_drift_amplitude: 0.4,
            vibrato_phase: 0.3 * index as f32,
            vibrato_amplitude_drift,
            frequency_drift_amplitude: 0.002,
            frequency_drift,
            internal_sr,
            output_sr,
            spectrum_re: Vec::new(),
            spectrum_im: Vec::new(),
            output_buffer: Vec::new(),
            spectrum_size: 0,
            output_size: 0,
            output_position: 0,
            period: 0.0,
            period_offset: 0.0,
            random,
            lowpass: Lowpass::new(5000.0, output_sr),
            decaying: core::array::from_fn(|_| DecayingNote::new()),
            decaying_seq: 0,
            start_new_note: false,
            last_note: 0,
            last_articulation: Articulation::Arco,
        }
    }

    pub fn reset(&mut self, seed: u32) {
        let it = self.instrument_type;
        let sr = self.output_sr;
        let coef = self.coef;
        let oversample = self.internal_sr > sr + 0.5;
        let timbre = self.timbre;
        *self = Self::new(it, 0, sr, oversample, seed);
        self.timbre = timbre;
        self.coef = coef;
    }

    pub fn set_timbre(&mut self, p: TimbreParams) {
        self.timbre = p;
        self.update_filter();
    }

    pub fn timbre(&self) -> TimbreParams {
        self.timbre
    }

    pub fn set_harmonics(&mut self, harmonics: bool) {
        self.harmonics = harmonics;
        self.update_filter();
    }

    pub fn set_frequency(&mut self, frequency: f32) {
        self.frequency = frequency;
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
    }

    pub fn get_volume(&self) -> f32 {
        self.volume
    }

    pub fn set_bow_position(&mut self, bow_position: f32) {
        self.bow_position = bow_position;
    }

    pub fn set_vibrato_amplitude(&mut self, amplitude: f32) {
        self.vibrato_amplitude = amplitude;
    }

    pub fn note_on(&mut self, note: i32, articulation: Articulation) {
        if note != self.last_note {
            self.start_new_note = true;
            self.last_note = note;
        }
        self.last_articulation = articulation;
        self.update_filter();
    }

    fn update_filter(&mut self) {
        let note_freq = 440.0 * (2.0f32).powf((self.last_note - 69) as f32 / 12.0);
        let multiplier = if self.harmonics { 4.0 } else { 1.0 };
        let cutoff = (12.0 * note_freq * multiplier).min(20000.0)
            * (self.output_sr / self.internal_sr)
            * self.timbre.cutoff_scale;
        self.lowpass.set_cutoff(cutoff);
    }

    /// 生成一个输出采样（内部按需过采样）。
    pub fn generate(&mut self, fft: &mut FftCache) -> f32 {
        if (self.internal_sr - self.output_sr).abs() < 0.5 {
            self.generate_internal(fft)
        } else {
            0.5 * (self.generate_internal(fft) + self.generate_internal(fft))
        }
    }

    fn generate_internal(&mut self, fft: &mut FftCache) -> f32 {
        let mut result = 0.0f32;

        if self.output_position >= self.output_size {
            if self.start_new_note {
                if self.spectrum_size > 0 {
                    self.push_decaying();
                }
                for i in 1..self.spectrum_size {
                    self.spectrum_re[i] = 0.0;
                    self.spectrum_im[i] = 0.0;
                }
                self.start_new_note = false;
            }

            for d in &mut self.decaying {
                result += d.generate(fft);
            }

            // 揉弦频率/幅度的自然漂移
            self.vibrato_amplitude_drift =
                0.99 * self.vibrato_amplitude_drift + 0.1 * self.random.get_normal();
            let lo = self.instrument_type.lowest_note();
            let hi = self.instrument_type.highest_note();
            let x = (self.last_note - lo) as f32 / (hi - lo) as f32;
            let vibrato_base_freq =
                self.coef.vibrato_low + x * x * (self.coef.vibrato_high - self.coef.vibrato_low)
                    + 0.5 * self.vibrato_amplitude;
            let vibrato_freq = vibrato_base_freq
                * (1.0 + self.vibrato_frequency_drift_amplitude * (0.5 * PI * self.vibrato_phase).cos());
            let vibrato_amplitude = if self.last_note == lo {
                0.0
            } else {
                self.vibrato_amplitude
                    * (1.0 + self.vibrato_amplitude_drift_amplitude * self.vibrato_amplitude_drift)
            };

            // 瞬时频率 = 基频 ×（1 + 随机漂移）×（1 + 揉弦）
            let freq_drift_decay = (-self.period / self.internal_sr).exp();
            let freq_drift_noise = (1.0 - freq_drift_decay * freq_drift_decay).sqrt();
            self.frequency_drift = freq_drift_decay * self.frequency_drift
                + freq_drift_noise * self.random.get_normal();
            let vibrato_offset = vibrato_freq * self.period / self.internal_sr;
            self.vibrato_phase = (self.vibrato_phase + vibrato_offset) % 4.0;
            let v = (2.0 * PI * self.vibrato_phase).sin();
            let vibrato = v * v * v;
            let current_frequency = self.frequency
                * (1.0 + self.frequency_drift_amplitude * self.frequency_drift)
                * (1.0 + vibrato_amplitude * vibrato);

            // 周期长度 → 缓冲尺寸
            let new_period = self.internal_sr / current_frequency;
            let new_output_size = (new_period + self.period_offset).floor() as usize;
            let new_output_size = new_output_size.max(4);
            let new_spectrum_size = (new_output_size as f32 / 2.0 + 1.0).floor() as usize;
            if new_output_size > self.output_buffer.len() {
                self.output_buffer.resize(new_output_size, 0.0);
            }
            if new_spectrum_size > self.spectrum_re.len() {
                self.spectrum_re.resize(new_spectrum_size, 0.0);
                self.spectrum_im.resize(new_spectrum_size, 0.0);
            }
            for i in self.spectrum_size..new_spectrum_size {
                self.spectrum_re[i] = 0.0;
                self.spectrum_im[i] = 0.0;
            }
            self.period = new_period;
            self.output_size = new_output_size;
            self.spectrum_size = new_spectrum_size;
            self.period_offset = new_period + self.period_offset - new_output_size as f32;

            if self.volume != 0.0 {
                self.add_bow_excitation();
            }
            self.apply_filter();

            fft.inverse_real(
                &self.spectrum_re[..self.spectrum_size],
                &self.spectrum_im[..self.spectrum_size],
                &mut self.output_buffer[..self.output_size],
            );
            self.output_position = 0;
        } else {
            for d in &mut self.decaying {
                result += d.generate(fft);
            }
        }

        result += self.output_buffer[self.output_position];
        result = self.lowpass.process(result);
        self.output_position += 1;
        result
    }

    fn add_bow_excitation(&mut self) {
        let mut c = self.volume / (self.spectrum_size as f32).sqrt();
        if self.harmonics {
            c *= 2.0;
        }
        if matches!(self.last_articulation, Articulation::ColLegno) {
            c *= 0.6;
        }
        if self.bow_position != 0.5 {
            // 极端弓位：更轻、更不稳定
            let scale = 1.0
                - 1.7
                    * self.timbre.bow_focus
                    * self.random.get_uniform()
                    * (0.5 - self.bow_position).abs();
            c *= scale * scale;
            if self.bow_position > 0.5 {
                // sul tasto 不补偿会太轻
                c *= 1.0 + 0.2 * self.timbre.bow_focus * (self.bow_position - 0.5);
            }
        }
        let volume = self.volume.min(1.0);
        let n = self.spectrum_size;
        match self.last_articulation {
            Articulation::Pizzicato | Articulation::ColLegno => {
                let lo = self.instrument_type.lowest_note();
                let hi = self.instrument_type.highest_note();
                let x = (self.last_note - lo) as f32 / (hi - lo) as f32;
                let decay_target = if x > 0.5 {
                    (1.0 - volume) * (1.5 - x)
                } else {
                    1.0 - volume
                };
                for i in 1..n {
                    let decay = 1.0 - decay_target * (i as f32 / n as f32);
                    let scale = c
                        * decay
                        * (1.0 - i as f32 / n as f32).powi(self.coef.pizzicato_exponent);
                    let r1 = self.random.get_uniform();
                    let r2 = self.random.get_uniform();
                    self.spectrum_re[i] += scale * r1;
                    self.spectrum_im[i] += scale * r2;
                }
            }
            _ => {
                let (m1, b1, m2, b2) = (self.coef.m1, self.coef.b1, self.coef.m2, self.coef.b2);
                let decay_target = 0.7 * (1.0 - volume) * (1.0 - volume);
                let bow = self.bow_position;
                for i in 1..n {
                    let x = i as f32 / n as f32;
                    let logx = x.ln();
                    let y1 = (-m1 * logx + b1).exp();
                    let y2 = (-m2 * logx + b2).exp();
                    let mut decay;
                    if x <= 0.2 {
                        decay = 1.0 - decay_target * x / 0.2;
                    } else {
                        decay = 1.0 - decay_target * (1.0 - x) / 0.8;
                    }
                    if bow >= 0.5 {
                        // sul tasto：削高频
                        decay *= (-18.0 * x * (bow - 0.5) * self.timbre.bow_focus).exp();
                        if i == 1 {
                            decay *= 2.0;
                        }
                    } else {
                        // sul ponticello：抬高高频、压基频
                        decay *= (10.0
                            * (x - 0.5)
                            * (x - 0.5)
                            * (0.5 - bow)
                            * self.timbre.bow_focus)
                            .exp();
                        if i == 1 {
                            decay *= 0.05 + 3.9 * bow;
                        }
                    }
                    let scale = c * decay * y1.min(y2) * (1.0 + self.timbre.tilt * x);
                    let r1 = self.random.get_uniform();
                    let r2 = self.random.get_uniform();
                    self.spectrum_re[i] += scale * r1;
                    self.spectrum_im[i] += scale * r2;
                }
            }
        }
    }

    fn apply_filter(&mut self) {
        let n = self.spectrum_size;
        let phase_shift = self.coef.phase_shift * self.timbre.phase_scale;
        if phase_shift != 0.0 {
            for i in 1..n {
                let phase = phase_shift * i as f32 / n as f32;
                let (s, c) = phase.sin_cos();
                let re = self.spectrum_re[i];
                let im = self.spectrum_im[i];
                self.spectrum_re[i] = re * c - im * s;
                self.spectrum_im[i] = re * s + im * c;
            }
        }
        let s = if self.volume == 0.0 { 0.2 } else { 0.15 };
        let sqrt_n = (n as f32).sqrt();
        for i in 1..n {
            let f = i as f32 / n as f32;
            let scale = 1.0
                - (0.07 - 0.06 * (-8.0 * f).exp())
                    * sqrt_n
                    * s
                    * self.timbre.damping_scale;
            self.spectrum_re[i] *= scale;
            self.spectrum_im[i] *= scale;
        }
        if self.harmonics {
            for i in 1..n {
                if i % 4 != 0 {
                    self.spectrum_re[i] *= 0.1;
                    self.spectrum_im[i] *= 0.1;
                }
            }
        }
    }

    fn push_decaying(&mut self) {
        // 选一个空槽；都在用就抢最老的
        let mut slot = 0;
        let mut oldest = u64::MAX;
        let mut found_free = false;
        for (i, d) in self.decaying.iter().enumerate() {
            if !d.active {
                slot = i;
                found_free = true;
                break;
            }
            if d.seq < oldest {
                oldest = d.seq;
                slot = i;
            }
        }
        let _ = found_free;
        self.decaying_seq += 1;
        let seq = self.decaying_seq;
        let n = self.spectrum_size;
        let out = self.output_size;
        let re = &self.spectrum_re[..n];
        let im = &self.spectrum_im[..n];
        self.decaying[slot].start(re, im, out, seq);
    }
}

struct DecayingNote {
    active: bool,
    seq: u64,
    spectrum_re: Vec<f32>,
    spectrum_im: Vec<f32>,
    spec_len: usize,
    output_buffer: Vec<f32>,
    output_size: usize,
    output_position: usize,
    finished: bool,
}

impl DecayingNote {
    fn new() -> Self {
        Self {
            active: false,
            seq: 0,
            spectrum_re: Vec::new(),
            spectrum_im: Vec::new(),
            spec_len: 0,
            output_buffer: Vec::new(),
            output_size: 0,
            output_position: 0,
            finished: true,
        }
    }

    fn start(&mut self, re: &[f32], im: &[f32], output_size: usize, seq: u64) {
        if self.spectrum_re.len() < re.len() {
            self.spectrum_re.resize(re.len(), 0.0);
            self.spectrum_im.resize(im.len(), 0.0);
        }
        self.spectrum_re[..re.len()].copy_from_slice(re);
        self.spectrum_im[..im.len()].copy_from_slice(im);
        self.spec_len = re.len();
        if self.output_buffer.len() < output_size {
            self.output_buffer.resize(output_size, 0.0);
        }
        self.output_size = output_size;
        self.output_position = output_size;
        self.finished = false;
        self.active = true;
        self.seq = seq;
    }

    fn apply_filter(&mut self) {
        let n = self.spec_len;
        let sqrt_n = (n as f32).sqrt();
        for i in 1..n {
            let f = i as f32 / n as f32;
            let scale = 1.0 - (0.07 - 0.06 * (-8.0 * f).exp()) * sqrt_n * 0.15;
            self.spectrum_re[i] *= scale;
            self.spectrum_im[i] *= scale;
        }
    }

    fn generate(&mut self, fft: &mut FftCache) -> f32 {
        if !self.active || self.finished {
            self.active = false;
            return 0.0;
        }
        if self.output_position >= self.output_size {
            self.apply_filter();
            fft.inverse_real(
                &self.spectrum_re[..self.spec_len],
                &self.spectrum_im[..self.spec_len],
                &mut self.output_buffer[..self.output_size],
            );
            self.output_position = 0;
            self.finished = true;
            for &v in &self.output_buffer[..self.output_size] {
                if v.abs() > 0.001 {
                    self.finished = false;
                    break;
                }
            }
        }
        let r = self.output_buffer[self.output_position];
        self.output_position += 1;
        r
    }
}
