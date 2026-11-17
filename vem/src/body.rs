//! 琴体：短 IR 卷积。
//!
//! 结构与参考实现一致（`synth/src/reverb.rs`）：
//! - IR 前 16 个 tap 直接时域卷积（零延迟）；
//! - 其余部分按 2 倍递增的分块做 FFT 卷积，结果加到一个环形输出缓冲的"未来"位置。
//!
//! 这样既保持 0 采样延迟，又能和参考实现逐采样对齐（同一线性卷积，只有舍入差）。

use crate::fft::FftCache;

const DIRECT_TAPS: usize = 16;

struct Block {
    width: usize,
    half: usize,
    spec_re: Vec<f32>,
    spec_im: Vec<f32>,
}

pub struct BodyConvolver {
    ready: bool,
    mask: usize,
    input_ring: Vec<f32>,
    output_ring: Vec<f32>,
    position: usize,
    direct: Vec<f32>,
    blocks: Vec<Block>,
    real_tmp: Vec<f32>,
    spec_re: Vec<f32>,
    spec_im: Vec<f32>,
    block_out: Vec<f32>,
}

impl BodyConvolver {
    pub fn new() -> Self {
        Self {
            ready: false,
            mask: 0,
            input_ring: Vec::new(),
            output_ring: Vec::new(),
            position: 0,
            direct: Vec::new(),
            blocks: Vec::new(),
            real_tmp: Vec::new(),
            spec_re: Vec::new(),
            spec_im: Vec::new(),
            block_out: Vec::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn clear(&mut self) {
        self.input_ring.fill(0.0);
        self.output_ring.fill(0.0);
        self.position = 0;
    }

    /// 设置 IR（单声道，48 kHz）。长度会被补零到 2 的幂。
    pub fn set_ir(&mut self, ir: &[f32], fft: &mut FftCache) {
        if ir.is_empty() {
            self.ready = false;
            return;
        }
        let mut width = 1usize;
        while width < ir.len() {
            width *= 2;
        }
        let mut padded = ir.to_vec();
        padded.resize(width, 0.0);

        let direct_width = DIRECT_TAPS.min(width);
        let mut blocks = Vec::new();
        let mut bw = 2 * direct_width;
        while bw <= width {
            let half = bw / 2;
            let bins = bw / 2 + 1;
            let mut tmp = vec![0.0f32; bw];
            tmp[..half].copy_from_slice(&padded[half..bw]);
            let mut spec_re = vec![0.0f32; bins];
            let mut spec_im = vec![0.0f32; bins];
            fft.forward_real(&tmp, &mut spec_re, &mut spec_im);
            blocks.push(Block {
                width: bw,
                half,
                spec_re,
                spec_im,
            });
            bw *= 2;
        }

        self.mask = 2 * width - 1;
        self.input_ring = vec![0.0; 2 * width];
        self.output_ring = vec![0.0; 2 * width];
        self.position = 0;
        self.direct = padded[..direct_width].to_vec();
        self.blocks = blocks;
        self.real_tmp = vec![0.0; 2 * width];
        self.spec_re = vec![0.0; width / 2 + 1];
        self.spec_im = vec![0.0; width / 2 + 1];
        self.block_out = vec![0.0; width];
        self.ready = true;
    }

    /// 逐采样卷积。
    pub fn process(&mut self, input: f32, fft: &mut FftCache) -> f32 {
        if !self.ready {
            return 0.0;
        }
        let mask = self.mask;
        self.input_ring[self.position] = input;

        // 前 16 个 tap：直接卷积（零延迟）
        for i in 0..self.direct.len() {
            self.output_ring[(self.position + i) & mask] += input * self.direct[i];
        }

        // 倍增分块 FFT
        for bi in 0..self.blocks.len() {
            let (width, half) = {
                let b = &self.blocks[bi];
                (b.width, b.half)
            };
            if self.position % half != half - 1 {
                continue;
            }
            let start = self.position + 1 - half;
            self.real_tmp[..half].copy_from_slice(&self.input_ring[start..start + half]);
            for i in half..width {
                self.real_tmp[i] = 0.0;
            }
            let bins = width / 2 + 1;
            fft.forward_real(
                &self.real_tmp[..width],
                &mut self.spec_re[..bins],
                &mut self.spec_im[..bins],
            );
            {
                let block = &self.blocks[bi];
                for i in 0..bins {
                    let xr = self.spec_re[i];
                    let xi = self.spec_im[i];
                    let hr = block.spec_re[i];
                    let hi = block.spec_im[i];
                    self.spec_re[i] = xr * hr - xi * hi;
                    self.spec_im[i] = xr * hi + xi * hr;
                }
            }
            fft.inverse_real(
                &self.spec_re[..bins],
                &self.spec_im[..bins],
                &mut self.block_out[..width],
            );
            let scale = 1.0 / width as f32;
            for i in 0..width {
                self.output_ring[(self.position + 1 + i) & mask] += scale * self.block_out[i];
            }
        }

        let result = self.output_ring[self.position];
        self.output_ring[self.position] = 0.0;
        self.position = (self.position + 1) & mask;
        result
    }
}

impl Default for BodyConvolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random::Random;

    /// 与朴素线性卷积逐点对比（IR 足够短，直接卷积可算）。
    #[test]
    fn matches_direct_convolution() {
        let mut rng = Random::with_seed(2024);
        let ir: Vec<f32> = (0..300).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
        let input: Vec<f32> = (0..800).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
        let mut fft = FftCache::new();
        let mut conv = BodyConvolver::new();
        conv.set_ir(&ir, &mut fft);
        for (n, &x) in input.iter().enumerate() {
            let got = conv.process(x, &mut fft);
            let mut expect = 0.0f64;
            for (k, &h) in ir.iter().enumerate() {
                if n >= k {
                    expect += (input[n - k] * h) as f64;
                }
            }
            assert!(
                (got as f64 - expect).abs() < 2e-3,
                "n={n} got={got} expect={expect}"
            );
        }
    }

    #[test]
    fn long_ir_does_not_blow_up() {
        let mut rng = Random::with_seed(9);
        let ir: Vec<f32> = (0..40000).map(|_| (rng.get_uniform() - 0.5) * 1e-3).collect();
        let mut fft = FftCache::new();
        let mut conv = BodyConvolver::new();
        conv.set_ir(&ir, &mut fft);
        let mut peak = 0.0f32;
        for i in 0..48000 {
            let x = ((i as f32) * 0.01).sin() * 0.5;
            let y = conv.process(x, &mut fft);
            assert!(y.is_finite());
            peak = peak.max(y.abs());
        }
        assert!(peak < 10.0, "peak={peak}");
    }
}
