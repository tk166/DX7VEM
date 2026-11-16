//! 一阶 IIR 低通（与参考实现同公式、同归一化习惯）。

pub struct Lowpass {
    alpha: f32,
    y: f32,
    sr: f32,
}

impl Lowpass {
    /// `sr` 是**输出采样率**：参考实现在 2× 过采样的内部循环里仍然用 48 kHz 的 dt，
    /// 等价于截止频率减半。我们在 48 kHz 下复刻这个行为，其它采样率按比例推广。
    pub fn new(cutoff: f32, sr: f32) -> Self {
        let mut f = Self { alpha: 0.0, y: 0.0, sr };
        f.set_cutoff(cutoff);
        f
    }

    pub fn set_cutoff(&mut self, cutoff: f32) {
        let cutoff = cutoff.max(1.0);
        let rc = 1.0 / (2.0 * core::f32::consts::PI * cutoff);
        let dt = 1.0 / self.sr;
        self.alpha = dt / (rc + dt);
    }

    pub fn reset(&mut self) {
        self.y = 0.0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.y += self.alpha * (x - self.y);
        self.y
    }
}
