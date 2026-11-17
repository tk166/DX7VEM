//! 总线压缩器：立体声联动、峰值检测、软拐点、带 makeup。
//! 参数与曲线照搬我们 DX7 版（`msfa/msfa_wrapper.cpp`），已在实际曲目上验证过。

use crate::smooth::smooth_coef;

pub struct Compressor {
    env: f32,
    threshold_db: f32,
    inv_ratio: f32,
    knee_db: f32,
    atk: f32,
    rel: f32,
    makeup: f32,
    pub enabled: bool,
}

impl Compressor {
    pub fn new() -> Self {
        Self {
            env: 0.0,
            threshold_db: -18.0,
            inv_ratio: 1.0,
            knee_db: 6.0,
            atk: 0.6,
            rel: 0.25,
            makeup: 1.0,
            enabled: false,
        }
    }

    pub fn set_params(
        &mut self,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        sr: f32,
    ) {
        self.threshold_db = threshold_db;
        self.inv_ratio = 1.0 / ratio.max(1.0);
        self.atk = smooth_coef(attack_ms.max(0.1), sr);
        self.rel = smooth_coef(release_ms.max(0.1), sr);
        self.makeup = 10.0f32.powf(makeup_db / 20.0);
        self.enabled = true;
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
    }

    #[inline]
    pub fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        if !self.enabled {
            return (l, r);
        }
        let peak = l.abs().max(r.abs());
        if peak > self.env {
            self.env += (peak - self.env) * self.atk;
        } else {
            self.env += (peak - self.env) * self.rel;
        }
        let db = 20.0 * (self.env + 1e-9).log10();
        let over = db - self.threshold_db;
        let half_knee = self.knee_db * 0.5;
        let gr_db = if over > half_knee {
            over * (self.inv_ratio - 1.0)
        } else if over > -half_knee {
            let x = over + half_knee;
            (self.inv_ratio - 1.0) * x * x / (2.0 * self.knee_db)
        } else {
            0.0
        };
        let g = 10.0f32.powf(gr_db / 20.0) * self.makeup;
        (l * g, r * g)
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduces_loud_signal() {
        let mut c = Compressor::new();
        c.set_params(-12.0, 4.0, 5.0, 100.0, 0.0, 48000.0);
        // 1 kHz 正弦，幅度 0.5
        let mut peak_in = 0.0f32;
        let mut peak_out = 0.0f32;
        for i in 0..48000 {
            let x = 0.5 * (i as f32 * 0.13).sin();
            let (l, _) = c.process(x, x);
            if i > 24000 {
                peak_in = peak_in.max(x.abs());
                peak_out = peak_out.max(l.abs());
            }
        }
        assert!(peak_out < peak_in * 0.9, "in={peak_in} out={peak_out}");
    }

    #[test]
    fn disabled_is_transparent() {
        let mut c = Compressor::new();
        assert_eq!(c.process(0.3, -0.2), (0.3, -0.2));
    }
}
