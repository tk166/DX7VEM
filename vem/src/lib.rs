//! # VEM 弦乐引擎
//!
//! 谱激励 + 谐波阻尼 + 周期重合成（**独立实现**；算法思路见
//! `docs/开发计划-VEM弦乐引擎.md`，行为对照只使用 `vem/reference/` 下的上游代码）。
//!
//! C ABI 与现有 `msfa_mixer_*` 同构，便于 `web/worklet.js` 加一层适配器即可切换引擎。
//!
//! 导出约定（踩过的坑，见 docs/MCP踩坑记录.md）：
//! - `wasm32-unknown-unknown` 的导出名**不带前导下划线**；
//! - `panic=abort`，不引入 `getrandom`/`std::time`/`std::fs`，保证 standalone（imports 为空）；
//! - 缓冲由 JS 侧调用 `vem_alloc` 分配一次，指针复用。

pub mod articulations;
pub mod body;
pub mod comp;
pub mod data;
pub mod engine;
pub mod envelope;
pub mod fft;
pub mod filter;
pub mod instrument;
pub mod noise;
pub mod random;
pub mod reverb;
pub mod smooth;
pub mod spectrum;

pub use articulations::Articulation;
pub use engine::{Engine, BLOCK, DEFAULT_SEED, DIRECTOR_SCALE, MAX_VOICES};
pub use envelope::EnvelopeParams;
pub use instrument::InstrumentType;
pub use spectrum::TimbreParams;

use core::ffi::c_void;

#[inline]
fn engine_mut<'a>(p: *mut c_void) -> &'a mut Engine {
    debug_assert!(!p.is_null());
    unsafe { &mut *(p as *mut Engine) }
}

// ---------------------------------------------------------------- 生命周期

#[no_mangle]
pub extern "C" fn vem_new(sample_rate: i32, instrument_type: i32) -> *mut c_void {
    let sr = if sample_rate > 0 { sample_rate as f32 } else { 48000.0 };
    let e = Box::new(Engine::new(sr, InstrumentType::from_i32(instrument_type)));
    Box::into_raw(e) as *mut c_void
}

#[no_mangle]
pub extern "C" fn vem_free(p: *mut c_void) {
    if !p.is_null() {
        unsafe { drop(Box::from_raw(p as *mut Engine)) };
    }
}

/// seek 用：清空所有声音与内部状态（不重新分配）。
#[no_mangle]
pub extern "C" fn vem_reset(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    engine_mut(p).reset();
}

/// 版本号：major*10000 + minor*100 + patch。
#[no_mangle]
pub extern "C" fn vem_version() -> i32 {
    0 * 10000 + 2 * 100 + 0
}

// ---------------------------------------------------------------- 参数

/// 音色/数据 blob（`VEMD` 容器：琴体 IR + 弓噪，见 `data.rs`）。
#[no_mangle]
pub extern "C" fn vem_set_patch(p: *mut c_void, blob: *const u8, len: usize) {
    if p.is_null() || blob.is_null() || len == 0 {
        return;
    }
    let bytes = unsafe { core::slice::from_raw_parts(blob, len) };
    engine_mut(p).set_data(bytes);
}

#[no_mangle]
pub extern "C" fn vem_set_seed(p: *mut c_void, seed: u32) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.seed = seed;
    e.reset();
}

#[no_mangle]
pub extern "C" fn vem_set_gain(p: *mut c_void, gain: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).set_gain(gain);
}

#[no_mangle]
pub extern "C" fn vem_set_master(p: *mut c_void, gain: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).master = gain.max(0.0);
}

#[no_mangle]
pub extern "C" fn vem_set_volume(p: *mut c_void, volume: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).volume = volume.clamp(0.0, 1.0);
}

#[no_mangle]
pub extern "C" fn vem_set_pitch_bend(p: *mut c_void, semitones: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).bend = (2.0f32).powf(semitones / 12.0);
}

#[no_mangle]
pub extern "C" fn vem_set_vibrato(p: *mut c_void, depth: f32, rate: f32) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.vibrato_depth = depth.max(0.0);
    e.vibrato_rate = rate.max(0.0);
}

/// 包络全参数（毫秒）：
/// attack_ms 基础起音 / attack_velocity_ms 力度 0 时额外量 /
/// release_ms 基础释放 / release_slow_ms release_rate=0 时额外量 /
/// bow_settle_ms 起音弓位归位时间 / vibrato_onset_ms 揉弦渐入延迟 / vibrato_onset_ramp_ms 渐入过渡
#[no_mangle]
pub extern "C" fn vem_set_envelope(
    p: *mut c_void,
    attack_ms: f32,
    attack_velocity_ms: f32,
    release_ms: f32,
    release_slow_ms: f32,
    bow_settle_ms: f32,
    vibrato_onset_ms: f32,
    vibrato_onset_ramp_ms: f32,
) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.envelope.set_attack_ms(attack_ms, attack_velocity_ms);
    e.envelope.set_release_ms(release_ms, release_slow_ms);
    e.envelope.set_bow_settle_ms(bow_settle_ms);
    e.envelope.vibrato_onset_ms = vibrato_onset_ms.max(0.0);
    e.envelope.vibrato_onset_ramp_ms = vibrato_onset_ramp_ms.max(0.0);
}

#[no_mangle]
pub extern "C" fn vem_set_release_rate(p: *mut c_void, rate: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).release_rate = rate.clamp(0.0, 1.0);
}

