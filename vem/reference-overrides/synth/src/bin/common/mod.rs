// 本地对拍工具（不属于上游 VEM）：参数解析 + float32 WAV 写出。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct Args {
    map: Vec<(String, String)>,
}

impl Args {
    pub fn parse() -> Self {
        let mut map = Vec::new();
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            if let Some(key) = argv[i].strip_prefix("--") {
                let (k, v) = if let Some((k, v)) = key.split_once('=') {
                    (k.to_string(), v.to_string())
                } else if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                    i += 1;
                    (key.to_string(), argv[i].clone())
                } else {
                    (key.to_string(), "1".to_string())
                };
                map.push((k, v));
            }
            i += 1;
        }
        Self { map }
    }

    pub fn raw(&self, key: &str) -> Option<&str> {
        self.map
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn f32(&self, key: &str, default: f32) -> f32 {
        self.raw(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn i32(&self, key: &str, default: i32) -> i32 {
        self.raw(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn string(&self, key: &str, default: &str) -> String {
        self.raw(key).unwrap_or(default).to_string()
    }
}

/// VEM_SEED：override 的 random.rs 会让 Random::new() 读这个环境变量。
/// 必须在创建 Instrument / Director **之前**调用，参考渲染才可复现。
pub fn set_seed(args: &Args) {
    let seed = args.i32("seed", 12345);
    std::env::set_var("VEM_SEED", seed.to_string());
}

/// 写出 32-bit float 立体声 WAV（分析/对拍用，不削顶）。
pub fn write_wav_f32(path: &str, sample_rate: u32, left: &[f32], right: &[f32]) -> std::io::Result<()> {
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let frames = left.len().min(right.len());
    let channels: u16 = 2;
    let bps = 4usize;
    let data_len = frames * channels as usize * bps;
    let fmt_len: u32 = 18;
    let riff_len = 4 + (8 + fmt_len) + (8 + data_len as u32);
    let mut w = BufWriter::new(File::create(path)?);

    w.write_all(b"RIFF")?;
    w.write_all(&riff_len.to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&fmt_len.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // IEEE float
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * channels as u32 * bps as u32).to_le_bytes())?;
    w.write_all(&((channels as usize * bps) as u16).to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(&0u16.to_le_bytes())?; // cbSize
    w.write_all(b"data")?;
    w.write_all(&(data_len as u32).to_le_bytes())?;
    for i in 0..frames {
        w.write_all(&left[i].to_le_bytes())?;
        w.write_all(&right[i].to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

pub fn peak_rms(left: &[f32], right: &[f32]) -> (f32, f32) {
    let peak = left
        .iter()
        .chain(right.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    let sum: f64 = left
        .iter()
        .chain(right.iter())
        .map(|v| (*v as f64) * (*v as f64))
        .sum();
    let rms = (sum / (left.len() + right.len()).max(1) as f64).sqrt() as f32;
    (peak, rms)
}
