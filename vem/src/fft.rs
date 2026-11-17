//! 任意长度的复数 IFFT + 实数 IFFT，带按长度缓存的“计划”。
//!
//! - 长度只含因子 2/3/5 → 递归混合基 Cooley–Tukey（我们的主力路径）；
//! - 其它长度（含 7/11/… 与大素数）→ Bluestein（chirp-z），内部用基 2 FFT；
//! - 计划缓存：音频路径上不分配（计划在首次遇到该长度时构建，之后只查表）。
//!
//! 约定与 `realfft::ComplexToReal` 一致（**不归一化**）：
//!   输入是半谱 `H[0..=n/2]`（Hermitian），输出 `x[t] = Σ X[k] e^{+2πikt/n}`。
//!   注意：参考实现直接用 realfft 的结果，所以这里有因子 n ——必须照抄，
//!   否则整体电平会差 `n` 倍（M1 第一次对拍就撞上了，见 docs/VEM踩坑记录.md）。
//!
//! 与计划 §11 的风险对策对应：tests 里对所有长度与**朴素 DFT**逐点对比，不是“看起来对”。

use core::f32::consts::PI;

const CACHE_CAPACITY: usize = 96;

pub struct FftCache {
    plans: Vec<Plan>,
    tick: u64,
    /// 复谱全谱缓冲（长度 n）
    buf_re: Vec<f32>,
    buf_im: Vec<f32>,
    /// 混合基递归信托缓冲（长度 n）
    sre: Vec<f32>,
    sim: Vec<f32>,
    /// Bluestein 工作区（长度 m）
    wre: Vec<f32>,
    wim: Vec<f32>,
}

struct Plan {
    n: usize,
    last_used: u64,
    kind: Kind,
}

enum Kind {
    /// 2/3/5 混合基：tw 是 e^{+2πi t/n}，len = n。
    Smooth {
        radices: Vec<usize>,
        tw_re: Vec<f32>,
        tw_im: Vec<f32>,
    },
    /// Bluestein：m = next_pow2(2n-1)
    Bluestein(Box<Blue>),
}

struct Blue {
    m: usize,
    /// a_k = e^{+iπk²/n}
    chirp_re: Vec<f32>,
    chirp_im: Vec<f32>,
    /// FFT_{-1}(b)，b 是 conj(chirp) 的循环摆放
    b_re: Vec<f32>,
    b_im: Vec<f32>,
    /// 基 2 旋转因子 e^{-2πi t/m}，len = m/2
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    bitrev: Vec<u32>,
}

impl FftCache {
    pub fn new() -> Self {
        Self {
            plans: Vec::with_capacity(CACHE_CAPACITY),
            tick: 0,
            buf_re: Vec::new(),
            buf_im: Vec::new(),
            sre: Vec::new(),
            sim: Vec::new(),
            wre: Vec::new(),
            wim: Vec::new(),
        }
    }

    /// 实数 IFFT：半谱 → 实数缓冲。`spec_*` 长度 = out.len()/2 + 1。
    pub fn inverse_real(&mut self, spec_re: &[f32], spec_im: &[f32], out: &mut [f32]) {
        let n = out.len();
        debug_assert_eq!(spec_re.len(), n / 2 + 1);
        debug_assert_eq!(spec_im.len(), n / 2 + 1);
        if n == 0 {
            return;
        }
        let mut buf_re = core::mem::take(&mut self.buf_re);
        let mut buf_im = core::mem::take(&mut self.buf_im);
        buf_re.resize(n, 0.0);
        buf_im.resize(n, 0.0);

        // 半谱 → 全谱（Hermitian）
        for k in 0..=n / 2 {
            buf_re[k] = spec_re[k];
            buf_im[k] = spec_im[k];
        }
        if n % 2 == 0 {
            buf_im[n / 2] = 0.0;
            for k in 1..n / 2 {
                let m = n - k;
                buf_re[m] = spec_re[k];
                buf_im[m] = -spec_im[k];
            }
        } else {
            for k in 1..=n / 2 {
                let m = n - k;
                buf_re[m] = spec_re[k];
                buf_im[m] = -spec_im[k];
            }
        }

        self.dft_inplace(&mut buf_re, &mut buf_im);

        out[..n].copy_from_slice(&buf_re);
        self.buf_re = buf_re;
        self.buf_im = buf_im;
    }

