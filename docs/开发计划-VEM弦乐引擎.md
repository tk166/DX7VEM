# VEM 弦乐引擎 · 开发计划（Rust / WASM）

> 目标：把 Viola Ex Machina（下称 VEM）的**谱合成弦乐**思路做成我们自己的引擎，
> **先保证“还原”**（音色/演奏法可信），体积优化放到后期。
> 语言改为 **Rust**，，与现有 DX7(C++, 文件在 ../261115-daw) 引擎并存。
>
> 参考实现：<https://github.com/peastman/ViolaExMachina>（LGPL-2.1，Rust，synth crate 1907 行）

---

## 1. 目标与非目标

### 1.1 目标（按优先级）

1. **还原度**：与 VEM 参考渲染相比，单音频谱余弦 ≥ 0.99、基频误差 < 1 cent、包络相关 > 0.98。
2. **演奏性**：`Arco / Marcato / Glissando / Spiccato / Pizzicato / ColLegno / Tremolo`、
   弓位（sul tasto ↔ sul ponticello）、揉弦、换弓瞬态。
3. **可交付**：Rust → wasm（standalone，无 JS import），接进现有 AudioWorklet 播放器。
4. **可验证**：每一步都有量化指标，不靠“听起来还行”。
5. 体积：v1 目标 **wasm ≤ 120 KB**、数据 ≤ 80 KB（不追求 30 KB，留到 v2）。

### 1.2 非目标（v1 不做）

- 极限体积（`no_std`、自写混合基 FFT、8-bit 采样压缩）→ v2
- 真实弓弦摩擦/波导物理建模（VEM 本身也不是）
- MPE、多麦克风、多琴体切换、实时调律
- 替换现有 DX7 全部声部（v1 只替换 CH3 旋律，见 §8）

---

## 2. 先搞清楚：VEM 的声音是怎么来的

实测（`synth/src/instrument.rs`）：

```
每个音：
  period = sr / freq
  spectrum[0..period/2] ← 经验弓激励包络（每乐器 4 个拟合系数），随机相位
  spectrum[i] *= 谐波阻尼（高次谐波衰减更快）
  IFFT(spectrum) → 正好一个周期的波形 → 循环播放
  换音/揉弦/滑音 → 重算 period，重新合成
  旧音的尾巴 → DecayingNote（独立频谱，各自 IFFT）
外加：
  琴体 = 录好的 IR 做卷积混响（violin.flac 等，0.37–0.68 s）
  弓噪 = 5.21 s 录音（300 KB）经谐振滤波器，随弓位/弓速调制
```

**关键结论：它不是物理建模，是“每周期频谱重合成”。** 代码短正是因为这个模型本身简单。

数据实测：

| 数据 | 规格 | 大小 |
|---|---|---|
| `bow_noise.flac` | 48 k / 16 bit / 5.21 s | 300 KB（解码 488 KB） |
| `violin/viola/cello/bass.flac` | 0.37–0.68 s IR | 各 13–17 KB |

---

## 3. 我们要“更好”的地方（相对 VEM 的具体改进）

| VEM 现状 | 我们的做法 | 收益 |
|---|---|---|
| 每周期调 `plan_fft_inverse(size)`（哈希查找 + Arc 克隆） | **按 size 缓存 FFT 计划**，音频路径零查找 | 稳定、可预测 |
| 音频路径里 `Vec::resize` / `decaying_notes.push+retain` | **预分配对象池**（voice 槽位固定） | 无分配、无抖动 |
| 每谐波每周期 3–4 次 `exp/ln` | **按谐波占比建表**（256 项查表 + 线性插值） | CPU 降一个量级 |
| 依赖 `getrandom` | **自实现 xorshift**，种子由参数传入 | 确定性渲染、可回归测试 |
| `SAMPLE_RATE` 写死 48 k，小提琴内部 96 k | **采样率全参数化**，2× 过采样做成开关 | 44.1 k 设备正确、CPU 可控 |
| 小提琴 2× 用 `0.5*(gen+gen)` 简单平均 | 可选 **半带/多相抽取**（或先 1× 跑，量化对比） | 抗混叠更干净 |
| 无参数平滑、无总线保护 | 复用我们已验证的**参数平滑 + 软限幅 + 压缩/混响** | 无 zipper、不破 0 dBFS |
| 无对拍工具 | 复用我们的**窗口平均频谱 / RMS 比 / 单轨对拍脚本** | 可量化验收 |

