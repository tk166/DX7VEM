# VEM 弦乐引擎 · 踩坑记录

> 与 `docs/MCP踩坑记录.md` 同一个原则：**遇到就写，写清楚“症状 → 根因 → 修法”**。
> 参考实现固定在上游 commit `5ddfdaae519b062b5be6d9be662f16e388961e30`（v1.4）。

---

## 1. 参考实现默认不可复现（getrandom）

**症状**：同一命令连续跑两次，`ref-instrument` 的 WAV 哈希不同，频谱余弦在 0.99x 抖动，
`M1` 门禁偶发不过。

**根因**：`synth/src/random.rs` 的 `Random::new()` 用 `getrandom` 取系统熵，所有随机相位/
弓位扰动/揉弦漂移都跟着变。

**修法**：本地 override（`vem/reference-overrides/synth/src/random.rs`）新增 `with_seed()`，
并让 `new()` 优先读环境变量 `VEM_SEED`。两个参考 bin 都接受 `--seed`（默认 12345），
在创建 `Instrument`/`Director` **之前** `set_var("VEM_SEED", ...)`。

> 副作用（有意为之）：我们的引擎用**同一套 LCG 常数**（Numerical Recipes）并在同序消费随机数，
> 这样同种子下可以与参考实现做到逐采样级对齐，M1 排错时能直接二分到“第几个谐波开始不同”。

## 2. Director 与 Instrument 的电平差 ~48 dB，不要直接混着比

**症状**：拿 `ref-instrument`（裸 Instrument）和 `Director` 渲出来的比，RMS 差 48 dB，
误以为引擎电平写错。

**根因**：`Director::generate()` 末尾有 `scale = 0.01/sqrt(instrument_count)`；
另外单声部 pan = 0.25π（×0.707），还会叠加琴体 `body_resonance`（violin 0.18）与弓噪。

**修法**：
- **M1**：只比 `ref-instrument`（裸 Instrument + 复刻的 Arco 包络），我们的引擎也走同样的包络；
- **M3+**：比 `ref-render`（完整 Director），我们的引擎同时开琴体/弓噪/总线。
- 两种真值不要交叉使用。

## 3. VEM 的“控制率”是 100 采样（480 Hz @48 k）

`Director::generate()`：

```rust
if self.step % 100 == 0 { self.process_messages(); }   // MIDI 消息
self.step += 1;
// Division::generate(): if director.step % 100 == 0 { update_transitions(); }
```

**影响**：
- 松键最多晚 100 采样（≈2 ms）才生效；
- 包络是每 100 采样跳一次的阶梯（raised-cosine 插值本身连续，但参数是块更新的）；
- 起音前 99 个采样音量为 0。

**我们的做法**：M1 对拍时保留同样的 100 采样控制率（保证包络相关指标可比），
M2 再把控制率提到 64 采样块（与 AudioWorklet 一致），并用指标验证差异可忽略。

## 4. `f32 → i64` 截断会让“7000”变成“6999”

Arco 攻击时间：`1000 + (30000.0*(1.0-velocity)) as i64`。
`1.0 - 0.8` 在 f32 里是 `0.19999999`，乘 30000 → `5999.99`，`as i64` 截断成 **5999**。

不是 bug，是参考实现的真实行为。我们的引擎必须用同样的表达式与类型，否则包络差 1 采样，
逐采样对拍会从这里开始分叉。

## 5. Windows PowerShell 5.1 读不了“无 BOM 的 UTF-8 .ps1”

**症状**：`fetch-reference.ps1` 报 `表达式或语句中包含意外的标记 "}"`，行号指向中文注释后的括号；
中文全部乱码。

**根因**：PowerShell 5.1 按 ANSI 解码无 BOM 的脚本，中文多字节序列把后面的引号吞掉了。

**修法**：`.ps1` 一律保存为 **UTF-8 with BOM**（本仓库的 `vem/*.ps1` 已带 BOM）；
或者脚本里只用 ASCII。`pwsh` 7 没这个问题，但用户机器不一定有 `pwsh`。

