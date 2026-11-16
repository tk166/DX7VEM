// 直接驱动单个 Instrument —— M1 谱合成核心的"唯一真值"。
//
// 为什么不用 Director：Director 会把琴体 IR(0.18) 和弓噪混进来，M1 只验证
// instrument.rs 的谱合成；这里用与 Director 完全相同的 Arco 包络时序来驱动它，
// 于是我们的引擎只要实现相同的包络即可逐窗口/逐采样对拍。
//
// 用法:
//   ref-instrument --instrument violin --note 69 --dur 3 --tail 1 --out out/ref-69.wav
// 可选: --vel 0.8 --bow 0.5 --vibrato 0.4 --release 0.5 --seed 12345
//       --note2 71 --note2-at 1.5   （legato 换音：复用同一 Instrument，旧谱转衰减尾巴）

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

#[derive(Copy, Clone)]
struct Trans {
    start: i64,
    end: i64,
    from: f32,
    to: f32,
}

impl Trans {
    fn value(&self, step: i64) -> f32 {
        if step <= self.start {
            return self.from;
        }
        if step >= self.end {
            return self.to;
        }
        let fraction = (step - self.start) as f32 / (self.end - self.start) as f32;
        let w2 = 0.5 - 0.5 * (fraction * PI).cos();
        (1.0 - w2) * self.from + w2 * self.to
    }
}

fn note_freq(note: i32) -> f32 {
    440.0 * 2.0f32.powf((note - 69) as f32 / 12.0)
}

fn main() {
    let args = Args::parse();
    let itype = parse_instrument(&args.string("instrument", "violin"));
    let note = args.i32("note", 69);
    let note2 = args.i32("note2", note);
    let has_note2 = args.raw("note2").is_some();
    let note2_at = args.f32("note2-at", 1.5).max(0.0);
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
    let note2_step = (note2_at * sr) as i64;

    let attack_time = 1000 + (30000.0 * (1.0 - vel)) as i64;
    let bow_transition = 10000i64;
    let release_time = 1000 + (10000.0 * (1.0 - release_rate)) as i64;

    let mut inst = Instrument::new(itype, 0);
    inst.set_frequency(note_freq(note));
    inst.set_volume(0.0);
    inst.set_bow_position(0.0);
    inst.set_vibrato_amplitude(0.0);
    inst.note_on(note, art);

    let mut planner = RealFftPlanner::<f32>::new();
    let mut mono = Vec::with_capacity(frames);

    let mut step: i64 = 0;
    let mut envelope = 0.0f32;
    let mut current_note = note;
    let mut env_trans = Trans { start: 0, end: attack_time, from: 0.0, to: 1.0 };
    let mut bow_trans = Trans { start: 0, end: bow_transition, from: -bow, to: 0.0 };
    let mut released = false;
    let mut sent_note2 = false;

    for _ in 0..frames {
        // Director: if step%100 == 0 { process_messages() }
        if step % 100 == 0 {
            // legato 换音（不松前一个音）
            if has_note2 && !sent_note2 && step >= note2_step {
                let from = 0.5 * envelope;
                inst.set_frequency(note_freq(note2));
                inst.note_on(note2, art);
                env_trans = Trans { start: step, end: step + attack_time, from, to: 1.0 };
                current_note = note2;
                sent_note2 = true;
                // update_transitions 立即执行一次
                envelope = env_trans.value(step);
                inst.set_volume(envelope);
                inst.set_vibrato_amplitude(0.01 * vibrato * envelope);
            }
            if !released && step >= note_off_step {
                env_trans = Trans { start: step, end: step + release_time, from: envelope, to: 0.0 };
                released = true;
                envelope = env_trans.value(step);
                inst.set_volume(envelope);
                inst.set_vibrato_amplitude(0.01 * vibrato * envelope);
            }
        }
        let _ = current_note;

        // Director: step += 1; 然后 Division: if step%100 == 0 { update_transitions() }
        step += 1;
        if step % 100 == 0 {
            envelope = env_trans.value(step);
            let bow_shift = bow_trans.value(step);
            inst.set_volume(envelope);
            inst.set_bow_position((bow + bow_shift).max(0.0));
            inst.set_vibrato_amplitude(0.01 * vibrato * envelope);
        }

        mono.push(inst.generate(&mut planner));
    }

    if let Err(e) = write_wav_f32(&out, SAMPLE_RATE as u32, &mono, &mono) {
        eprintln!("写出失败 {out}: {e}");
        std::process::exit(1);
    }
    let (peak, rms) = peak_rms(&mono, &mono);
    println!(
        "ref-instrument {} note={} note2={}{} dur={:.2}s tail={:.2}s sr={} vel={:.2} art=arco bow={:.2} vib={:.2}",
        itype_name(itype),
        note,
        note2,
        if has_note2 { format!("@{:.2}s", note2_at) } else { "".to_string() },
        dur,
        tail,
        SAMPLE_RATE,
        vel,
        bow,
        vibrato
    );
    println!(
        "  frames={} peak={:.4} rms={:.5} ({:.2} dBFS)  attack={} release={}",
        frames,
        peak,
        rms,
        20.0 * rms.max(1e-12).log10(),
        attack_time,
        release_time
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