/// legato 换音行为：start = 接续比例（参考实现 0.5）、ramp_ms = 回到 1 的时间（0=用起音）、
/// accent = 小上冲、boost = 换音后的激励补偿（0=关）、boost_ms = 补偿衰减时间
#[no_mangle]
pub extern "C" fn vem_set_legato(
    p: *mut c_void,
    start: f32,
    ramp_ms: f32,
    accent: f32,
    boost: f32,
    boost_ms: f32,
) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.envelope.legato_start = start.clamp(0.0, 1.0);
    e.envelope.legato_ramp_ms = ramp_ms.max(0.0);
    e.envelope.legato_accent = accent.clamp(0.0, 0.2);
    e.envelope.legato_boost = boost.clamp(0.0, 8.0);
    e.envelope.legato_boost_ms = boost_ms.clamp(1.0, 200.0);
}

#[no_mangle]
pub extern "C" fn vem_set_bow(p: *mut c_void, position: f32, pressure: f32) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.bow_position = position.clamp(0.0, 1.0);
    e.bow_pressure = pressure.clamp(0.0, 1.0);
}

#[no_mangle]
pub extern "C" fn vem_set_polyphonic(p: *mut c_void, on: i32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).polyphonic = on != 0;
}

#[no_mangle]
pub extern "C" fn vem_set_articulation(p: *mut c_void, art: i32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).articulation = Articulation::from_i32(art);
}

#[no_mangle]
pub extern "C" fn vem_set_harmonics(p: *mut c_void, on: i32) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.harmonics = on != 0;
}

/// 过采样：0=自动（小提琴 2×）1=强制开 2=强制关
#[no_mangle]
pub extern "C" fn vem_set_oversample(p: *mut c_void, mode: i32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).set_oversample(mode);
}

/// 音色微调（默认全部中性 = 参考实现）：阻尼倍率 / 谐波倾斜 / 弓位强度 / 截止倍率 / 相位倍率
#[no_mangle]
pub extern "C" fn vem_set_timbre(
    p: *mut c_void,
    damping_scale: f32,
    tilt: f32,
    bow_focus: f32,
    cutoff_scale: f32,
    phase_scale: f32,
) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.timbre = TimbreParams {
        damping_scale: damping_scale.clamp(0.1, 4.0),
        tilt,
        bow_focus: bow_focus.max(0.0),
        cutoff_scale: cutoff_scale.clamp(0.1, 4.0),
        phase_scale,
    };
}

#[no_mangle]
pub extern "C" fn vem_set_body(p: *mut c_void, mix: f32, size: f32) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.body_mix = mix.clamp(0.0, 1.0);
    e.body_size = size.clamp(0.0, 1.0);
}

#[no_mangle]
pub extern "C" fn vem_set_reverb(p: *mut c_void, mix: f32, size: f32, damp: f32, width: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).set_reverb(mix, size, damp, width);
}

#[no_mangle]
pub extern "C" fn vem_set_comp(
    p: *mut c_void,
    on: i32,
    thr: f32,
    ratio: f32,
    atk: f32,
    rel: f32,
    makeup: f32,
) {
    if p.is_null() {
        return;
    }
    engine_mut(p).set_comp(on != 0, thr, ratio, atk, rel, makeup);
}

/// 0=关 1=采样 2=合成（M3 实现）。
#[no_mangle]
pub extern "C" fn vem_set_bow_noise(p: *mut c_void, mode: i32, amount: f32) {
    if p.is_null() {
        return;
    }
    let e = engine_mut(p);
    e.bow_noise_mode = mode;
    e.bow_noise_amount = amount.clamp(0.0, 1.0);
}

// ---------------------------------------------------------------- 事件与渲染

#[no_mangle]
pub extern "C" fn vem_note_on(p: *mut c_void, note: i32, velocity: f32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).note_on(note, velocity);
}

#[no_mangle]
pub extern "C" fn vem_note_off(p: *mut c_void, note: i32) {
    if p.is_null() {
        return;
    }
    engine_mut(p).note_off(note);
}

#[no_mangle]
pub extern "C" fn vem_note_off_all(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    engine_mut(p).note_off_all();
}

/// 渲染 frames 帧到 L/R（frames 建议为 64 的倍数）。
#[no_mangle]
pub extern "C" fn vem_render(
    p: *mut c_void,
    out_l: *mut f32,
    out_r: *mut f32,
    frames: i32,
) {
    if p.is_null() || out_l.is_null() || out_r.is_null() || frames <= 0 {
        return;
    }
    let n = frames as usize;
    let l = unsafe { core::slice::from_raw_parts_mut(out_l, n) };
    let r = unsafe { core::slice::from_raw_parts_mut(out_r, n) };
    engine_mut(p).render(l, r);
}

// ---------------------------------------------------------------- 线性内存
// JS 侧分配渲染缓冲（malloc 一次，seek/播放复用）。wasm32 下 usize = u32。

#[no_mangle]
pub extern "C" fn vem_alloc(bytes: usize) -> *mut u8 {
    if bytes == 0 {
        return core::ptr::null_mut();
    }
    let layout = match core::alloc::Layout::from_size_align(bytes, 4) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    unsafe { std::alloc::alloc(layout) }
}

#[no_mangle]
pub extern "C" fn vem_free_mem(p: *mut u8, bytes: usize) {
    if p.is_null() || bytes == 0 {
        return;
    }
    if let Ok(layout) = core::alloc::Layout::from_size_align(bytes, 4) {
        unsafe { std::alloc::dealloc(p, layout) };
    }
}
