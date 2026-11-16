//! 确定性随机数（自实现，无 getrandom 依赖）。
//!
//! 这里的 LCG 与参考实现 Viola Ex Machina 的 "even quicker generator"
//! （Numerical Recipes）**同一套常数**。这是公开的数值方法，不构成代码衍生；
//! 之所以故意选它，是为了让参考渲染与我们的引擎在**同一种子**下可以逐采样对拍，
//! M1 调试时能直接定位到"哪一步开始不一样"。
//!
//! 生产路径不需要系统熵：种子由引擎参数传入（默认固定值），
//! 因此渲染完全可复现，回归测试稳定。

pub struct Random {
    i: u32,
    next_normal: f32,
    next_normal_valid: bool,
}

const UNIFORM_SCALE: f32 = 1.0 / (0x1_0000_0000u64 as f32);

impl Random {
    pub fn with_seed(seed: u32) -> Self {
        Self {
            i: seed,
            next_normal: 0.0,
            next_normal_valid: false,
        }
    }

    pub fn reset(&mut self, seed: u32) {
        self.i = seed;
        self.next_normal = 0.0;
        self.next_normal_valid = false;
    }

    /// 一个 32 位随机整数。
    pub fn get_int(&mut self) -> u32 {
        self.i = (self.i as u64).wrapping_mul(1664525).wrapping_add(1013904223) as u32;
        self.i
    }

    /// [0, 1) 均匀分布。
    pub fn get_uniform(&mut self) -> f32 {
        UNIFORM_SCALE * (self.get_int() as f32)
    }

    /// 标准正态分布（Box-Muller，与参考实现同序：一次调用可能预取下一个值）。
    pub fn get_normal(&mut self) -> f32 {
        if self.next_normal_valid {
            self.next_normal_valid = false;
            return self.next_normal;
        }
        loop {
            let x = 2.0 * self.get_uniform() - 1.0;
            let y = 2.0 * self.get_uniform() - 1.0;
            let r2 = x * x + y * y;
            if r2 < 1.0 && r2 != 0.0 {
                let multiplier = (-2.0 * r2.ln() / r2).sqrt();
                self.next_normal = y * multiplier;
                self.next_normal_valid = true;
                return x * multiplier;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let mut a = Random::with_seed(12345);
        let mut b = Random::with_seed(12345);
        for _ in 0..1000 {
            assert_eq!(a.get_int(), b.get_int());
            assert_eq!(a.get_uniform(), b.get_uniform());
        }
    }

    #[test]
    fn uniform_in_range() {
        let mut r = Random::with_seed(7);
        for _ in 0..10000 {
            let v = r.get_uniform();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn normal_is_bounded_and_repeatable() {
        let mut a = Random::with_seed(99);
        let mut b = Random::with_seed(99);
        let mut sum = 0.0;
        let n = 20000;
        for _ in 0..n {
            let v = a.get_normal();
            assert_eq!(v, b.get_normal());
            assert!(v.abs() < 8.0);
            sum += v;
        }
        assert!((sum / n as f32).abs() < 0.05);
    }
}
