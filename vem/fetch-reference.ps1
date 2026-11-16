# 拉取并固定 VEM 参考实现（只用于开发期对拍，不参与交付）
#
# 上游: https://github.com/peastman/ViolaExMachina  (LGPL-2.1)
# 固定 commit + 本地 override（新增两个 [[bin]]、随机数支持固定种子）。
#
# 用法:
#   pwsh -File vem/fetch-reference.ps1
#   pwsh -File vem/fetch-reference.ps1 -Rebuild

param(
    [string]$Commit = "5ddfdaae519b062b5be6d9be662f16e388961e30",
    [switch]$Rebuild
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$dest = Join-Path $root "reference"

if (-not (Test-Path $dest)) {
    Write-Host "clone ViolaExMachina → $dest"
    git clone https://github.com/peastman/ViolaExMachina.git $dest
}

Write-Host "checkout $Commit"
git -C $dest checkout --quiet $Commit

Write-Host "apply local overrides（不改上游算法，只新增 bin + 固定种子）"
$ov = Join-Path $root "reference-overrides"
Copy-Item (Join-Path $ov "synth/Cargo.toml") (Join-Path $dest "synth/Cargo.toml") -Force
Copy-Item (Join-Path $ov "synth/src/random.rs") (Join-Path $dest "synth/src/random.rs") -Force
New-Item -ItemType Directory -Force (Join-Path $dest "synth/src/bin/common") | Out-Null
Copy-Item (Join-Path $ov "synth/src/bin/ref_instrument.rs") (Join-Path $dest "synth/src/bin/ref_instrument.rs") -Force
Copy-Item (Join-Path $ov "synth/src/bin/ref_render.rs") (Join-Path $dest "synth/src/bin/ref_render.rs") -Force
Copy-Item (Join-Path $ov "synth/src/bin/common/mod.rs") (Join-Path $dest "synth/src/bin/common/mod.rs") -Force

if ($Rebuild -or -not (Test-Path (Join-Path $dest "target/release/ref-instrument.exe"))) {
    Write-Host "build reference bins"
    cargo build --release -p synth --manifest-path (Join-Path $dest "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "参考实现编译失败" }
}

Write-Host "OK: $dest"
Write-Host "  ref-instrument.exe  → M1 谱合成真值"
Write-Host "  ref-render.exe      → M2/M3 全链路真值"
