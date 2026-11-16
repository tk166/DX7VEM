//! 原生渲染 CLI：产出与参考实现同参数可对拍的 WAV。
//!
//! 用法：
//!   cargo run --release --bin render -- --instrument violin --note 69 --dur 3 --out out/vem-69.wav
//!   可选：--tail 1.0 --sr 48000 --vel 0.8 --art arco --bow 0.5 --vibrato 0.4 --pcm16 --seed 12345

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use vem::{Articulation, Engine, InstrumentType};

struct Args {
    map: Vec<(String, String)>,
}

impl Args {
    fn parse() -> Self {
        let mut map = Vec::new();
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some(key) = a.strip_prefix("--") {
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

    fn raw(&self, key: &str) -> Option<&str> {
        self.map
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn flag(&self, key: &str) -> bool {
        self.raw(key).is_some()
    }

    fn f32(&self, key: &str, default: f32) -> f32 {
        self.raw(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    fn i32(&self, key: &str, default: i32) -> i32 {
        self.raw(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    fn string(&self, key: &str, default: &str) -> String {
        self.raw(key).unwrap_or(default).to_string()
    }
}

fn write_wav_f32(path: &str, sample_rate: u32, left: &[f32], right: &[f32]) -> std::io::Result<()> {
    write_wav(path, sample_rate, left, right, 32)
}

fn write_wav(
    path: &str,
    sample_rate: u32,
    left: &[f32],
    right: &[f32],
    bits: u16,
) -> std::io::Result<()> {
    if let Some(dir) = Path::new(path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let frames = left.len().min(right.len());
    let channels: u16 = 2;
    let bps = (bits / 8) as usize;
    let data_len = frames * channels as usize * bps;
    let fmt_len: u32 = if bits == 32 { 18 } else { 16 };
    let riff_len = 4 + (8 + fmt_len) + (8 + data_len as u32);
    let mut w = BufWriter::new(File::create(path)?);

    w.write_all(b"RIFF")?;
    w.write_all(&riff_len.to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&fmt_len.to_le_bytes())?;
    w.write_all(&(if bits == 32 { 3u16 } else { 1u16 }).to_le_bytes())?; // 3 = IEEE float
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&(sample_rate * channels as u32 * bps as u32).to_le_bytes())?;
    w.write_all(&((channels as usize * bps) as u16).to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    if bits == 32 {
        w.write_all(&0u16.to_le_bytes())?; // cbSize
    }
    w.write_all(b"data")?;
    w.write_all(&(data_len as u32).to_le_bytes())?;
    for i in 0..frames {
        for ch in [left[i], right[i]] {
            if bits == 32 {
                w.write_all(&ch.to_le_bytes())?;
            } else {
                let v = (ch.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                w.write_all(&v.to_le_bytes())?;
            }
        }
    }
    w.flush()?;
    Ok(())
}

fn main() {
    let args = Args::parse();
    if args.flag("help") {
        eprintln!(
            "用法: render --instrument violin --note 69 --dur 3 [--tail 1] [--sr 48000]\n\
             \x20     [--vel 0.8] [--art arco] [--bow 0.5] [--vibrato 0.4] [--pcm16]\n\
             \x20     [--seed 12345] --out out/vem-69.wav"
        );
        return;
    }

    let instrument = InstrumentType::parse(&args.string("instrument", "violin"))
        .unwrap_or_else(|| panic!("未知乐器: {}", args.string("instrument", "")));
    let note = args.i32("note", 69);
    let dur = args.f32("dur", 3.0).max(0.0);
    let tail = args.f32("tail", 1.0).max(0.0);
    let sample_rate = args.i32("sr", 48000).max(8000) as f32;
    let velocity = args.f32("vel", 0.8).clamp(0.0, 1.0);
    let bow = args.f32("bow", 0.5).clamp(0.0, 1.0);
    let vibrato = args.f32("vibrato", 0.4).max(0.0);
    let art = Articulation::parse(&args.string("art", "arco")).unwrap_or(Articulation::Arco);
    let seed = args.i32("seed", 12345) as u32;
    let out = args.string("out", "out/vem-render.wav");
    let pcm16 = args.flag("pcm16");

    let mut engine = Engine::new(sample_rate, instrument);
    engine.rng.reset(seed);
    engine.set_gain(1.0);
    engine.vibrato_depth = vibrato;
    engine.bow_position = bow;
    engine.articulation = art;

    let total = ((dur + tail) * sample_rate).round() as usize;
    let note_off_at = (dur * sample_rate).round() as usize;
    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];

    engine.note_on(note, velocity);

    // 分块渲染，在 dur 处松键（块长 1024，音符边界误差 < 21 ms 在该块内是逐采样精确的）
    let chunk = 1024;
    let mut pos = 0;
    let mut released = false;
    while pos < total {
        if !released && pos >= note_off_at {
            engine.note_off(note);
            released = true;
        }
        let mut end = (pos + chunk).min(total);
        if !released && note_off_at < end {
            end = note_off_at;
        }
        engine.render(&mut left[pos..end], &mut right[pos..end]);
        pos = end;
    }
    if !released {
        engine.note_off(note);
    }

    let peak = left
        .iter()
        .chain(right.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()));
    let rms = {
        let sum: f64 = left
            .iter()
            .chain(right.iter())
            .map(|v| (*v as f64) * (*v as f64))
            .sum();
        (sum / (2 * total).max(1) as f64).sqrt()
    };

    if let Err(e) = write_wav_f32(&out, sample_rate as u32, &left, &right) {
        eprintln!("写出失败 {}: {e}", out);
        std::process::exit(1);
    }

    println!(
        "{} {} note={} dur={:.2}s tail={:.2}s sr={} vel={:.2} art={} bow={:.2} vib={:.2} seed={}",
        instrument.name(),
        out,
        note,
        dur,
        tail,
        sample_rate as i32,
        velocity,
        art.name(),
        bow,
        vibrato,
        seed
    );
    println!(
        "  frames={} peak={:.4} rms={:.5} ({:.2} dBFS)",
        total,
        peak,
        rms,
        20.0 * (rms.max(1e-12) as f32).log10()
    );

    if pcm16 {
        let pcm_path = out.replace(".wav", "-pcm16.wav");
        let _ = write_wav(&pcm_path, sample_rate as u32, &left, &right, 16);
        println!("  额外写出 16-bit: {pcm_path}");
    }
}