## 6. Rust → wasm 的导出与内存（与 emscripten 不同）

- `wasm32-unknown-unknown` 的 `#[no_mangle] extern "C"` 导出**不带前导下划线**，与 msfa 的
  standalone 产物一致，`web/worklet.js` 里现成的 `_` 别名兼容逻辑可以照用；
- 没有 emscripten 的 `malloc/free`，所以额外导出 `vem_alloc(bytes)` / `vem_free_mem(ptr, bytes)`；
- 当前骨架产物 **33.2 KB、imports = 0**（`node scripts/check-standalone.mjs vem/wasm/vem.wasm`）；
- `panic=abort` + 不格式化 panic，避免把 `core::fmt` 拖进来（体积报告见 M6）。

## 7. 对拍一律用 32-bit float WAV

16-bit 写出会在接近 0 dBFS 的地方引入第二重削顶（`docs/MCP踩坑记录.md` §19 踩过）。
两个参考 bin 与我们的 `render` 默认都写 **IEEE float32 stereo**，现有
`compare-windows / band-energy / peak-report / vem-metrics` 都已支持（`bits===32 → readFloatLE`）。

## 8. 小提琴在参考实现里是 2× 过采样

`InstrumentType::Violin` 的 `sample_rate = 2.0*SAMPLE_RATE`。
`generate()` 每个输出采样调用两次 `generate_internal()` 再取 `0.5*(a+b)` —— 这是**简单平均**，
不是正经半带抽取（计划 §3 的可选改进点）。M1 先照抄，保证“还原优先”。

## 9. 参考实现的 `ref-instrument` 只复刻了 Arco

其它 6 种弓法的包络/滑音/换弓逻辑在 `Director`/`Division` 里，M2 用 `ref-render` 做真值。
不要拿 `ref-instrument` 渲非 Arco（会直接退出码 2）。

---

# M1（谱合成）阶段

## 10. 【第一枪】realfft 的逆变换**不归一化** → 电平差正好 n 倍

**症状**：第一次对拍，频谱余弦已经 1.0000、基频 0.00 cent，但电平差 **-46.76 dB**；
峰值比 ≈ 218 = A4 在 96 kHz 内部采样率下的周期长度。

**根因**：`rustfft`/`realfft` 的逆变换都不乘 `1/n`（正变换 + 逆变换 = n×）。
参考实现直接把 `ComplexToReal::process` 的结果当波形用，所以输出天然带 `n` 倍；
我们的自写 IFFT 一开始按"教科书式 DFT"乘了 `1/n`。

**修法**：`vem/src/fft.rs` 的 `dft_inplace` **不做**归一化（与 realfft 一致）；
Bluestein 路径只保留卷积本身的 `1/m`。修好后电平差 0.00 dB。

> 教训：频谱/包络都对了但**电平整体差一个整数倍**时，先怀疑 FFT 的归一化约定，
> 不要先去调增益。

## 11. 参考实现自带 ±3.5 cent 的随机频率漂移

**症状**：`ref-instrument --note 69` 测出来 440.8 Hz（+3.3 cent），关掉揉弦也还在；
不同音高/种子偏差方向不一样。

**根因**：`frequency_drift` 是 OU 随机游走，`frequency_drift_amplitude = 0.002`（= ±3.5 cent），
`vibrato` 只是叠加在它上面的。ACF 在有限窗口里测到的是这次随机轨迹的均值。

**修法/结论**：
- M1 门禁比较的是**我们 vs 参考**的基频差（0.00 cent），不是"vs 音名"；
- `vem-metrics` 里"相对 note"那行只作参考，不参与 PASS/FAIL；
- 以后要做"音准比原版好"，把 `frequency_drift_amplitude` 调小/关掉即可（已参数化）。

## 12. legato 换音的包络从 **50%** 接上，不是从 0

`Division::note_on` 的 Arco 分支：

