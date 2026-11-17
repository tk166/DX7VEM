//! 弓噪：采样噪声 + 谐振滤波（与参考实现的 `Division` 侧逻辑一致）。
//!
//! 参考实现把 `bow_noise.flac` 解码后按"每个乐器一个起点"循环播放，
//! 再过一个谐振滤波器（中心 2×基频、带宽 = 基频），随弓噪量/弓法调制。

#[derive(Clone, Copy)]
pub struct ResonantFilter {
    b1: f32,
    b2: f32,
    y1: f32,
    y2: f32,
}

impl ResonantFilter {
    pub fn new(resonant_frequency: f32, bandwidth: f32, sample_rate: f32) -> Self {
        let w = 2.0 * core::f32::consts::PI * resonant_frequency / sample_rate;
        let r = 1.0 - core::f32::consts::PI * bandwidth / sample_rate;
        Self {
            b1: -2.0 * r * w.cos(),
            b2: r * r,
            y1: 0.0,
            y2: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.b1 * self.y1 - self.b2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// 一个 voice 的噪声读取状态。
#[derive(Clone, Copy)]
pub struct NoisePlayhead {
    pub position: usize,
    pub filter: ResonantFilter,
}

impl NoisePlayhead {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            position: 0,
            filter: ResonantFilter::new(100.0, 100.0, sample_rate),
        }
    }

    pub fn tune(&mut self, note_frequency: f32, sample_rate: f32) {
        self.filter = ResonantFilter::new(2.0 * note_frequency, note_frequency, sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_is_stable_and_finite() {
        let mut f = ResonantFilter::new(440.0, 220.0, 48000.0);
        let mut peak = 0.0f32;
        for i in 0..48000 {
            let y = f.process(if i == 0 { 1.0 } else { 0.0 });
            assert!(y.is_finite());
            peak = peak.max(y.abs());
        }
        assert!(peak < 100.0, "peak={peak}");
    }
}
