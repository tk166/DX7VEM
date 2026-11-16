//! 集成测试：走一遍 C ABI（与 wasm 侧 worklet 调用的路径一致）。

use core::ffi::c_void;
use vem::*;

#[test]
fn abi_lifecycle_render_and_alloc() {
    let e: *mut c_void = vem_new(48000, 0);
    assert!(!e.is_null());
    assert_eq!(vem_version() / 100, 2);

    vem_set_gain(e, 1.0);
    vem_set_master(e, 1.0);
    vem_set_vibrato(e, 0.4, 5.2);
    vem_set_bow(e, 0.5, 0.8);
    vem_set_articulation(e, 0);
    vem_set_body(e, 0.18, 0.6);
    vem_set_reverb(e, 0.0, 0.6, 0.4, 1.0);
    vem_set_comp(e, 0, -18.0, 3.0, 15.0, 200.0, 0.0);
    vem_set_bow_noise(e, 0, 0.5);
    vem_set_seed(e, 12345);

    let frames: usize = 64 * 8;
    let pl = vem_alloc(frames * 4);
    let pr = vem_alloc(frames * 4);
    assert!(!pl.is_null() && !pr.is_null());

    vem_note_on(e, 69, 0.8);
    for _ in 0..40 {
        vem_render(e, pl as *mut f32, pr as *mut f32, 64);
    }
    let l = unsafe { core::slice::from_raw_parts(pl as *const f32, 64) };
    assert!(l.iter().any(|v| v.abs() > 0.001), "ABI 渲染没有声音");
    assert!(l.iter().all(|v| v.is_finite()));

    vem_note_off(e, 69);
    vem_note_off_all(e);
    vem_reset(e);

    vem_free_mem(pl, frames * 4);
    vem_free_mem(pr, frames * 4);
    vem_free(e);
}

#[test]
fn abi_null_pointers_are_safe() {
    let null = core::ptr::null_mut();
    vem_reset(null);
    vem_set_gain(null, 1.0);
    vem_note_on(null, 60, 1.0);
    vem_note_off(null, 60);
    vem_render(null, core::ptr::null_mut(), core::ptr::null_mut(), 64);
    vem_free(null);
    assert!(vem_alloc(0).is_null());
}
