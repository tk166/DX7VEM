//! 琴体：短 IR 卷积。
//!
//! 结构（与参考实现等价，但把"大 FFT 尖峰"拆掉了）：
//! - IR 前 16 个 tap：直接时域卷积（零延迟）；
//! - 16..512：2 倍递增的分块 FFT（和参考一样，小块、便宜）；
//! - 512 以后：**均匀分块 + 频域延迟线（FDL）**。因为这段 IR 本身有 512 采样的延迟，
//!   尾部结果晚一个块输出正好抵消 → 整体仍是零延迟、同一个线性卷积。
//!
//! 旧版对尾巴用 32768 点的大 FFT，每隔 8192/16384 采样会在单个 64 采样块里爆一次，
//! 在 Safari 的 WebAssembly 上直接造成 ~185 ms 周期的卡顿；改成均匀分块后每块工作量恒定。

use crate::fft::FftCache;

const DIRECT_TAPS: usize = 16;
const BLOCK: usize = 512; // 尾部均匀分块长度（= 延迟补偿量）
const N: usize = 2 * BLOCK; // overlap-save 窗长
const BINS: usize = N / 2 + 1;

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

    // 尾部均匀分块（FDL）
    tail_parts: usize,
    tail_h_re: Vec<f32>, // [part][BINS]
    tail_h_im: Vec<f32>,
    tail_x_re: Vec<f32>, // [part][BINS] 输入谱历史（环形）
    tail_x_im: Vec<f32>,
    tail_y_re: Vec<f32>, // 累加谱
    tail_y_im: Vec<f32>,
    tail_time: Vec<f32>, // 2B 时域缓冲
    tail_buf: Vec<f32>,  // 当前待输出的 B 个尾部样本
    tail_pos: usize,
    tail_ready: bool,
    tail_seen: usize, // 已经处理过多少个输入块（用于环形历史）
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
            tail_parts: 0,
            tail_h_re: Vec::new(),
            tail_h_im: Vec::new(),
            tail_x_re: Vec::new(),
            tail_x_im: Vec::new(),
            tail_y_re: Vec::new(),
            tail_y_im: Vec::new(),
            tail_time: Vec::new(),
            tail_buf: Vec::new(),
            tail_pos: 0,
            tail_ready: false,
            tail_seen: 0,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn clear(&mut self) {
        self.input_ring.fill(0.0);
        self.output_ring.fill(0.0);
        self.position = 0;
        self.tail_x_re.fill(0.0);
        self.tail_x_im.fill(0.0);
        self.tail_buf.fill(0.0);
        self.tail_pos = 0;
        self.tail_ready = false;
        self.tail_seen = 0;
    }

    /// 设置 IR（单声道，48 kHz）。
    pub fn set_ir(&mut self, ir: &[f32], fft: &mut FftCache) {
        if ir.is_empty() {
            self.ready = false;
            return;
        }
        let mut padded = ir.to_vec();
        // 头部（直接/倍增分块）覆盖到 512；更短的 IR 用 2 的幂封口，避免漏掉最后一小段
        let head_len = if ir.len() <= BLOCK {
            ir.len().next_power_of_two().max(2 * DIRECT_TAPS).min(BLOCK)
        } else {
            BLOCK
        };
        if padded.len() < head_len {
            padded.resize(head_len, 0.0);
        }
        let direct_width = DIRECT_TAPS.min(head_len);
        let mut blocks = Vec::new();
        let mut bw = 2 * direct_width;
        while bw <= head_len {
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

        // 环形缓冲只需覆盖倍增分块的最大宽度
        let ring_width = (2 * BLOCK).max(2 * direct_width);
        self.mask = ring_width - 1;
        self.input_ring = vec![0.0; ring_width];
        self.output_ring = vec![0.0; ring_width];
        self.position = 0;
        self.direct = padded[..direct_width].to_vec();
        self.blocks = blocks;
        self.real_tmp = vec![0.0; ring_width];
        self.spec_re = vec![0.0; ring_width / 2 + 1];
        self.spec_im = vec![0.0; ring_width / 2 + 1];
        self.block_out = vec![0.0; ring_width];

        // FDL 尾巴
        let tail_start = head_len;
        if padded.len() > tail_start {
            let tail_len = padded.len() - tail_start;
            let parts = (tail_len + BLOCK - 1) / BLOCK;
            self.tail_parts = parts;
            self.tail_h_re = vec![0.0; parts * BINS];
            self.tail_h_im = vec![0.0; parts * BINS];
            self.tail_x_re = vec![0.0; parts * BINS];
            self.tail_x_im = vec![0.0; parts * BINS];
            self.tail_y_re = vec![0.0; BINS];
            self.tail_y_im = vec![0.0; BINS];
            self.tail_time = vec![0.0; N];
            self.tail_buf = vec![0.0; BLOCK];
            for p in 0..parts {
                let mut tmp = vec![0.0f32; N];
                let start = tail_start + p * BLOCK;
                let end = (start + BLOCK).min(padded.len());
                let len = end - start;
                tmp[..len].copy_from_slice(&padded[start..end]);
                let mut hr = vec![0.0f32; BINS];
                let mut hi = vec![0.0f32; BINS];
                fft.forward_real(&tmp, &mut hr, &mut hi);
                self.tail_h_re[p * BINS..(p + 1) * BINS].copy_from_slice(&hr);
                self.tail_h_im[p * BINS..(p + 1) * BINS].copy_from_slice(&hi);
            }
            self.tail_seen = 0;
            self.tail_pos = 0;
            self.tail_ready = false;
        } else {
            self.tail_parts = 0;
            self.tail_h_re.clear();
            self.tail_h_im.clear();
            self.tail_x_re.clear();
            self.tail_x_im.clear();
            self.tail_y_re.clear();
            self.tail_y_im.clear();
            self.tail_time.clear();
            self.tail_buf.clear();
        }
        padded.clear();
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

        // 16..512：倍增分块 FFT（小块，便宜）
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

        let mut result = self.output_ring[self.position];

        // 尾巴：每 512 采样算一次，下一个块按采样输出（延迟 512 = IR 尾本身的延迟）
        if self.tail_ready {
            result += self.tail_buf[self.tail_pos];
            self.tail_pos += 1;
            if self.tail_pos >= BLOCK {
                self.tail_pos = 0;
                self.tail_ready = false;
            }
        }

        self.output_ring[self.position] = 0.0;
        self.position = (self.position + 1) & mask;

        if self.tail_parts > 0 && self.position % BLOCK == 0 {
            self.compute_tail(fft);
        }

        result
    }

    /// 处理一个 512 采样的输入块：算输入谱、乘所有 IR 分块、IFFT，存到下一块的输出缓冲。
    fn compute_tail(&mut self, fft: &mut FftCache) {
        let parts = self.tail_parts;
        if parts == 0 {
            return;
        }
        let mask = self.mask;
        // 窗：最近 N 个采样（= 上一块 + 当前块）；position 已前进到下一块开头
        for i in 0..N {
            self.tail_time[i] = self.input_ring[(self.position + i) & mask];
        }
        fft.forward_real(&self.tail_time, &mut self.tail_y_re[..], &mut self.tail_y_im[..]);

        // 存入输入谱历史（环形）
        let slot = self.tail_seen % parts;
        let off = slot * BINS;
        self.tail_x_re[off..off + BINS].copy_from_slice(&self.tail_y_re);
        self.tail_x_im[off..off + BINS].copy_from_slice(&self.tail_y_im);

        // 累加：Y += H_p * X_{k-p}
        for i in 0..BINS {
            let mut sr = 0.0f32;
            let mut si = 0.0f32;
            for p in 0..parts {
                let xs = (self.tail_seen + parts - p) % parts;
                let xo = xs * BINS;
                let ho = p * BINS;
                let xr = self.tail_x_re[xo + i];
                let xi = self.tail_x_im[xo + i];
                let hr = self.tail_h_re[ho + i];
                let hi = self.tail_h_im[ho + i];
                sr += xr * hr - xi * hi;
                si += xr * hi + xi * hr;
            }
            self.tail_y_re[i] = sr;
            self.tail_y_im[i] = si;
        }
        self.tail_seen += 1;

        fft.inverse_real(&self.tail_y_re, &self.tail_y_im, &mut self.tail_time);
        let scale = 1.0 / N as f32;
        // overlap-save 的有效输出是第二半
        for i in 0..BLOCK {
            self.tail_buf[i] = self.tail_time[BLOCK + i] * scale;
        }
        self.tail_pos = 0;
        self.tail_ready = true;
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

    #[test]
    fn matches_direct_convolution_short() {
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
    fn matches_direct_convolution_long() {
        let mut rng = Random::with_seed(7);
        let ir: Vec<f32> = (0..3000).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
        let input: Vec<f32> = (0..2500).map(|_| rng.get_uniform() * 2.0 - 1.0).collect();
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
                (got as f64 - expect).abs() < 5e-3,
                "n={n} got={got} expect={expect}"
            );
        }
    }
}