    /// 复数 IFFT（正指数，**不归一化**，与 realfft 一致）。
    pub fn dft_inplace(&mut self, re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        debug_assert_eq!(re.len(), im.len());
        if n <= 1 {
            return;
        }
        let idx = self.plan_index(n);
        self.tick += 1;
        self.plans[idx].last_used = self.tick;

        let blue = match &self.plans[idx].kind {
            Kind::Smooth { .. } => None,
            Kind::Bluestein(b) => Some(b.m),
        };

        if let Some(m) = blue {
            self.wre.resize(m, 0.0);
            self.wim.resize(m, 0.0);
            let plan = &self.plans[idx];
            let (wre, wim) = (&mut self.wre[..m], &mut self.wim[..m]);
            if let Kind::Bluestein(b) = &plan.kind {
                bluestein_run(b, re, im, wre, wim);
            }
        } else {
            self.sre.resize(n, 0.0);
            self.sim.resize(n, 0.0);
            let plan = &self.plans[idx];
            let (sre, sim) = (&mut self.sre[..n], &mut self.sim[..n]);
            if let Kind::Smooth {
                radices,
                tw_re,
                tw_im,
            } = &plan.kind
            {
                mixed_fft(re, im, 0, n, 1, radices, tw_re, tw_im, n, sre, sim);
            }
        }
    }

    /// 复数 DFT（负指数，不归一化）。
    pub fn dft_forward(&mut self, re: &mut [f32], im: &mut [f32]) {
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.dft_inplace(re, im);
        for v in im.iter_mut() {
            *v = -*v;
        }
    }

    /// 实数 DFT：输入 n 个实数 → 半谱 `spec[0..=n/2]`（与 realfft 的 RealToComplex 同约定）。
    pub fn forward_real(&mut self, input: &[f32], spec_re: &mut [f32], spec_im: &mut [f32]) {
        let n = input.len();
        debug_assert_eq!(spec_re.len(), n / 2 + 1);
        let mut re = core::mem::take(&mut self.buf_re);
        let mut im = core::mem::take(&mut self.buf_im);
        re.clear();
        re.extend_from_slice(input);
        im.clear();
        im.resize(n, 0.0);
        self.dft_forward(&mut re, &mut im);
        spec_re.copy_from_slice(&re[..n / 2 + 1]);
        spec_im.copy_from_slice(&im[..n / 2 + 1]);
        self.buf_re = re;
        self.buf_im = im;
    }

    fn plan_index(&mut self, n: usize) -> usize {
        if let Some(i) = self.plans.iter().position(|p| p.n == n) {
            return i;
        }
        let plan = Plan::new(n);
        if self.plans.len() < CACHE_CAPACITY {
            self.plans.push(plan);
            self.plans.len() - 1
        } else {
            // LRU 替换（音频路径上极少发生）
            let mut victim = 0;
            for (i, p) in self.plans.iter().enumerate() {
                if p.last_used < self.plans[victim].last_used {
                    victim = i;
                }
            }
            self.plans[victim] = plan;
            victim
        }
    }
}

impl Default for FftCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Plan {
    fn new(n: usize) -> Self {
        let mut radices = Vec::new();
        let mut rest = n;
        for r in [2usize, 3, 5] {
            while rest % r == 0 {
                radices.push(r);
                rest /= r;
            }
        }
        if rest == 1 {
            let (tw_re, tw_im) = twiddles(n);
            Self {
                n,
                last_used: 0,
                kind: Kind::Smooth {
                    radices,
                    tw_re,
                    tw_im,
                },
            }
        } else {
            Self {
                n,
                last_used: 0,
                kind: Kind::Bluestein(Box::new(Blue::new(n))),
            }
        }
    }
}

fn twiddles(n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut re = Vec::with_capacity(n);
    let mut im = Vec::with_capacity(n);
    for t in 0..n {
        let a = 2.0 * PI * t as f32 / n as f32;
        re.push(a.cos());
        im.push(a.sin());
    }
    (re, im)
}

impl Blue {
    fn new(n: usize) -> Self {
        let mut m = 1usize;
        while m < 2 * n - 1 {
            m <<= 1;
        }
        // a_k = e^{+iπk²/n}（k² 先对 2n 取模，避免 f32 精度损失）
        let two_n = 2 * n as i64;
        let mut chirp_re = Vec::with_capacity(n);
        let mut chirp_im = Vec::with_capacity(n);
        for k in 0..n {
            let k2 = (k as i64 * k as i64) % two_n;
            let ang = PI * k2 as f32 / n as f32;
            chirp_re.push(ang.cos());
            chirp_im.push(ang.sin());
        }
        // b = conj(a)，循环摆放
        let mut b_re = vec![0.0f32; m];
        let mut b_im = vec![0.0f32; m];
        b_re[0] = 1.0;
        for t in 1..n {
            b_re[t] = chirp_re[t];
            b_im[t] = -chirp_im[t];
            b_re[m - t] = chirp_re[t];
            b_im[m - t] = -chirp_im[t];
        }
        // 基 2 计划
        let mut bitrev = vec![0u32; m];
        let bits = m.trailing_zeros();
        for i in 0..m {
            bitrev[i] = (i as u32).reverse_bits() >> (32 - bits);
        }
        let mut tw_re = Vec::with_capacity(m / 2);
        let mut tw_im = Vec::with_capacity(m / 2);
        for t in 0..m / 2 {
            let a = -2.0 * PI * t as f32 / m as f32;
            tw_re.push(a.cos());
            tw_im.push(a.sin());
        }
        // B = FFT_{-1}(b)
        radix2(&mut b_re, &mut b_im, false, &tw_re, &tw_im, &bitrev);

        Self {
            m,
            chirp_re,
            chirp_im,
            b_re,
            b_im,
            tw_re,
            tw_im,
            bitrev,
        }
    }
}

