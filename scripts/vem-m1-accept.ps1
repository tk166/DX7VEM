# M1 验收：四件乐器 × 多音高/弓位/力度 + legato，逐项跑 vem-metrics --gate
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File scripts/vem-m1-accept.ps1
#
# 依赖:
#   - vem/reference/target/release/ref-instrument.exe（vem/fetch-reference.ps1）
#   - vem/target/release/render.exe（cargo build --release）
#   - node + scripts/vem-metrics.mjs

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$refExe = Join-Path $root "vem/reference/target/release/ref-instrument.exe"
$ourExe = Join-Path $root "vem/target/release/render.exe"
$metrics = Join-Path $root "scripts/vem-metrics.mjs"
$outDir = Join-Path $root "out"
New-Item -ItemType Directory -Force $outDir | Out-Null

foreach ($p in @($refExe, $ourExe, $metrics)) {
    if (-not (Test-Path $p)) { throw "缺少: $p" }
}

$cases = @(
    @{ id = "violin-69"; inst = "violin"; note = 69; extra = @() },
    @{ id = "violin-84"; inst = "violin"; note = 84; extra = @() },
    @{ id = "viola-60"; inst = "viola"; note = 60; extra = @() },
    @{ id = "viola-72"; inst = "viola"; note = 72; extra = @() },
    @{ id = "cello-48"; inst = "cello"; note = 48; extra = @() },
    @{ id = "cello-60"; inst = "cello"; note = 60; extra = @() },
    @{ id = "bass-36"; inst = "bass"; note = 36; extra = @() },
    @{ id = "bass-48"; inst = "bass"; note = 48; extra = @() },
    @{ id = "violin-vel40"; inst = "violin"; note = 69; extra = @("--vel", "0.4") },
    @{ id = "violin-pont"; inst = "violin"; note = 69; extra = @("--bow", "0.3") },
    @{ id = "violin-tasto"; inst = "violin"; note = 69; extra = @("--bow", "0.7") },
    @{ id = "violin-vib0"; inst = "violin"; note = 69; extra = @("--vibrato", "0.0") },
    @{ id = "violin-legato"; inst = "violin"; note = 69; extra = @("--note2", "76", "--note2-at", "1.5") }
)

$failed = @()
$rows = @()

foreach ($c in $cases) {
    $refWav = Join-Path $outDir "m1-ref-$($c.id).wav"
    $ourWav = Join-Path $outDir "m1-vem-$($c.id).wav"

    $argv = @("--instrument", $c.inst, "--note", "$($c.note)", "--dur", "3", "--tail", "1", "--seed", "12345") + $c.extra
    & $refExe @argv --out $refWav | Out-Null
    & $ourExe @argv --out $ourWav --raw | Out-Null

    $text = & node $metrics $refWav $ourWav --from 0.5 --to 3.5 --note $c.note --gate
    $code = $LASTEXITCODE
    $summary = ($text | Select-String -Pattern "逐采样差|结论").Line -join " | "
    if ($code -ne 0) { $failed += $c.id }
    $rows += [pscustomobject]@{ case = $c.id; pass = ($code -eq 0); detail = $summary }
    Write-Host "=== $($c.id) ==="
    $text | ForEach-Object { Write-Host $_ }
}

Write-Host ""
Write-Host "===== M1 汇总 ====="
$rows | Format-Table -AutoSize
if ($failed.Count -gt 0) {
    Write-Host "FAIL: $($failed -join ', ')"
    exit 1
}
Write-Host "全部 PASS (M1 acceptance)"