---

## 4. 新目录与模块划分

```
vem/                                # 新目录（Rust workspace）
  Cargo.toml                        # [lib] crate-type = ["cdylib"]（wasm）+ rlib（测试）
  src/
    lib.rs                          # C ABI 导出（与现有 msfa_mixer_* 同构）
    engine.rs                       # 引擎总装：voice 池 / 事件 / 混音 / FX
    instrument.rs                   # 谱合成核心（移植自 VEM，自写）
    spectrum.rs                     # 激励包络、阻尼、相位、查表
    fft.rs                          # 实数 IFFT + 计划缓存
    articulations.rs                # 弓法状态机（Arco/Spiccato/…）
    body.rs                         # 琴体：IR 卷积（v1）→ 模态 EQ（v2）
    noise.rs                        # 弓噪：采样版 + 合成版（可切换）
    reverb.rs                       # Freeverb（移植我们 C++ 版）
    comp.rs                         # 压缩器（移植我们已验证的实现）
    smooth.rs                       # 一阶平滑 / 软限幅
  src/data/                         # 系数、IR、噪声（先自采/合成，见 §9 许可）
  src/bin/render.rs                 # 原生渲染 CLI（做对拍的真值输出）
  tests/                            # 单元测试（FFT/阻尼/包络/CPU）
  build-wasm.sh                     # wasm32-unknown-unknown + wasm-opt
  reference/                        # 【只做开发对照】VEM synth crate（submodule/副本）
```

**与现有仓库的分工**

| 目录 | 作用 |
|---|---|
| `vem/` | 新引擎（Rust，独立构建，不被 `web/` 直接引用源码） |
| `web/` | 现有播放器（DX7）。v1 阶段新增 `engine` 适配层，可切引擎 |
| `msfa/` | 旧引擎（保留，做 A/B 与兜底） |
| `scripts/` | 复用：数据导出、对拍、体积/CPU 检查 |
| `docs/` | 本计划 + 新引擎踩坑记录 |

---

## 5. C ABI 设计（照抄我们 DX7 的经验）

与 `msfa_mixer_*` 保持同构，这样 `web/worklet.js` 只需换一套函数名（或加适配层）：

```c
// 生命周期
void* vem_new(int sample_rate, int instrument_type);
void  vem_free(void* e);
void  vem_reset(void* e);                       // seek 用

// 音色/演奏参数
void  vem_set_patch(void* e, const uint8_t* blob);   // 系数/IR/默认值（自定义紧凑格式）
void  vem_set_gain(void* e, float g);
void  vem_set_vibrato(void* e, float depth, float rate);
void  vem_set_bow(void* e, float position, float pressure);
void  vem_set_articulation(void* e, int art);
void  vem_set_body(void* e, float mix, float size);
void  vem_set_reverb(void* e, float mix, float size, float damp, float width);
void  vem_set_comp(void* e, int on, float thr, float ratio, float atk, float rel, float makeup);
void  vem_set_bow_noise(void* e, int mode, float amount);  // 0=关 1=采样 2=合成

// 事件与渲染
void  vem_note_on(void* e, int note, float vel);
void  vem_note_off(void* e, int note);
void  vem_render(void* e, float* outL, float* outR, int frames);  // frames 为 64 的倍数
```

**导出约定（踩过的坑）**

- `wasm32-unknown-unknown` 的导出名**不带前导下划线**；我们现有 loader 已做 `_` 别名兼容。
- `-C panic=abort` + 不格式化 panic 信息（否则拖进大量 `core::fmt`）。
- 不引入 `getrandom`/`std::time`/`std::fs`，否则 standalone wasm 会出现 import。
- 用 `wasm-opt -Oz` 收体积，但**先保证正确性**。

---

## 6. 复用当前仓库的哪些经验