```rust
let start_envelope = 0.5*self.envelope[0];
self.add_transition(0, attack_time, EnvelopeChange { start_envelope, end_envelope: 1.0 });
```

而且 `update_transitions` 在 `note_on` 末尾**立刻执行一次**，所以换音瞬间音量会掉到一半再起音，
弓位（`bow_position_shift`）只在**非 legato**时归位。

我们的 `Voice::start(legato)` 复刻了这个行为；legato 对拍（69→72 @1.5s）SNR 136.8 dB。

## 13. 参考实现的"释放"只是停止激励，尾巴衰减很慢

**症状**：note off 之后 0.5 s 还有 -54 dB 的声（对 56 的峰值），1 s 后还有 0.1 左右；
以为释放包络没生效。

**根因**：`volume=0` 只是让 `add_bow_excitation` 不再累加新能量；
阻尼是对**每个谐波**的，基频那几根衰减极慢（~3%/周期），而周期只有 2 ms。

**修法**：测试/门禁里的"静音"判定用**相对峰值**（如 `tail < peak*1e-4`），
不要用绝对 -100 dB；对拍窗口默认取 0.5–3.5 s，避开这个长尾。

## 14. 自写 FFT 的对拍结果（M1）

四件乐器 × 2 音高 + 力度/弓位/揉弦/legato 共 13 个用例：
频谱余弦（线性 & 1/3 oct）全部 **1.0000**、电平差 **0.00 dB**、包络相关 **1.0000**、
逐采样 SNR **135.5–137.2 dB**（差值来自 FFT 舍入，不是算法差异）。
wasm：**64.2 KB**、imports = 0。

---

# M2（演奏性）阶段

## 15. `Division::generate` 在音量为 0 时**跳过**乐器推进

**症状**：M2 第一次对拍，arco 的电平/频谱都对，但 SNR 是负数、最佳相关位移约 -250 采样；
频谱余弦 0.997、基频差 1.6 cent。

**根因**：参考实现的 `Division::generate`：

```rust
if self.instruments[0].get_volume() > 0.0 { self.steps_until_off = 10000; }
if self.steps_until_off == 0 { return (0.0, 0.0); }   // ← 直接返回，乐器一帧都不生成
```

起音前 100 个采样（音量还是 0）**根本没有调用 `Instrument::generate`**，
所以乐器的周期边界、揉弦相位、频率漂移都比"从 0 开始生成静音"晚 100 采样。
我们的引擎一开始是每采样都生成 → 揉弦轨迹从此分叉。

**修法**：`Voice.has_sounded` 门：音量为 0 且还没响过时跳过 `inst.generate`。
`ref-instrument` 也必须加同样的门，否则 M1 与 M2 的真值会互相矛盾。

## 16. transition “还没开始”时不能改写参数

**症状**：marcato 的包络前 20 ms 直接跳到峰值（3.4），参考却从 0 慢慢起；
包络相关 0.975，失败。

**根因**：marcato 有**两段**包络 transition：`0→attack` 升到 peak，`attack→3×attack` 回落到 1。
我们的插值函数在 `step < start` 时返回 `from`，于是第二段在**起点前**就把包络写成了 peak。
参考实现的 `update_transitions` 只在 `j >= transition.start` 时才写：

```rust
if j >= transition.start {  /* 才计算 weight 并赋值 */ }
```

**修法**：`Trans::value()` 返回 `Option<f32>`，未开始返回 `None`，调用方跳过。
Arco 只有一段且从 0 开始，所以 M1 没暴露这个问题。

## 17. ColLegno 的弓噪不受 `bow_noise` 控制

```rust
let noise_scale = match articulation {
    ColLegno => director.bow_noise_scale,          // ← 没乘 bow_noise
    Tremolo => 1.5*scale*bow_noise,
    _ => scale*bow_noise,
};
```

