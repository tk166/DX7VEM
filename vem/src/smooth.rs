//! 参数平滑与总线保护（与 msfa/msfa_wrapper.cpp 里验证过的一致）。

/// 一阶平滑（逐采样或逐块调用，系数 a 决定快慢）。
#[inline]
pub fn smooth_to(cur: f32, target: f32, a: f32) -> f32 {
    cur + (target - cur) * a
}

/// 逐采样平滑系数：时间常数 ms → a。
#[inline]
pub fn smooth_coef(ms: f32, sr: f32) -> f32 {
    1.0 - (-1.0 / (0.001 * ms * sr)).exp()
}

/// 逐块平滑系数（块长 n）：时间常数 ms → a。
#[inline]
pub fn block_coef(ms: f32, sr: f32, n: usize) -> f32 {
    1.0 - (-(n as f32) / (0.001 * ms * sr)).exp()
}

/// 末端软限幅：只处理 |x|>0.98 的峰，保护 0 dBFS，正常动态几乎不受影响。
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    const T: f32 = 0.98;
    const INV: f32 = 1.0 - T;
    if x > T {
        T + INV * ((x - T) / INV).tanh()
    } else if x < -T {
        -T - INV * ((-x - T) / INV).tanh()
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clip_is_bounded_and_continuous() {
        assert!((soft_clip(0.5) - 0.5).abs() < 1e-7);
        assert!(soft_clip(100.0) <= 1.0);
        assert!(soft_clip(100.0) > 0.99);
        assert!(soft_clip(-100.0) >= -1.0);
        let a = soft_clip(0.9799);
        let b = soft_clip(0.9801);
        assert!((b - a).abs() < 1e-3);
    }

    #[test]
    fn smooth_reaches_target() {
        let mut v = 0.0;
        let a = smooth_coef(5.0, 48000.0);
        for _ in 0..48000 {
            v = smooth_to(v, 1.0, a);
        }
        assert!((v - 1.0).abs() < 0.01);
    }
}