| 现有资产 | 复用到新引擎的方式 |
|---|---|
| `msfa/msfa_wrapper.cpp` 的**混音器结构**（通道增益/总线压缩/混响/软限幅） | 在 Rust 里等价重写（结构、参数范围、平滑系数照搬） |
| `msfa/build.sh` 的**双产物**思路（Node 胶水版 + standalone） | Rust 只需一个 wasm；Node 用同一 wasm + 40 行 loader |
| `web/worklet.js` 的**调度**（64 采样块、事件在块边界、seek 不追算、2 ms 淡入） | 抽成 `createEngine()` 适配层，DX7/VEM 共用 |
| `scripts/check-standalone.mjs` | 通用化：检查 imports 为空、导出齐全 |
| `scripts/test-worklet.mjs` | 加 `--engine vem` 参数，同一套假 AudioWorklet 环境 |
| `scripts/compare-windows.mjs` / `parity-track.mjs` / `band-energy.mjs` | 直接用于 VEM 的 A/B 与频谱验收 |
| `scripts/extract-reaper-notes.mjs` → `data/notes.bin` | 事件格式不变（8 B/事件），新引擎照读 |
| `docs/MCP踩坑记录.md` 的**踩坑文化** | 新建 `docs/VEM踩坑记录.md`，遇到就写 |
| git 流程（`.gitignore` / `.gitattributes` / 一个功能一提交） | 沿用；wasm 产物入库 |

---

## 7. 里程碑（先还原，后体积）

> 时间按“工作日”粗估，仅用于排期，不作为承诺。

### M0 · 骨架与真值（0.5 天）

- Rust workspace、`cargo test` 跑通、`src/bin/render.rs` 能出 WAV。
- **把 VEM 的 `synth` crate 拉进 `reference/`，加一个 `[[bin]]` 直接渲染单音**（不装 nih-plug），
  作为**唯一真值**：`reference-ref.wav`。
- 产出：`vem/reference` 渲染脚本 + 对比脚本骨架。

验收：`cargo run --bin render -- --note 69 --dur 3` 出 WAV；参考渲染同参数出 WAV。

### M1 · 单音还原（2–3 天）

- 移植 `instrument.rs` 的谱合成：激励包络（4 组系数）、谐波阻尼、随机相位、周期 IFFT、尾巴。
- 自写实数 IFFT（任意长度：先做混合基 2/3/5，够用即可）+ 计划缓存。
- 查表化 `exp/ln`。

验收（对照 `reference-ref.wav`）：
- 频谱余弦 **≥ 0.99**（线性谱 + 1/3 倍频程各算一次）
- 基频误差 **< 1 cent**
- 包络（20 ms 窗）相关 **> 0.98**，电平差 **±1 dB**
- 四件乐器（violin/viola/cello/bass）都过

### M2 · 演奏性（2 天）

- vibrato（含频率/幅度漂移）、弓位、弓压、release。
- 7 种 articulation 的状态机；换弓瞬态。
- 参考对照：同一 articulation 下的谱质心/起音时间趋势一致。

验收：7 种弓法 × 2 个音高，与参考的“谱质心排序”一致；颤音速率误差 < 2%。

### M3 · 琴体 + 总线（1–2 天）

- v1：**短 IR 卷积**（保真优先）；IR 用我们自己的录音或合成 IR（见 §9）。
- 总线：压缩器 + 混响（照搬 DX7 版参数与曲线）+ 软限幅。
- 对比：`RT60`、`band-energy` 三段频响差 ≤ 2 dB。

### M4 · 接数据与单轨替换（1–2 天）

- 复用 `notes.bin` + `tracks.json`；把 **CH3 旋律**换成 VEM violin，其余 9 轨仍走 DX7。
- 混音器层面：两个引擎输出相加（各自有增益），不强行合并成一套。
- 对拍：与现有 `out/p03-reaper.wav` 比整体 RMS/频谱（注：音色本就不同，只看电平与节奏对齐）。

### M5 · Web 集成（2 天）

- Rust wasm 接进 `web/worklet.js`（适配层），新增 UI：弓法/弓位/揉弦/弓噪。
- 目标：48 kHz 下 **单声部 CPU ≤ 3%**（实测，不估算）。

### M6 · 收尾（1 天）

- 体积/CPU 报告；`docs/VEM踩坑记录.md`；许可证与数据来源说明；README。
- 决定 v2 是否做 `no_std` + 自写 FFT + 8-bit 数据。

---

## 8. 与《梁祝》的集成策略

**v1 采用混合编制**（风险最低、听感提升最大）：

```
CH1/2/4/5/6/7/8/9/11 → 现有 DX7（msfa）不变
CH3（旋律）          → VEM violin（独奏）
总线                 → 统一压缩 + 混响 + 软限幅
```