所以 `ref-render --bow-noise 0` 对 collegno **无效**，对拍会多出 -3 dB 的噪声层。
给参考加了另一个开发期开关 `VEM_NOISE=0`（与 `VEM_BODY=0` 同一处注入），对拍时两个都关。

## 18. M2 对拍结果

真值 = `ref-render`（Director 全链路）+ `VEM_BODY=0` + `VEM_NOISE=0`：

| 用例 | 频谱余弦 | SNR |
|---|---|---|
| arco / marcato | 1.0000 | 136.5 / 136.4 dB |
| glissando 69→76 @0.75s | 1.0000 | 135.7 dB |
| spiccato / pizzicato / collegno | 1.0000 | 136.4 / 137.0 / 136.6 dB |
| tremolo（violin / cello） | 1.0000 | 135.6 / 136.1 dB |
| viola pizzicato / bass marcato | 1.0000 | 136.2 / 135.1 dB |

wasm：**66.9 KB**、imports = 0。

---

# M3（琴体 + 总线 + 弓噪）阶段

## 19. 数据不进 wasm：`VEMD` 容器 + 运行时装载

琴体 IR 4 件乐器共 ~380 KB、弓噪 5.21 s 就有 ~1 MB（f32），直接嵌进 wasm
和"≤120 KB"的目标冲突；而且计划 §9 要求 v1 的数据自采/自合成。

做法：
- 数据走 **`vem_set_patch(blob, len)`**（原生侧 `render --data <dir>`），格式见 `vem/src/data.rs`；
- 开发期用 `vem/tools/gen-data` 把参考实现的 FLAC 转成 blob（只用于对拍，产物在 `out/vem-data/`，不入库）；
- 交付时换自采/合成数据，wasm 本体不背素材。

## 20. 琴体卷积可以"零延迟"逐采样对齐

参考实现 `reverb.rs` 不是普通的 overlap-add：
- IR **前 16 个 tap 直接时域卷积**（所以当前采样立刻有输出）；
- 其余部分按 **2 倍递增**的分块做 FFT，结果加到环形输出缓冲的**未来位置**。

只要是同一个线性卷积，分块方式不同只带来舍入差。我们按同样结构实现
（`vem/src/body.rs`），对拍结果：**SNR 125–128 dB**、频段能量差 **0.0 dB**。
注意 FFT 逆变换同样**不乘 1/n**，分块结果要乘 `1/width`（和参考一样）。

## 21. 弓噪的起点要跟着"第一次随机抽样"走

参考在 `Division::initialize_instruments` 里为每个乐器分配噪声起点：
`noise_position = len × (i + 0.5×uniform) / instrument_count`。
这个 `uniform` 是 Division 随机序列的**第一次**抽样——如果我们的引擎不消费它，
后面 spiccato/tremolo 的随机数就整体错位。

做法：`Engine::new/reset` 先 `rng.get_uniform()` 存成 `noise_anchor`，装载数据时再用它算起点。

## 22. 总线（压缩/混响/软限幅）默认关闭

参照 DX7 版的总线（`msfa_wrapper.cpp`）逐段移植：
`通道增益 → 压缩 → Freeverb（干湿）→ 主音量 → softClip(0.98)`。
但 VEM 参考没有总线，**默认关闭**（`--bus` 才开），否则 M1–M3 的对拍都会多一层非线性。
冒烟结果：`--bus` 输出 peak 0.56、无 NaN/Inf、无削顶。

## 23. M3 对拍结果

真值 = `ref-render`（完整 Director：琴体 + 弓噪），我们的侧 `--data out/vem-data`：

| 用例 | 频谱余弦 | 频段能量差 | SNR |
|---|---|---|---|
| violin / viola / cello / bass arco | 1.0000 | 0.0 dB | 125.3–128.1 dB |
| violin spiccato / pizzicato / tremolo | 1.0000 | 0.0 dB | 同量级 |

wasm：**83.5 KB**、imports = 0（不含素材）。

---

# M4（CH3 单轨替换）

## 24. `notes.bin` 格式（与 worklet 一致）

