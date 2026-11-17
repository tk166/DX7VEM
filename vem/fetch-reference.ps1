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
git -C $dest reset --hard --quiet $Commit

Write-Host "apply local overrides（不改上游算法，只新增 bin + 固定种子）"
$ov = Join-Path $root "reference-overrides"
New-Item -ItemType Directory -Force (Join-Path $dest "synth/src/bin/common") | Out-Null
Copy-Item (Join-Path $ov "synth/src/bin/ref_instrument.rs") (Join-Path $dest "synth/src/bin/ref_instrument.rs") -Force
Copy-Item (Join-Path $ov "synth/src/bin/ref_render.rs") (Join-Path $dest "synth/src/bin/ref_render.rs") -Force
Copy-Item (Join-Path $ov "synth/src/bin/common/mod.rs") (Join-Path $dest "synth/src/bin/common/mod.rs") -Force

# synth/Cargo.toml 追加两个 [[bin]]（不覆盖上游文件，保持仓库无 LGPL 拷贝）
$manifest = Join-Path $dest "synth/Cargo.toml"
$text = [IO.File]::ReadAllText($manifest)
if (-not $text.Contains('name = "ref-instrument"')) {
    $text += "`n[[bin]]`nname = `"ref-instrument`"`npath = `"src/bin/ref_instrument.rs`"`n`n[[bin]]`nname = `"ref-render`"`npath = `"src/bin/ref_render.rs`"`n"
    [IO.File]::WriteAllText($manifest, $text, (New-Object Text.UTF8Encoding $false))
}

# random.rs：允许 VEM_SEED 固定种子（字符串注入，不把 LGPL 源码放进本仓库）
$rnd = Join-Path $dest "synth/src/random.rs"
$rt = [IO.File]::ReadAllText($rnd)
if (-not $rt.Contains("VEM_SEED")) {
    $anchor = "    pub fn new() -> Self {"
    if (-not $rt.Contains($anchor)) { throw "random.rs 结构变了，无法注入 VEM_SEED" }
    $inject = "    pub fn new() -> Self {`n" +
              "        // [local override] VEM_SEED 固定种子（开发期对拍用）`n" +
              "        if let Ok(text) = std::env::var(`"VEM_SEED`") {`n" +
              "            if let Ok(seed) = text.trim().parse::<u32>() {`n" +
              "                return Self {i: seed, next_normal: 0.0, next_normal_valid: false};`n" +
              "            }`n" +
              "        }`n`n"
    $rt = $rt.Replace($anchor, $inject)
    [IO.File]::WriteAllText($rnd, $rt, (New-Object Text.UTF8Encoding $false))
}

# director.rs 的开发期 A/B 开关（不改算法）：
#   VEM_BODY=0  关掉琴体共振
#   VEM_NOISE=0 关掉弓噪（collegno 分支不受 --bow-noise 控制，必须用这个）
$director = Join-Path $dest "synth/src/director.rs"
$text = [IO.File]::ReadAllText($director)
if (-not $text.Contains("VEM_NOISE")) {
    $anchor = "        let ir = match instrument_type {"
    if (-not $text.Contains($anchor)) { throw "director.rs 结构变了，无法注入 VEM_BODY/VEM_NOISE" }
    $inject = "        // [local override] dev A/B gates (algorithms unchanged)`n" +
              "        self.body_resonance *= std::env::var(`"VEM_BODY`").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);`n" +
              "        self.bow_noise_scale *= std::env::var(`"VEM_NOISE`").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);`n`n"
    $text = $text.Replace($anchor, $inject + $anchor)
    [IO.File]::WriteAllText($director, $text, (New-Object Text.UTF8Encoding $false))
}

if ($Rebuild -or -not (Test-Path (Join-Path $dest "target/release/ref-instrument.exe"))) {
    Write-Host "build reference bins"
    cargo build --release -p synth --manifest-path (Join-Path $dest "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "参考实现编译失败" }
}
Write-Host "OK: $dest"
Write-Host "  ref-instrument.exe  → M1 谱合成真值"
Write-Host "  ref-render.exe      → M2/M3 全链路真值（VEM_BODY=0 可关琴体）"