理由：

- VEM 每个 Instrument 是**单声部**（一个音 + 衰减尾巴），复音靠多实例；独奏线刚好。
- 全曲 10 轨都换 VEM 会立刻撞上 CPU 与体积上限，且伴奏本来是合成器音色。
- 混合方案可以先交付、再逐步替换其他弦乐声部。

---

## 9. 数据与许可证（必须先定）

1. **VEM 的 FLAC 是作者录制的素材**，仓库整体 LGPL-2.1。
   - **直接用**：可以，但整份交付要按 LGPL 开源（含源码、允许替换/重链接）。
   - **推荐**：v1 用**我们自己录/合成**的 IR 与弓噪，避免素材许可问题；`reference/` 仅在开发期作为对照，不进交付。
2. **代码移植 vs 独立实现**：
   - 逐行翻译 → 衍生作品，需 LGPL。
   - 按“公开的算法思路”（频谱激励 + 谐波阻尼 + 周期重合成）独立实现 → 不构成衍生。
   - 计划：**独立实现为主，参考文件只在 `reference/` 做行为对照**；`vem/` 顶部写明实现来源与许可。
3. 我们的 DX7 引擎是 Apache-2.0，两者可以并存分发。

---

## 10. 验收工具（全部复用现有脚本）

```powershell
# 参考渲染（VEM synth crate，开发期真值）
cargo run --release --manifest-path vem/reference/Cargo.toml --bin ref-render -- --note 69 --dur 3 --out out/ref-69.wav

# 我们的引擎
cargo run --release --manifest-path vem/Cargo.toml --bin render -- --instrument violin --note 69 --dur 3 --out out/vem-69.wav

# 频谱/电平
node scripts/compare-windows.mjs out/ref-69.wav out/vem-69.wav 5 5
node scripts/band-energy.mjs out/ref-69.wav out/vem-69.wav 0 0 3
node scripts/peak-report.mjs out/ref-69.wav out/vem-69.wav

# wasm 体检 + 浏览器前测试
bash vem/build-wasm.sh
node scripts/check-standalone.mjs vem/wasm/vem.wasm
node scripts/test-worklet.mjs --engine vem
```

**验收门槛（写进 CI 也可以）**

| 指标 | M1 门槛 | 说明 |
|---|---|---|
| 线性谱余弦 | ≥ 0.99 | 单音稳态 1 s 窗口 |
| 1/3 倍频程余弦 | ≥ 0.98 | 容忍谐波位置微小偏差 |
| 基频误差 | < 1 cent | 自相关/谱峰 |
| 包络相关 | > 0.98 | 20 ms RMS 包络 |
| 电平差 | ±1 dB | 同音同力度 |
| CPU/声部 | ≤ 3% @48 k | 实测 |
| wasm | ≤ 120 KB | v1 |

---

## 11. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| 任意长度实数 IFFT 写错 | 全盘错 | M1 先做 2/3/5 混合基 + 与 `rustfft` 结果逐点对比 |
| Rust wasm 体积超预期 | 交付变重 | `panic=abort`、无 std I/O、`wasm-opt -Oz`；超了就上 `no_std` |
| 音频路径分配/抖动 | 咔哒/掉音 | 对象池 + 预分配；用我们的 `maxjump`/`detect-clicks` 检查 |
| 琴体 IR 卷积 CPU | 实时不够 | 分块卷积（partitioned）；不行就降级成模态 EQ |
| 参考实现改版 | 对拍失效 | `reference/` 固定 commit，记录版本 |
| 素材许可 | 不能分发 | 自采/合成数据（§9） |
| “像合成器不像琴” | 白做 | M1 就做 A/B；不达标优先修激励包络与阻尼，而不是加效果器 |

---

## 12. 第一步（本次就做）

1. 建 `vem/` 骨架 + `cargo test` 通过。
2. 加 `vem/reference/`（VEM synth crate 固定 commit）+ `ref-render` 二进制，产出第一份真值 WAV。
3. 写 `vem/src/bin/render.rs`（先用最简单的正弦占位），把 `compare-windows.mjs` 跑通。
4. 提交：`feat(vem): 骨架 + 参考渲染 + 对拍管线`。

完成后再进 M1（谱合成移植）。