```
0   "MFA1"
4   u32 事件数
8   f32 总时长（秒）
12  每事件 8 字节：f32 t | u8 ch | u8 note | u8 vel | u8 on(0/1)
```

《梁祝》这份数据：4824 事件 / 279.25 s / 10 通道；CH3 有 411 个 note-on
（333 个"从静音起音"，另外 78 次是同音重复触发，区间没有真正重叠 → VEM 单声部够用）。

## 25. 参考实现的默认包络不适合 30 ms 的短音

CH3 音符长度中位数 **0.03 s**，而 VEM 默认起音是
`1000 + 30000×(1-velocity)` 采样 —— 力度 74 时约 **281 ms**，音符还没起完就松键了，
听感会糊成一片，和 DX7 原 patch 的颗粒感差很远。

所以我们给整曲混音加了一个 **`melody` 预设**（不是改引擎默认值）：
起音 8 ms + 60 ms×力度项、释放 60 ms + 120 ms、揉弦 120 ms 后渐入。
`render-mix.mjs` 默认用它；`--preset fidelity` 回到参考实现的原始参数。

## 26. M4 不用"绝对包络相关"做门禁

试过 10 ms RMS 包络相关：整曲只有 0.70，但这不代表节奏错——
CH3 长休止段两边都是静音（相关无意义），而乐器音色/释放本来就不同。
更稳的指标是 **"活动一致"**：50 ms 帧、相对峰值 -40 dB 判"这一帧有没有声音"，
算 Jaccard / 召回。结果：

| 指标 | 数值 |
|---|---|
| 整曲电平差 | **+0.05 dB** |
| 最佳时移 | **0 ms** |
| 活动一致（Jaccard / 召回） | **0.999 / 0.999**（缺 5 帧、多 2 帧，共 5600+ 帧） |
| 1/3 倍频程谱余弦 | **0.9883**（CH3 音色本来就不同） |
| CH3 stem 电平 | DX7 0.1175 / VEM 0.1297（+0.9 dB，混音时按 RMS 自动校准） |

## 27. 两引擎混音方式

- DX7 侧：`msfa.wasm` standalone，10 轨里除 CH3 外都按 `tracks.json` 的 patch/增益渲染，
  总线 FX 全关（对齐 REAPER 干混）；
- VEM 侧：CH3 事件单独喂给 `vem.wasm`（单声部，带琴体/弓噪）；
- 电平：用 DX7 的 CH3 stem RMS 自动校准 VEM 增益（本例 -0.9 dB）；
- 相加后写 float32（peak 0.887，未削顶），与 `out/p03-reaper.wav` 对比。

---

# M5（Web / AudioWorklet）

## 28. 一个 worklet 里跑两套 wasm

`web/worklet.js` 同时实例化 `msfa.wasm`（9 轨）与 `vem.wasm`（CH3）：
- CH3 不再创建 DX7 实例（`set_patch` 跳过），事件按通道分流；
- 两个引擎各自渲染 64 采样块后相加；总线 FX 分别下发（VEM 用我们自己的压缩/混响）；
- seek 时 `msfa_mixer_reset` + `vem_reset`，并重新下发音色/参数；
- CH3 轨道推子联动 `vem_set_gain`（初始 = 轨道音量 × 0.906 校准）。

## 29. 琴体卷积是 CPU 大头：17% → 2.8%

第一版直接照抄参考的分块卷积，Node/wasm 实测 **17.3% 实时**（单声部，含琴体+弓噪），
远超 3% 目标。逐项排查：
- 关掉琴体 → 0.63%；只开噪声 → 0.88% → **琴体就是瓶颈**；
- IR 长度 16/64/2048/8192/17891 采样时分别是 1.0 / 2.7 / 8.8 / 12.4 / 17.5%。

