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