/// 迭代基 2 FFT；`inverse = true` → e^{+2πi}。
fn radix2(
    re: &mut [f32],
    im: &mut [f32],
    inverse: bool,
    tw_re: &[f32],
    tw_im: &[f32],
    bitrev: &[u32],
) {
    let n = re.len();
    for i in 0..n {
        let j = bitrev[i] as usize;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0f32 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let step = n / len;
        for base in (0..n).step_by(len) {
            for k in 0..half {
                let t = k * step;
                let wr = tw_re[t];
                let wi = sign * tw_im[t];
                let u = base + k;
                let v = base + k + half;
                let vr = re[v] * wr - im[v] * wi;
                let vi = re[v] * wi + im[v] * wr;
                re[v] = re[u] - vr;
                im[v] = im[u] - vi;
                re[u] += vr;
                im[u] += vi;
            }
        }
        len <<= 1;
    }
}

/// Bluestein：DFT 正指数（含调用方的 1/n 归一化由 dft_inplace 做）。
fn bluestein_run(b: &Blue, re: &mut [f32], im: &mut [f32], wre: &mut [f32], wim: &mut [f32]) {
    let n = re.len();
    let m = b.m;
    for k in 0..m {
        wre[k] = 0.0;
        wim[k] = 0.0;
    }
    for k in 0..n {
        let ar = b.chirp_re[k];
        let ai = b.chirp_im[k];
        wre[k] = re[k] * ar - im[k] * ai;
        wim[k] = re[k] * ai + im[k] * ar;
    }
    radix2(wre, wim, false, &b.tw_re, &b.tw_im, &b.bitrev);
    for k in 0..m {
        let yr = wre[k];
        let yi = wim[k];
        let br = b.b_re[k];
        let bi = b.b_im[k];
        wre[k] = yr * br - yi * bi;
        wim[k] = yr * bi + yi * br;
    }
    radix2(wre, wim, true, &b.tw_re, &b.tw_im, &b.bitrev);
    let inv_m = 1.0 / m as f32;
    for k in 0..n {
        let pr = wre[k] * inv_m;
        let pi = wim[k] * inv_m;
        let ar = b.chirp_re[k];
        let ai = b.chirp_im[k];
        re[k] = pr * ar - pi * ai;
        im[k] = pr * ai + pi * ar;
    }
}

