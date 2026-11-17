//! 原生渲染 CLI：产出与参考实现同参数可对拍的 WAV。
//!
//! 用法：
//!   cargo run --release --bin render -- --instrument violin --note 69 --dur 3 --out out/vem-69.wav
//!
//! 对拍（与 ref-instrument 直接比较，需关掉总线缩放）：
//!   ... --raw --seed 12345
//!
//! 可选参数：
//!   --tail 1.0 --sr 48000 --vel 0.8 --art arco --bow 0.5 --vibrato 0.4 --seed 12345
//!   --raw（output_gain=1，跳过软限幅） --pcm16
//!   --attack-ms 20.83 --attack-vel-ms 625 --release-ms 20.83 --release-slow-ms 208.33
//!   --bow-settle-ms 208.33 --vib-onset-ms 0 --vib-onset-ramp-ms 60 --release-rate 0.5
//!   --oversample auto|on|off --harmonics --volume 1 --bend 0
//!   --damping 1 --tilt 0 --bow-focus 1 --cutoff 1 --phase 1

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use vem::{Articulation, Engine, InstrumentType, TimbreParams};

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

fn write_wav(path: &str, sample_rate: u32, left: &[f32], right: &[f32], bits: u16) -> std::io::Result<()> {
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
        eprintln!("见本文件顶部注释：render --instrument violin --note 69 --dur 3 --out out/vem-69.wav [--raw]");
        return;
    }

    let instrument = InstrumentType::parse(&args.string("instrument", "violin"))
        .unwrap_or_else(|| panic!("未知乐器: {}", args.string("instrument", "")));
    let note = args.i32("note", 69);
    let note2 = args.i32("note2", note);
    let has_note2 = args.raw("note2").is_some();
    let note2_at = args.f32("note2-at", 1.5).max(0.0);
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
    engine.seed = seed;
    engine.reset();
    engine.polyphonic = args.flag("poly");
    engine.set_gain(args.f32("gain", 1.0));
    engine.master = args.f32("master", 1.0);
    engine.volume = args.f32("volume", 1.0).clamp(0.0, 1.0);
    engine.vibrato_depth = vibrato;
    engine.bow_position = bow;
    engine.bow_pressure = args.f32("bow-pressure", 1.0).clamp(0.0, 1.0);
    engine.release_rate = args.f32("release-rate", 0.5).clamp(0.0, 1.0);
    engine.articulation = art;
    engine.bend = (2.0f32).powf(args.f32("bend", 0.0) / 12.0);
    engine.harmonics = args.flag("harmonics");

    // 包络：只有显式给了参数才覆盖（避免 ms↔sample 往返带来 1 采样误差）
    if args.raw("attack-ms").is_some() || args.raw("attack-vel-ms").is_some() {
        engine.envelope.set_attack_ms(
            args.f32("attack-ms", engine.envelope.attack_ms()),
            args.f32("attack-vel-ms", engine.envelope.attack_velocity_ms()),
        );
    }
    if args.raw("release-ms").is_some() || args.raw("release-slow-ms").is_some() {
        engine.envelope.set_release_ms(
            args.f32("release-ms", engine.envelope.release_ms()),
            args.f32("release-slow-ms", engine.envelope.release_slow_ms()),
        );
    }
    if args.raw("bow-settle-ms").is_some() {
        engine.envelope.set_bow_settle_ms(args.f32("bow-settle-ms", engine.envelope.bow_settle_ms()));
    }
    engine.envelope.vibrato_onset_ms = args.f32("vib-onset-ms", 0.0).max(0.0);
    engine.envelope.vibrato_onset_ramp_ms = args.f32("vib-onset-ramp-ms", 60.0).max(0.0);
    engine.envelope.legato_start = args.f32("legato-start", 0.5).clamp(0.0, 1.0);
    engine.envelope.legato_ramp_ms = args.f32("legato-ramp-ms", 0.0).max(0.0);
    engine.envelope.legato_accent = args.f32("legato-accent", 0.0).clamp(0.0, 0.2);
    engine.envelope.legato_boost = args.f32("legato-boost", 0.0).clamp(0.0, 8.0);
    engine.envelope.legato_boost_ms = args.f32("legato-boost-ms", 25.0).clamp(1.0, 200.0);

    let mut timbre = TimbreParams::default();
    timbre.damping_scale = args.f32("damping", 1.0);
    timbre.tilt = args.f32("tilt", 0.0);
    timbre.bow_focus = args.f32("bow-focus", 1.0);
    timbre.cutoff_scale = args.f32("cutoff", 1.0);
    timbre.phase_scale = args.f32("phase", 1.0);
    engine.timbre = timbre;

    let raw = args.flag("raw");
    if raw {
        engine.output_gain = 1.0;
        engine.safe_clip = false;
    }

    let ov = args.string("oversample", "auto");
    engine.set_oversample(match ov.as_str() {
        "on" => 1,
        "off" => 2,
        _ => 0,
    });

    // 数据（琴体 IR / 弓噪）：--data <目录>，里面放 body-<instrument>.bin 与 noise.bin
    let mut data_info = String::from("无");
    if let Some(dir) = args.raw("data") {
        let base = Path::new(dir);
        let mut loaded = 0;
        for name in [
            format!("body-{}.bin", instrument.name()),
            "noise.bin".to_string(),
        ] {
            let path = base.join(&name);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    if engine.set_data(&bytes) {
                        loaded += 1;
                    } else {
                        eprintln!("数据格式不正确: {}", path.display());
                    }
                }
                Err(e) => eprintln!("读取失败 {}: {e}", path.display()),
            }
        }
        data_info = format!("{dir} ({loaded} 个文件)");
    }
    if args.flag("no-body") {
        engine.body_mix = 0.0;
    }
    if args.flag("no-noise") {
        engine.bow_noise_mode = 0;
    }
    if let Some(v) = args.raw("body-mix") {
        engine.body_mix = v.parse().unwrap_or(engine.body_mix);
    }
    if let Some(v) = args.raw("bow-noise") {
        engine.bow_noise_amount = v.parse().unwrap_or(engine.bow_noise_amount);
    }

    // 总线（DX7 版参数）：默认关（保证与参考逐采样可对拍）；--bus 打开
    let bus = args.flag("bus");
    if bus {
        engine.set_comp(
            true,
            args.f32("comp-thr", -12.0),
            args.f32("comp-ratio", 2.0),
            args.f32("comp-atk", 15.0),
            args.f32("comp-rel", 200.0),
            args.f32("comp-makeup", 1.0),
        );
        engine.set_reverb(
            args.f32("rev-mix", 0.25),
            args.f32("rev-size", 0.6),
            args.f32("rev-damp", 0.45),
            args.f32("rev-width", 1.0),
        );
        engine.safe_clip = true;
    }
    if let Some(v) = args.raw("rev-mix") {
        engine.set_reverb(
            v.parse().unwrap_or(0.25),
            args.f32("rev-size", 0.6),
            args.f32("rev-damp", 0.45),
            args.f32("rev-width", 1.0),
        );
    }

    let total = ((dur + tail) * sample_rate).round() as usize;
    let note_off_at = (dur * sample_rate).round() as usize;
    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];

    engine.note_on(note, velocity);
    let mut current_note = note;
    let mut sent_note2 = false;
    let note2_at_frame = (note2_at * sample_rate).round() as usize;

    // 分块渲染，换音/松键点精确到采样
    let chunk = 1024;
    let mut pos = 0;
    let mut released = false;
    while pos < total {
        if has_note2 && !sent_note2 && pos >= note2_at_frame {
            engine.note_on(note2, velocity);
            current_note = note2;
            sent_note2 = true;
        }
        if !released && pos >= note_off_at {
            engine.note_off(current_note);
            released = true;
        }
        let mut end = (pos + chunk).min(total);
        if !released && note_off_at < end {
            end = note_off_at;
        }
        if has_note2 && !sent_note2 && note2_at_frame < end {
            end = note2_at_frame;
        }
        engine.render(&mut left[pos..end], &mut right[pos..end]);
        pos = end;
    }
    if !released {
        engine.note_off(current_note);
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

    if let Err(e) = write_wav(&out, sample_rate as u32, &left, &right, 32) {
        eprintln!("写出失败 {}: {e}", out);
        std::process::exit(1);
    }

    println!(
        "{} {} note={} dur={:.2}s tail={:.2}s sr={} vel={:.2} art={} bow={:.2} vib={:.2} seed={} oversample={} raw={}",
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
        seed,
        if engine.oversample { "on" } else { "off" },
        raw
    );
    println!(
        "  frames={} peak={:.4} rms={:.5} ({:.2} dBFS)  control={} samples  attack={}  release={}",
        total,
        peak,
        rms,
        20.0 * (rms.max(1e-12) as f32).log10(),
        engine.control_samples(),
        engine.envelope.attack_samples(velocity, sample_rate),
        engine.envelope.release_samples(engine.release_rate, sample_rate)
    );
    println!(
        "  body={:.2} (ir={})  bow_noise={:.2} (mode={})  data={}",
        engine.body_mix,
        if engine.body.is_ready() { "有" } else { "无" },
        engine.bow_noise_amount,
        engine.bow_noise_mode,
        data_info
    );

    if pcm16 {
        let pcm_path = out.replace(".wav", "-pcm16.wav");
        let _ = write_wav(&pcm_path, sample_rate as u32, &left, &right, 16);
        println!("  额外写出 16-bit: {pcm_path}");
    }
}
