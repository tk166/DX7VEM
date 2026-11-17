//! 每件乐器的谱合成系数（全部来自参考实现 `instrument.rs` 的拟合值）。
//!
//! 这些常数决定"音色"，M1 阶段**一个字节都不改**——先把还原跑通，
//! 之后再通过 `TimbreParams`（见 `instrument.rs`）暴露可调项做超越。

use crate::instrument::InstrumentType;

#[derive(Clone, Copy, Debug)]
pub struct VoiceCoef {
    pub vibrato_low: f32,
    pub vibrato_high: f32,
    /// 弓激励经验谱：(m1, b1, m2, b2)
    pub m1: f32,
    pub b1: f32,
    pub m2: f32,
    pub b2: f32,
    pub pizzicato_exponent: i32,
    /// 内部过采样倍率（小提琴 2×，其余 1×）
    pub oversample: f32,
    pub phase_shift: f32,
    /// tremolo 一个弓程的长度/间隔（采样 @48k）
    pub tremolo_length: i32,
    pub tremolo_space: i32,
    /// 琴体共振量（参考实现：violin/viola 0.18、cello 0.22、bass 0.30）
    pub body_mix: f32,
    /// 弓噪基础量（参考实现：violin 1.0、viola/cello 0.6、bass 0.9）
    pub bow_noise_scale: f32,
}

pub fn coef_for(t: InstrumentType) -> VoiceCoef {
    match t {
        InstrumentType::Violin => VoiceCoef {
            vibrato_low: 5.15,
            vibrato_high: 5.4,
            m1: 0.18917376,
            b1: 0.0050823237,
            m2: 1.3494484,
            b2: -2.9302601,
            pizzicato_exponent: 20,
            oversample: 2.0,
            phase_shift: 0.65,
            tremolo_length: 4100,
            tremolo_space: 200,
            body_mix: 0.18,
            bow_noise_scale: 1.0,
        },
        InstrumentType::Viola => VoiceCoef {
            vibrato_low: 5.15,
            vibrato_high: 5.4,
            m1: 0.59566957,
            b1: -2.227358,
            m2: 0.96543276,
            b2: -2.170609,
            pizzicato_exponent: 20,
            oversample: 1.0,
            phase_shift: 0.35,
            tremolo_length: 4600,
            tremolo_space: 200,
            body_mix: 0.18,
            bow_noise_scale: 0.6,
        },
        InstrumentType::Cello => VoiceCoef {
            vibrato_low: 5.0,
            vibrato_high: 5.5,
            m1: -0.0037527911,
            b1: 0.55326806,
            m2: 2.1803675,
            b2: -4.7307866,
            pizzicato_exponent: 30,
            oversample: 1.0,
            phase_shift: 0.0,
            tremolo_length: 4600,
            tremolo_space: 400,
            body_mix: 0.22,
            bow_noise_scale: 0.6,
        },
        InstrumentType::Bass => VoiceCoef {
            vibrato_low: 4.9,
            vibrato_high: 5.4,
            m1: 0.34754488,
            b1: -0.852053,
            m2: 1.0190364,
            b2: -2.5493735,
            pizzicato_exponent: 40,
            oversample: 1.0,
            phase_shift: 0.0,
            tremolo_length: 4500,
            tremolo_space: 1000,
            body_mix: 0.30,
            bow_noise_scale: 0.9,
        },
    }
}

/// 音色可调项（默认全部中性，不改变 M1 的还原结果）。
#[derive(Clone, Copy, Debug)]
pub struct TimbreParams {
    /// 高次谐波阻尼的倍率（1.0 = 参考实现）
    pub damping_scale: f32,
    /// 谐波倾斜：>0 更亮（给高频乘 (1+tilt*x)）
    pub tilt: f32,
    /// 弓位影响的强度（1.0 = 参考实现）
    pub bow_focus: f32,
    /// 额外低通截止倍率（1.0 = 参考实现）
    pub cutoff_scale: f32,
    /// 相位偏移倍率（1.0 = 参考实现）
    pub phase_scale: f32,
}

impl Default for TimbreParams {
    fn default() -> Self {
        Self {
            damping_scale: 1.0,
            tilt: 0.0,
            bow_focus: 1.0,
            cutoff_scale: 1.0,
            phase_scale: 1.0,
        }
    }
}