/// 递归混合基 DIT（正指数 = 逆变换）。
/// 序列在 `base + t*stride` 上，长度 n，`radices` 是当前层的因子序列。
#[allow(clippy::too_many_arguments)]
fn mixed_fft(
    re: &mut [f32],
    im: &mut [f32],
    base: usize,
    n: usize,
    stride: usize,
    radices: &[usize],
    tw_re: &[f32],
    tw_im: &[f32],
    wn: usize,
    sre: &mut [f32],
    sim: &mut [f32],
) {
    if n <= 1 || radices.is_empty() {
        return;
    }
    let r = radices[0];
    let m = n / r;
    if r == 1 {
        mixed_fft(re, im, base, m, stride, &radices[1..], tw_re, tw_im, wn, sre, sim);
        return;
    }
    for j in 0..r {
        mixed_fft(
            re,
            im,
            base + j * stride,
            m,
            stride * r,
            &radices[1..],
            tw_re,
            tw_im,
            wn,
            sre,
            sim,
        );
    }
    // 合并：X[k + q*m] = Σ_j W_n^{jk} W_r^{jq} Y_j[k]
    let wstep = wn / n; // W_len^{jk} = W_wn^{jk * (wn/len)}
    let rstep = wn / r;
    let mut zr = [0.0f32; 8];
    let mut zi = [0.0f32; 8];
    for k in 0..m {
        for j in 0..r {
            let idx = base + (j + k * r) * stride;
            let t = (j * k * wstep) % wn;
            let wr = tw_re[t];
            let wi = tw_im[t];
            let (dr, di) = (re[idx], im[idx]);
            zr[j] = dr * wr - di * wi;
            zi[j] = dr * wi + di * wr;
        }
        for q in 0..r {
            let mut sr = 0.0f32;
            let mut si = 0.0f32;
            for j in 0..r {
                let t = (j * q * rstep) % wn;
                let wr = tw_re[t];
                let wi = tw_im[t];
                sr += zr[j] * wr - zi[j] * wi;
                si += zr[j] * wi + zi[j] * wr;
            }
            sre[k + q * m] = sr;
            sim[k + q * m] = si;
        }
    }
    for k in 0..n {
        let idx = base + k * stride;
        re[idx] = sre[k];
        im[idx] = sim[k];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random::Random;

    fn naive_dft_re(re: &[f32], im: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let n = re.len();
        let mut dr = vec![0.0f32; n];
        let mut di = vec![0.0f32; n];
        for k in 0..n {
            let mut sr = 0.0f64;
            let mut si = 0.0f64;
            for t in 0..n {
                let a = 2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                let (s, c) = a.sin_cos();
                sr += re[t] as f64 * c - im[t] as f64 * s;
                si += re[t] as f64 * s + im[t] as f64 * c;
            }
            dr[k] = sr as f32;
            di[k] = si as f32;
        }
        (dr, di)
    }

    #[test]
    fn complex_ifft_matches_naive() {
        let sizes = [
            2usize, 3, 4, 5, 6, 7, 8, 9, 10, 12, 15, 16, 25, 30, 49, 64, 97, 128, 218, 1000, 1553,
        ];
        let mut cache = FftCache::new();
        let mut rng = Random::with_seed(4242);
        for &n in &sizes {
            let mut re: Vec<f32> = (0..n).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
            let mut im: Vec<f32> = (0..n).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
            let (er, ei) = naive_dft_re(&re, &im);
            cache.dft_inplace(&mut re, &mut im);
            for k in 0..n {
                let dr = (re[k] - er[k]).abs();
                let di = (im[k] - ei[k]).abs();
                let mag = er[k].abs().max(1.0);
                assert!(
                    dr < 2e-2 * mag && di < 2e-2 * mag,
                    "n={n} k={k} dr={dr} di={di} (mag={mag})"
                );
            }
        }
    }

    #[test]
    fn real_ifft_reconstructs_cosines() {
        let n = 218usize;
        let mut cache = FftCache::new();
        let spec_len = n / 2 + 1;
        let mut sr = vec![0.0f32; spec_len];
        let mut si = vec![0.0f32; spec_len];
        let amp = 0.3f32;
        sr[1] = amp * 0.5;
        sr[2] = amp * 0.125;
        let mut out = vec![0.0f32; n];
        cache.inverse_real(&sr, &si, &mut out);
        let expected: Vec<f32> = (0..n)
            .map(|t| {
                let a = 2.0 * PI * t as f32 / n as f32;
                amp * (a).cos() + 0.25 * amp * (2.0 * a).cos()
            })
            .collect();
        let max_err = out
            .iter()
            .zip(expected.iter())
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(max_err < 1e-4, "max_err={max_err}");
    }

    #[test]
    fn cache_reuses_plans() {
        let mut cache = FftCache::new();
        for _ in 0..10 {
            let mut re = vec![1.0f32; 49];
            let mut im = vec![0.0f32; 49];
            cache.dft_inplace(&mut re, &mut im);
        }
        assert_eq!(cache.plans.len(), 1);
    }

    #[test]
    fn real_forward_inverse_roundtrip() {
        let n = 128usize;
        let mut cache = FftCache::new();
        let mut rng = Random::with_seed(11);
        let input: Vec<f32> = (0..n).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
        let mut sr = vec![0.0f32; n / 2 + 1];
        let mut si = vec![0.0f32; n / 2 + 1];
        cache.forward_real(&input, &mut sr, &mut si);
        let mut out = vec![0.0f32; n];
        cache.inverse_real(&sr, &si, &mut out);
        let scale = 1.0 / n as f32;
        let max_err = input
            .iter()
            .zip(out.iter())
            .fold(0.0f32, |m, (a, b)| m.max((a - b * scale).abs()));
        assert!(max_err < 1e-5, "max_err={max_err}");
    }

    #[test]
    fn real_forward_matches_naive() {
        let n = 64usize;
        let mut cache = FftCache::new();
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.3).sin()).collect();
        let mut sr = vec![0.0f32; n / 2 + 1];
        let mut si = vec![0.0f32; n / 2 + 1];
        cache.forward_real(&input, &mut sr, &mut si);
        for k in 0..=n / 2 {
            let mut er = 0.0f64;
            let mut ei = 0.0f64;
            for t in 0..n {
                let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                let (s, c) = a.sin_cos();
                er += input[t] as f64 * c;
                ei += input[t] as f64 * s;
            }
            assert!((sr[k] as f64 - er).abs() < 1e-3, "k={k} re");
            assert!((si[k] as f64 - ei).abs() < 1e-3, "k={k} im");
        }
    }
}
