//! 总线混响：Freeverb（8 comb + 4 allpass / 声道）。
//! 移植自我们 DX7 版 `msfa/msfa_wrapper.cpp`（参数曲线一致）。

const NCOMB: usize = 8;
const NALL: usize = 4;
const COMB_TUNE: [usize; NCOMB] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALL_TUNE: [usize; NALL] = [556, 441, 341, 225];
const COMB_SPREAD: usize = 23;

struct Comb {
    buf: Vec<f32>,
    idx: usize,
    store: f32,
    damp1: f32,
    damp2: f32,
}

impl Comb {
    fn new(n: usize) -> Self {
        Self {
            buf: vec![0.0; n],
            idx: 0,
            store: 0.0,
            damp1: 0.2,
            damp2: 0.8,
        }
    }

    fn set_damp(&mut self, d: f32) {
        self.damp1 = d;
        self.damp2 = 1.0 - d;
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.idx = 0;
        self.store = 0.0;
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32) -> f32 {
        let out = self.buf[self.idx];
        self.store = out * self.damp2 + self.store * self.damp1;
        self.buf[self.idx] = input + self.store * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(n: usize) -> Self {
        Self {
            buf: vec![0.0; n],
            idx: 0,
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.idx = 0;
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let b = self.buf[self.idx];
        let out = -input + b;
        self.buf[self.idx] = input + b * 0.5;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

pub struct Freeverb {
    comb_l: Vec<Comb>,
    comb_r: Vec<Comb>,
    all_l: Vec<Allpass>,
    all_r: Vec<Allpass>,
    feedback: f32,
    wet1: f32,
    wet2: f32,
}

impl Freeverb {
    pub fn new(sr: f32) -> Self {
        let scale = sr / 44100.0;
        let mut comb_l = Vec::with_capacity(NCOMB);
        let mut comb_r = Vec::with_capacity(NCOMB);
        for &t in COMB_TUNE.iter() {
            comb_l.push(Comb::new(((t as f32) * scale) as usize));
            comb_r.push(Comb::new((((t + COMB_SPREAD) as f32) * scale) as usize));
        }
        let mut all_l = Vec::with_capacity(NALL);
        let mut all_r = Vec::with_capacity(NALL);
        for &t in ALL_TUNE.iter() {
            all_l.push(Allpass::new(((t as f32) * scale) as usize));
            all_r.push(Allpass::new((((t + COMB_SPREAD) as f32) * scale) as usize));
        }
        let mut r = Self {
            comb_l,
            comb_r,
            all_l,
            all_r,
            feedback: 0.84,
            wet1: 1.0,
            wet2: 0.0,
        };
        r.set_params(0.5, 0.5, 1.0);
        r
    }

    pub fn set_params(&mut self, size: f32, dampness: f32, width: f32) {
        self.feedback = 0.70 + 0.28 * size.clamp(0.0, 1.0);
        let damp = 0.05 + 0.85 * dampness.clamp(0.0, 1.0);
        for c in self.comb_l.iter_mut().chain(self.comb_r.iter_mut()) {
            c.set_damp(damp);
        }
        self.wet1 = width * 0.5 + 0.5;
        self.wet2 = (1.0 - width) * 0.5;
    }

    pub fn clear(&mut self) {
        for c in self.comb_l.iter_mut().chain(self.comb_r.iter_mut()) {
            c.clear();
        }
        for a in self.all_l.iter_mut().chain(self.all_r.iter_mut()) {
            a.clear();
        }
    }

    /// 输入同时包含 L/R，输出直接写回（湿声）。
    pub fn process_block(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        for i in 0..n {
            let input = (l[i] + r[i]) * 0.015;
            let mut out_l = 0.0f32;
            let mut out_r = 0.0f32;
            for c in 0..NCOMB {
                out_l += self.comb_l[c].process(input, self.feedback);
                out_r += self.comb_r[c].process(input, self.feedback);
            }
            for a in 0..NALL {
                out_l = self.all_l[a].process(out_l);
                out_r = self.all_r[a].process(out_r);
            }
            l[i] = out_l * self.wet1 + out_r * self.wet2;
            r[i] = out_r * self.wet1 + out_l * self.wet2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_produces_decaying_tail() {
        let mut rv = Freeverb::new(48000.0);
        rv.set_params(0.7, 0.4, 1.0);
        let n = 48000;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        l[0] = 1.0;
        r[0] = 1.0;
        rv.process_block(&mut l, &mut r);
        let early = l[100..2000].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let late = l[20000..30000].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(early > 1e-4, "early={early}");
        assert!(late < early, "没有衰减: early={early} late={late}");
        assert!(l.iter().all(|v| v.is_finite()));
    }
}
