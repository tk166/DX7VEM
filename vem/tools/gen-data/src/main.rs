//! 生成 VEM 数据容器：把参考实现的 FLAC（琴体 IR / 弓噪）转成 `VEMD` blob。
//!
//! **开发期工具**：产物不进仓库、不进 wasm；v1 交付会换成自采/合成数据（计划 §9）。
//!
//! 用法:
//!   cargo run --release --manifest-path vem/tools/gen-data/Cargo.toml -- \
//!     --ref-dir vem/reference/synth/src/data --out-dir out/vem-data

use std::fs;
use std::path::Path;

use vem::data::VemData;

fn decode_flac(path: &Path) -> Vec<f32> {
    let mut reader = claxon::FlacReader::open(path).expect("打开 FLAC 失败");
    let info = reader.streaminfo();
    assert_eq!(info.sample_rate, 48000, "只支持 48 kHz");
    assert_eq!(info.channels, 1, "只支持单声道");
    assert_eq!(info.bits_per_sample, 16, "只支持 16-bit");
    reader
        .samples()
        .map(|s| s.expect("解码失败") as f32 / 32768.0)
        .collect()
}

fn main() {
    let mut ref_dir = String::from("vem/reference/synth/src/data");
    let mut out_dir = String::from("out/vem-data");
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--ref-dir" if i + 1 < argv.len() => {
                ref_dir = argv[i + 1].clone();
                i += 2;
            }
            "--out-dir" if i + 1 < argv.len() => {
                out_dir = argv[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }
    let ref_dir = Path::new(&ref_dir);
    let out_dir = Path::new(&out_dir);
    fs::create_dir_all(out_dir).expect("创建输出目录失败");

    for (index, name) in ["violin", "viola", "cello", "bass"].iter().enumerate() {
        let flac = ref_dir.join(format!("{name}.flac"));
        if !flac.exists() {
            eprintln!("跳过（不存在）: {}", flac.display());
            continue;
        }
        let body = decode_flac(&flac);
        let blob = VemData::write(&body, &[], index as u8);
        let out = out_dir.join(format!("body-{name}.bin"));
        fs::write(&out, &blob).expect("写出失败");
        println!(
            "{}  样本 {}  ({:.2}s)  {:.1} KB",
            out.display(),
            body.len(),
            body.len() as f32 / 48000.0,
            blob.len() as f32 / 1024.0
        );
    }

    let noise_flac = ref_dir.join("bow_noise.flac");
    if noise_flac.exists() {
        let noise = decode_flac(&noise_flac);
        let blob = VemData::write(&[], &noise, 0);
        let out = out_dir.join("noise.bin");
        fs::write(&out, &blob).expect("写出失败");
        println!(
            "{}  样本 {}  ({:.2}s)  {:.1} KB",
            out.display(),
            noise.len(),
            noise.len() as f32 / 48000.0,
            blob.len() as f32 / 1024.0
        );
    }
}
