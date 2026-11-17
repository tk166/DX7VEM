# VEM 弦乐引擎（Rust / wasm）

把 Viola Ex Machina 的**谱合成弦乐**思路做成我们自己的引擎：**先保证还原**（音色/演奏法可信），
体积优化放到后期。与现有 DX7 引擎（`../261115-daw`，C++/wasm）并存。

- 开发计划：`docs/开发计划-VEM弦乐引擎.md`
- 踩坑记录：`docs/VEM踩坑记录.md`
- 引擎说明 / 命令：`vem/README.md`

## 目录分工

| 目录 | 作用 |
|---|---|
| `vem/` | 新引擎（Rust，独立构建；wasm 产物入库） |
| `vem/reference/` | 上游 VEM 固定 commit 副本（**gitignore**，只做开发期对照，不参与交付） |
| `web/` | 播放器 baseline：DX7 `msfa.wasm` + worklet + `data/`（M5 接 VEM） |
| `scripts/` | 对拍/验收工具（DX7 仓库复用 + `vem-metrics` / `render-mix` / `m4-check`） |
| `docs/` | 计划与本项目踩坑记录 |
| `out/` | 渲染产物（gitignore） |

## 快速开始

```powershell
cargo test --manifest-path vem/Cargo.toml

powershell -ExecutionPolicy Bypass -File vem/fetch-reference.ps1
.\vem\reference\target\release\ref-instrument.exe --instrument violin --note 69 --dur 3 --tail 1 --out out/ref-69.wav --seed 12345

cargo run --release --manifest-path vem/Cargo.toml --bin render -- --instrument violin --note 69 --dur 3 --tail 1 --out out/vem-69.wav

node scripts/vem-metrics.mjs out/ref-69.wav out/vem-69.wav --from 0.5 --to 3.5 --note 69 --gate
```

## 进度

| 里程碑 | 状态 |
|---|---|
| M0 骨架 + 参考真值 + 对拍管线 | ✅ |
| M1 单音还原（谱合成 + 自写 IFFT） | ✅ |
| M2 演奏性（7 种弓法/弓位/揉弦） | ✅ |
| M3 琴体 IR + 总线 FX | ✅ |
| M4 接数据、CH3 单轨替换 | ✅ |
| M5 Web/AudioWorklet 集成 | ✅ |
| M6 收尾（体积/CPU/许可） | ⬜ |
