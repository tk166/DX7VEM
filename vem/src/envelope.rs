//! 包络参数（单位统一：**48 kHz 下的采样数**，与参考实现同一套常数，保证 48 k 逐位对齐；
//! 其它采样率按 `sr/48000` 缩放）。
//!
//! 全部可调 —— 默认值是"还原"档，之后做"超越"时只改这些参数，不动 DSP 结构。

#[derive(Clone, Copy, Debug)]
pub struct EnvelopeParams {
    /// 起音基础时长（采样 @48k）
    pub attack: f32,
    /// 力度 0 时额外增加的起音时长（采样 @48k）：attack = attack + attack_vel × (1 - vel)
    pub attack_vel: f32,
    /// 释放基础时长
    pub release: f32,
    /// release_rate=0 时额外增加的释放时长
    pub release_vel: f32,
    /// 起音时弓位从"贴琴马"归位的时间
    pub bow_settle: f32,
    /// 揉弦渐入延迟（ms，0 = 参考实现行为：立即满幅）
    pub vibrato_onset_ms: f32,
    /// 揉弦渐入过渡（ms）
    pub vibrato_onset_ramp_ms: f32,
    /// legato 换音时接续多少当前包络（参考实现 = 0.5，越大越"不断音"）
    pub legato_start: f32,
    /// legato 换音后回到 1 的时间（ms，0 = 使用起音时间）
    pub legato_ramp_ms: f32,
    /// legato 换音的小上冲（0 = 关，0.03 ≈ +3%）
    pub legato_accent: f32,
    /// legato 换音后短暂提高激励，补偿"新频谱从零重建"的凹陷（0 = 关）
    pub legato_boost: f32,
    /// 激励补偿的衰减时间（ms）
    pub legato_boost_ms: f32,
}

impl Default for EnvelopeParams {
    fn default() -> Self {
        // 与参考实现一致（1000 / 30000 / 1000 / 10000 / 10000 采样 @48k）
        Self {
            attack: 1000.0,
            attack_vel: 30000.0,
            release: 1000.0,
            release_vel: 10000.0,
            bow_settle: 10000.0,
            vibrato_onset_ms: 0.0,
            vibrato_onset_ramp_ms: 60.0,
            legato_start: 0.5,
            legato_ramp_ms: 0.0,
            legato_accent: 0.0,
            legato_boost: 0.0,
            legato_boost_ms: 25.0,
        }
    }
}

impl EnvelopeParams {
    #[inline]
    fn scale(&self, sr: f32) -> f32 {
        sr / 48000.0
    }

    pub fn attack_samples(&self, velocity: f32, sr: f32) -> i64 {
        ((self.attack + self.attack_vel * (1.0 - velocity)) * self.scale(sr)) as i64
    }

    pub fn release_samples(&self, release_rate: f32, sr: f32) -> i64 {
        ((self.release + self.release_vel * (1.0 - release_rate)) * self.scale(sr)) as i64
    }

    pub fn bow_settle_samples(&self, sr: f32) -> i64 {
        (self.bow_settle * self.scale(sr)) as i64
    }

    /// 从毫秒设置（UI 友好）；内部换算回"48k 采样"。
    pub fn set_attack_ms(&mut self, base_ms: f32, velocity_ms: f32) {
        self.attack = (base_ms * 48.0).max(0.0);
        self.attack_vel = (velocity_ms * 48.0).max(0.0);
    }

    pub fn set_release_ms(&mut self, base_ms: f32, slow_ms: f32) {
        self.release = (base_ms * 48.0).max(0.0);
        self.release_vel = (slow_ms * 48.0).max(0.0);
    }

    pub fn set_bow_settle_ms(&mut self, ms: f32) {
        self.bow_settle = (ms * 48.0).max(0.0);
    }

    pub fn attack_ms(&self) -> f32 {
        self.attack / 48.0
    }

    pub fn attack_velocity_ms(&self) -> f32 {
        self.attack_vel / 48.0
    }

    pub fn release_ms(&self) -> f32 {
        self.release / 48.0
    }

    pub fn release_slow_ms(&self) -> f32 {
        self.release_vel / 48.0
    }

    pub fn bow_settle_ms(&self) -> f32 {
        self.bow_settle / 48.0
    }
}
