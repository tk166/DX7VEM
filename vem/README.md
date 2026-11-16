# VEM 弦乐引擎

Rust 实现的**谱合成弦乐**引擎（谱激励 + 谐波阻尼 + 周期重合成），
编译成 standalone wasm 给 AudioWorklet 用，原生侧有渲染 CLI 做量化对拍。

- 开发计划：`docs/开发计划-VEM弦乐引擎.md`
- 踩坑记录：`docs/VEM踩坑记录.md`
- 上游参考：<https://github.com/peastman/ViolaExMachina>（LGPL-2.1，**只在开发期对拍，不参与交付**）

## 目录

```
vem/
  Cargo.toml                 # lib(cdylib+rlib) + bin(render)
  src/
    lib.rs                   # C ABI（薄导出层，与 msfa_mixer_* 同构）
    engine.rs                # voice 池 / 控制率包络 / 参数平滑 / 混音 / 总线
    instrument.rs            # 单声部乐器：谱激励 + 谐波阻尼 + 周期重合成 + 衰减尾巴
    spectrum.rs              # 每件乐器的拟合系数 + 音色可调项
    fft.rs                   # 自写 IFFT（混合基 2/3/5 + Bluestein 兜底 + 计划缓存）
    filter.rs                # 一阶低通
    envelope.rs              # 包络参数（48k 采样基准，可换成 ms 可调）
    articulations.rs         # 弓法枚举（M2 加状态机）
    smooth.rs                # 一阶平滑 / 软限幅
    random.rs                # 确定性 LCG（与参考同序，便于逐采样对拍）
    bin/render.rs            # 原生渲染 CLI（float32 WAV）
  tests/                     # 集成测试（C ABI）
  reference/                 # 【gitignore】上游固定 commit 副本（fetch-reference.ps1 生成）
  reference-overrides/       # 只新增两个 ref bin + 固定种子，不改上游算法
  wasm/vem.wasm              # 构建产物（入库）
  build-wasm.ps1
  fetch-reference.ps1
```

## 常用命令

```powershell
# 单元/集成测试
cargo test --manifest-path vem/Cargo.toml

# 原生渲染（float32 WAV）
cargo run --release --manifest-path vem/Cargo.toml --bin render -- `
  --instrument violin --note 69 --dur 3 --tail 1 --out out/vem-69.wav

# 与参考真值对拍（--raw 关掉总线缩放，逐采样可比）
cargo run --release --manifest-path vem/Cargo.toml --bin render -- `
  --instrument violin --note 69 --dur 3 --tail 1 --out out/vem-69.wav --seed 12345 --raw

# M1 全套验收（13 个用例，FAIL 时退出码 1）
powershell -ExecutionPolicy Bypass -File scripts/vem-m1-accept.ps1

# 参考实现（首次会 clone + 编译）
powershell -ExecutionPolicy Bypass -File vem/fetch-reference.ps1
.\vem\reference\target\release\ref-instrument.exe `
  --instrument violin --note 69 --dur 3 --tail 1 --out out/ref-69.wav --seed 12345

# 验收指标（F0 / 频谱余弦 / 包络相关 / 电平差）
node scripts/vem-metrics.mjs out/ref-69.wav out/vem-69.wav --from 0.5 --to 3.5 --note 69 --gate

# 整段窗口对拍 / 分频段
node scripts/compare-windows.mjs out/ref-69.wav out/vem-69.wav 1 1
node scripts/band-energy.mjs out/ref-69.wav out/vem-69.wav 0.5 0.5 3

# wasm 构建 + standalone 体检
powershell -ExecutionPolicy Bypass -File vem/build-wasm.ps1
node scripts/check-standalone.mjs vem/wasm/vem.wasm
```

## C ABI（M5 接 worklet 时用）

```c
void* vem_new(int sample_rate, int instrument_type);  // 0=violin 1=viola 2=cello 3=bass
void  vem_free(void*);
void  vem_reset(void*);
void  vem_set_patch(void*, const uint8_t* blob, size_t len);   // M3 起生效
void  vem_set_seed(void*, uint32_t);
void  vem_set_gain(void*, float);
void  vem_set_master(void*, float);
void  vem_set_volume(void*, float);                  // 0..1
void  vem_set_pitch_bend(void*, float semitones);
void  vem_set_vibrato(void*, float depth, float rate);
void  vem_set_envelope(void*, float attack_ms, float attack_velocity_ms,
                       float release_ms, float release_slow_ms,
                       float bow_settle_ms, float vibrato_onset_ms, float vibrato_onset_ramp_ms);
void  vem_set_release_rate(void*, float);            // 0..1
void  vem_set_bow(void*, float position, float pressure);
void  vem_set_articulation(void*, int art);   // 0 arco 1 marcato 2 glissando 3 spiccato 4 pizzicato 5 collegno 6 tremolo
void  vem_set_harmonics(void*, int on);
void  vem_set_oversample(void*, int mode);    // 0 自动（小提琴 2×）1 强制开 2 强制关
void  vem_set_timbre(void*, float damping_scale, float tilt, float bow_focus,
                     float cutoff_scale, float phase_scale);   // 默认全中性
void  vem_set_polyphonic(void*, int on);      // 默认 0 = 参考实现的单声部
void  vem_set_body(void*, float mix, float size);
void  vem_set_reverb(void*, float mix, float size, float damp, float width);
void  vem_set_comp(void*, int on, float thr, float ratio, float atk, float rel, float makeup);
void  vem_set_bow_noise(void*, int mode, float amount);        // 0 关 1 采样 2 合成
void  vem_note_on(void*, int note, float velocity);
void  vem_note_off(void*, int note);
void  vem_note_off_all(void*);
void  vem_render(void*, float* outL, float* outR, int frames);
uint8_t* vem_alloc(size_t bytes);
void  vem_free_mem(uint8_t* ptr, size_t bytes);
```

## 状态

| 里程碑 | 状态 | 说明 |
|---|---|---|
| M0 骨架 + 真值 + 对拍管线 | ✅ | wasm 33 KB / imports 0；参考渲染可复现（`--seed`） |
| M1 单音还原（谱合成 + 自写 IFFT） | ✅ | 13 用例全 PASS：余弦 1.0000、电平 0.00 dB、包络 1.0000、逐采样 SNR ≈136 dB；wasm 64 KB |
| M2 演奏性 | ⬜ | 7 种弓法 / 换弓 / 揉弦渐入（参数已就位） |
| M3 琴体 + 总线 | ⬜ | 琴体 IR / 压缩 / 混响 / 弓噪 |
| M4 单轨替换 | ⬜ | CH3 旋律 + 衰减尾巴 |
| M5 Web 集成 | ⬜ | AudioWorklet 适配层 + UI |
| M6 收尾 | ⬜ | 体积 / CPU / 许可 |
