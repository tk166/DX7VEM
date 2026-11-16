# 构建 wasm（standalone，无 JS import）
#
# 用法: pwsh -File vem/build-wasm.ps1 [-NoOpt]
# 产物: vem/wasm/vem.wasm（入库）

param([switch]$NoOpt)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot

Write-Host "cargo build --release --target wasm32-unknown-unknown"
cargo build --release --target wasm32-unknown-unknown --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "wasm 编译失败" }

$src = Join-Path $root "target/wasm32-unknown-unknown/release/vem.wasm"
$dstDir = Join-Path $root "wasm"
New-Item -ItemType Directory -Force $dstDir | Out-Null
$dst = Join-Path $dstDir "vem.wasm"
Copy-Item $src $dst -Force

$size = (Get-Item $dst).Length
Write-Host ("raw: {0:N0} bytes ({1:N1} KB)" -f $size, ($size / 1KB))

$opt = Get-Command wasm-opt -ErrorAction SilentlyContinue
if ($opt -and -not $NoOpt) {
    & $opt.Source -Oz $dst -o $dst
    if ($LASTEXITCODE -ne 0) { throw "wasm-opt 失败" }
    $size = (Get-Item $dst).Length
    Write-Host ("wasm-opt -Oz: {0:N0} bytes ({1:N1} KB)" -f $size, ($size / 1KB))
} else {
    Write-Host "wasm-opt 未安装，跳过（体积优化到 v1 收尾再做）"
}

Write-Host "产物: $dst"
node (Join-Path $root "../scripts/check-standalone.mjs") $dst
