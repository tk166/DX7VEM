// 直接驱动单个 Instrument —— M1 谱合成核心的"唯一真值"。
//
// 为什么不用 Director：Director 会把琴体 IR(0.18) 和弓噪混进来，M1 只验证
// instrument.rs 的谱合成；这里用与 Director 完全相同的 Arco 包络时序来驱动它，
// 于是我们的引擎只要实现相同的包络即可逐窗口/逐采样对拍。
//
// 用法:
//   ref-instrument --instrument violin --note 69 --dur 3 --tail 1 --out out/ref-69.wav
// 可选: --vel 0.8 --bow 0.5 --vibrato 0.4 --release 0.5 --seed 12345

mod common;

use common::{peak_rms, set_seed, write_wav_f32, Args};
use realfft::RealFftPlanner;
use std::f32::consts::PI;
use synth::instrument::Instrument;
use synth::{Articulation, InstrumentType, SAMPLE_RATE};

fn parse_instrument(s: &str) -> InstrumentType {
    match s.to_ascii_lowercase().as_str() {
        "violin" | "vln" => InstrumentType::Violin,
        "viola" | "vla" => InstrumentType::Viola,
        "cello" | "vc" => InstrumentType::Cello,
        "bass" | "cb" => InstrumentType::Bass,
        _ => panic!("未知乐器: {s}"),
    }
}

fn parse_articulation(s: &str) -> Articulation {
    match s.to_ascii_lowercase().as_str() {
        "arco" => Articulation::Arco,
        "marcato" => Articulation::Marcato,
        "glissando" => Articulation::Glissando,
        "spiccato" => Articulation::Spiccato,
        "pizzicato" => Articulation::Pizzicato,
        "collegno" => Articulation::ColLegno,
        "tremolo" => Articulation::Tremolo,
        _ => panic!("未知弓法: {s}"),
    }
}

/// Director::update_transitions 的形状：raised-cosine 插值。
fn weight(step: i64, start: i64, end: i64) -> f32 {
    let j = step;
    if j >= start {
        if j < end {
            let fraction = (j - start) as f32 / (end - start) as f32;
            0.5 - 0.5 * (fraction * PI).cos()
        } else {
            1.0
        }
    } else {
        0.0
    }
}

fn main() {
    let args = Args::parse();
    let itype = parse_instrument(&args.string("instrument", "violin"));
    let note = args.i32("note", 69);
    let dur = args.f32("dur", 3.0).max(0.0);
    let tail = args.f32("tail", 1.0).max(0.0);
    let vel = args.f32("vel", 0.8).clamp(0.0, 1.0);
    let bow = args.f32("bow", 0.5).clamp(0.0, 1.0);
    let vibrato = args.f32("vibrato", 0.4).max(0.0);
    let release_rate = args.f32("release", 0.5).clamp(0.0, 1.0);
    let art = parse_articulation(&args.string("art", "arco"));
    let out = args.string("out", "out/ref-instrument.wav");
    let sr = SAMPLE_RATE as f32;
    set_seed(&args);

    if art != Articulation::Arco {
        eprintln!("ref-instrument 只复刻了 Arco 的包络；其它弓法请用 ref-render（Director 全链路）。");
        std::process::exit(2);
    }

    let frames = ((dur + tail) * sr) as usize;
    let note_off_step = (dur * sr) as i64;

    // Director 的 Arco 参数
    let attack_time = 1000 + (30000.0 * (1.0 - vel)) as i64;
    let bow_transition = 10000i64;
    let release_time = 1000 + (10000.0 * (1.0 - release_rate)) as i64;

    let freq = 440.0 * 2.0f32.powf((note - 69) as f32 / 12.0);

    let mut inst = Instrument::new(itype, 0);
    inst.set_frequency(freq);
    inst.set_volume(0.0);
    inst.set_bow_position(0.0); // bow + shift(-bow) = 0
    inst.set_vibrato_amplitude(0.0);
    inst.note_on(note, art);

    let mut planner = RealFftPlanner::<f32>::new();
    let mut mono = Vec::with_capacity(frames);

    let mut step: i64 = 0;
    let mut envelope = 0.0f32;
    let mut release: Option<(i64, f32)> = None;

    for _ in 0..frames {
        // Director: if step%100 == 0 { process_messages() } —— 松键消息只在这个边界生效
        if step % 100 == 0 && release.is_none() && step >= note_off_step {
            release = Some((step, envelope));
        }

        // Director: step += 1; 然后 Division: if step%100 == 0 { update_transitions() }
        step += 1;
        if step % 100 == 0 {
            if let Some((rstart, from)) = release {
                let w = weight(step, rstart, rstart + release_time);
                envelope = from * (1.0 - w);
            } else {
                envelope = weight(step, 0, attack_time);
            }
            let bow_shift = -bow * (1.0 - weight(step, 0, bow_transition));
            inst.set_volume(envelope);
            inst.set_bow_position((bow + bow_shift).max(0.0));
            inst.set_vibrato_amplitude(0.01 * vibrato * envelope);
            inst.set_frequency(freq);
        }

        mono.push(inst.generate(&mut planner));
    }

    if let Err(e) = write_wav_f32(&out, SAMPLE_RATE as u32, &mono, &mono) {
        eprintln!("写出失败 {out}: {e}");
        std::process::exit(1);
    }
    let (peak, rms) = peak_rms(&mono, &mono);
    println!(
        "ref-instrument {} note={} dur={:.2}s tail={:.2}s sr={} vel={:.2} art={} bow={:.2} vib={:.2} (attack {} samples, release {} samples)",
        itype_name(itype),
        note,
        dur,
        tail,
        SAMPLE_RATE,
        vel,
        "arco",
        bow,
        vibrato,
        attack_time,
        release_time
    );
    println!(
        "  frames={} peak={:.4} rms={:.5} ({:.2} dBFS)",
        frames,
        peak,
        rms,
        20.0 * rms.max(1e-12).log10()
    );
}

fn itype_name(t: InstrumentType) -> &'static str {
    match t {
        InstrumentType::Violin => "violin",
        InstrumentType::Viola => "viola",
        InstrumentType::Cello => "cello",
        InstrumentType::Bass => "bass",
    }
}
