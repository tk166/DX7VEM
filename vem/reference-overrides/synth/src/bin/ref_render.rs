// 完整 Director 链路（包络 + 弓噪 + 琴体 IR + 立体声）—— M2/M3/M4 的真值。
//
// 用法:
//   ref-render --instrument violin --note 69 --dur 3 --tail 1 --out out/ref-full-69.wav
// 可选: --vel 0.8 --art arco --bow 0.5 --vibrato 0.4 --bow-noise 0.5 --volume 1.0 --seed 12345

mod common;

use common::{peak_rms, set_seed, write_wav_f32, Args};
use std::sync::mpsc;
use synth::director::{Director, Message};
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

fn main() {
    let args = Args::parse();
    let itype = parse_instrument(&args.string("instrument", "violin"));
    let note = args.i32("note", 69);
    let dur = args.f32("dur", 3.0).max(0.0);
    let tail = args.f32("tail", 1.0).max(0.0);
    let vel = args.f32("vel", 0.8).clamp(0.0, 1.0);
    let bow = args.f32("bow", 0.5).clamp(0.0, 1.0);
    let vibrato = args.f32("vibrato", 0.4).max(0.0);
    let bow_noise = args.f32("bow-noise", 0.5).clamp(0.0, 1.0);
    let volume = args.f32("volume", 1.0).clamp(0.0, 1.0);
    let art = parse_articulation(&args.string("art", "arco"));
    let out = args.string("out", "out/ref-render.wav");
    let sr = SAMPLE_RATE as f32;

    let frames = ((dur + tail) * sr) as usize;
    let note_off_step = (dur * sr) as i64;

    set_seed(&args);

    let (tx, rx) = mpsc::channel();
    let mut director = Director::new(itype, 1, rx);
    tx.send(Message::SetArticulation { articulation: art }).unwrap();
    tx.send(Message::SetVolume { volume }).unwrap();
    tx.send(Message::SetVibrato { vibrato }).unwrap();
    tx.send(Message::SetBowPosition { bow_position: bow }).unwrap();
    tx.send(Message::SetBowNoise { bow_noise }).unwrap();
    tx.send(Message::NoteOn { note_index: note, velocity: vel }).unwrap();

    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut sent_off = false;
    for i in 0..frames {
        if !sent_off && i as i64 >= note_off_step && i % 100 == 0 {
            tx.send(Message::NoteOff { note_index: note }).unwrap();
            sent_off = true;
        }
        let (l, r) = director.generate();
        left.push(l);
        right.push(r);
    }

    if let Err(e) = write_wav_f32(&out, SAMPLE_RATE as u32, &left, &right) {
        eprintln!("写出失败 {out}: {e}");
        std::process::exit(1);
    }
    let (peak, rms) = peak_rms(&left, &right);
    println!(
        "ref-render {} note={} dur={:.2}s tail={:.2}s sr={} vel={:.2} art={} bow={:.2} vib={:.2} bow_noise={:.2} volume={:.2}",
        itype_name(itype),
        note,
        dur,
        tail,
        SAMPLE_RATE,
        vel,
        art_name(art),
        bow,
        vibrato,
        bow_noise,
        volume
    );
    println!(
        "  frames={} peak={:.4} rms={:.5} ({:.2} dBFS)  (含琴体 IR + 弓噪)",
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

fn art_name(a: Articulation) -> &'static str {
    match a {
        Articulation::Arco => "arco",
        Articulation::Marcato => "marcato",
        Articulation::Glissando => "glissando",
        Articulation::Spiccato => "spiccato",
        Articulation::Pizzicato => "pizzicato",
        Articulation::ColLegno => "collegno",
        Articulation::Tremolo => "tremolo",
    }
}