优化分三步：
1. **2 的幂长度走专用迭代 radix-2**（原来是通用混合基递归 + 逐层拷贝）：17.5% → 4.1%；
2. **实数 FFT**（n 点实数打包成 n/2 点复数再解包）：4.1% → 3.3%；
3. radix-2 热循环去掉边界检查：3.3% → **2.80%**。

> 关键点：我们的"不归一化"约定必须一致——长度 n 的实数逆变换返回 `n·x`，
> 打包版是 `2·IFFT_{n/2}`（不是 `1/m`，这个写错会让 M3 的电平差 n 倍）。

## 30. M5 实测

| 项 | 结果 |
|---|---|
| VEM 单声部 CPU（48 k，含琴体+弓噪） | **2.80% 实时**（Node/V8 wasm） |
| 整曲 worklet（9 轨 DX7 + VEM） | 17.7% 实时（Node 假环境） |
| 浏览器冒烟（ego-browser，Chrome） | ready @48 k / 4824 事件 / 播放时间前进 / 电平表 11% |
| VEM 参数 UI | 弓法、弓位、揉弦深度/渐入、起音、弓噪、琴体 |

---

# M7（弦乐组：中提琴 / 大提琴 / 低音提琴）

## 31. 轨道分析决定乐器与拆分

| 轨道 | 原音色 | 音域（note） | 音符 | 同时发声 | 结论 |
|---|---|---|---|---|---|
| CH3 | StringFlex | 59–100 | 411 | 1（无真重叠） | 小提琴，单声部 legato |
| CH6 | Sitar 2 | 50–86 | 192 | 最多 4 | **中提琴**（48–88 正合适） |
| CH8 | STRING-PNO | 31–100 | 366 | 最多 8 | **按音高拆分**：≤43 低音提琴、≥44 大提琴 |

拆分点 43/44：低音提琴最高 67、大提琴最低 36，重叠区取 43 让低音声部更像"低音提琴组"。
两条同通道的引擎在事件分发时按 `minNote..maxNote` 路由，互不重复。

复音最多 8 → `MAX_VOICES` 从 8 提到 **12**（要留出释放尾巴占用的槽位，否则会抢音）。

## 32. 同一 wasm 实例跑多个引擎

`vem.wasm` 只实例化一次，`vem_new(sr, instrument)` 建 4 个引擎；
琴体/弓噪数据按乐器分别 `vem_set_patch` 装载（每个引擎各自一份噪声缓冲，约 1 MB/引擎）。

## 33. 电平校准：新声部比 DX7 原音色"响"得多

| 通道 | DX7 stem RMS | VEM 合成 RMS | 校准增益 |
|---|---|---|---|
| CH3 | 0.1175 | 0.1297 | -0.9 dB |
| CH6 | 0.0124 | 0.1474 | **-21.5 dB** |
| CH8 | 0.0303 | 0.4482 | **-23.4 dB** |

DX7 的 Sitar2 / STRING-PNO 在混音里本来就很轻，所以按 RMS 对齐后中提琴/大提琴会比"想象中"安静。
UI 的轨道推子现在对 VEM 通道下发**绝对增益**（初始值 = 校准值，可往上推），
想要更突出的弦乐组直接推 CH6/CH8 推子即可，不用改代码。

## 34. M7 验收

| 项 | 结果 |
|---|---|
| 每通道召回（VEM 覆盖 DX7 有声帧） | CH3 0.975 / CH6 0.996 / CH8 0.998 |
| 整曲 vs REAPER | 电平 **+0.05 dB**、时移 **0 ms**、活动一致 **0.999**、1/3 oct 余弦 **0.9877** |
| 峰值 | 0.955（float 输出，未削顶） |
| 浏览器（Chrome） | 4 个 VEM 声部行、10 轨推子、播放推进、电平 7.7% |
| wasm | **88.3 KB**、imports 0 |

> 注意：单通道的"包络时移"对稀疏声部不可靠（长休止导致相关峰值漂移）；
> 门禁用**召回**（DX7 有声的帧我们是否覆盖），整曲时移用完整的 m4-check。






