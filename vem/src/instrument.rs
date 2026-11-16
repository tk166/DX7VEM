//! 单声部乐器（M0：正弦占位；M1 换成谱激励 + 周期重合成）。
//!
//! 结构上先按最终形态切好：一个 Instrument = 一个音高 + 自己的包络/释放。
//! 复音由 engine 的槽位池提供（VEM 的 Director 也是多 Instrument 并置）。

use crate::articulations::Articulation;

const ATTACK_SECONDS: f32 = 0.005;
const RELEASE_SECONDS: f32 = 0.12;

pub struct Instrument {
    active: bool,
    pub note: i32,
    pub velocity: f32,
    pub frequency: f32,
    phase: f32,
    phase_inc: f32,
    attack_gain: f32,
    release_gain: f32,
    releasing: bool,
    pub age: u64,
}

impl Instrument {
    pub fn new() -> Self {
        Self {
            active: false,
            note: -1,
            velocity: 0.0,
            frequency: 0.0,
            phase: 0.0,
            phase_inc: 0.0,
            attack_gain: 0.0,
            release_gain: 0.0,
            releasing: false,
            age: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_releasing(&self) -> bool {
        self.releasing
    }

    pub fn note_on(&mut self, note: i32, velocity: f32, sample_rate: f32, age: u64) {
        self.active = true;
        self.note = note;
        self.velocity = velocity.clamp(0.0, 1.0);
        self.frequency = note_to_frequency(note);
        self.phase = 0.0;
        self.phase_inc = self.frequency / sample_rate;
        self.attack_gain = 0.0;
        self.release_gain = 1.0;
        self.releasing = false;
        self.age = age;
    }

    pub fn note_off(&mut self) {
        self.releasing = true;
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 叠加到输出缓冲（不覆盖 —— 多声部求和）。
    pub fn render_add(&mut self, out: &mut [f32], sample_rate: f32, articulation: Articulation) {
        if !self.active {
            return;
        }
        // TODO(M1): 用谱合成取代正弦；articulation 目前只做电平差异占位。
        let amp = 0.15 * self.velocity * articulation_gain(articulation);
        let attack_step = 1.0 / (ATTACK_SECONDS * sample_rate);
        let release_coef = (-1.0 / (RELEASE_SECONDS * sample_rate)).exp();
        for s in out.iter_mut() {
            if self.attack_gain < 1.0 {
                self.attack_gain = (self.attack_gain + attack_step).min(1.0);
            }
            if self.releasing {
                self.release_gain *= release_coef;
            }
            let env = self.attack_gain * self.release_gain;
            *s += amp * env * (core::f32::consts::TAU * self.phase).sin();
            self.phase += self.phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            if self.releasing && self.release_gain < 1e-4 {
                self.active = false;
                break;
            }
        }
    }
}

fn articulation_gain(a: Articulation) -> f32 {
    match a {
        Articulation::Arco => 1.0,
        Articulation::Marcato => 1.3,
        Articulation::Glissando => 1.0,
        Articulation::Spiccato => 0.9,
        Articulation::Pizzicato => 0.8,
        Articulation::ColLegno => 0.6,
        Articulation::Tremolo => 0.85,
    }
}

#[inline]
pub fn note_to_frequency(note: i32) -> f32 {
    440.0 * (2.0f32).powf((note - 69) as f32 / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_is_440() {
        assert!((note_to_frequency(69) - 440.0).abs() < 1e-3);
        assert!((note_to_frequency(57) - 220.0).abs() < 1e-2);
        assert!((note_to_frequency(81) - 880.0).abs() < 1e-2);
    }

    #[test]
    fn releases_to_silence() {
        let mut inst = Instrument::new();
        let sr = 48000.0;
        inst.note_on(69, 1.0, sr, 0);
        let mut buf = vec![0.0f32; 4800];
        inst.render_add(&mut buf, sr, Articulation::Arco);
        assert!(inst.is_active());
        inst.note_off();
        for _ in 0..2000 {
            let mut b = vec![0.0f32; 480];
            inst.render_add(&mut b, sr, Articulation::Arco);
            if !inst.is_active() {
                break;
            }
        }
        assert!(!inst.is_active(), "release 后应停止发声");
    }
}
