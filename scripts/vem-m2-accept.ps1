# M2 验收：7 种弓法 + 换音滑音，与参考 Director（VEM_BODY=0、弓噪=0）对拍
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File scripts/vem-m2-accept.ps1
#
# 说明:
#   - 真值用 ref-render（Director 全链路），但用 VEM_BODY=0 关掉琴体、--bow-noise 0 关掉弓噪，
#     这样比的是"乐器 + 包络 + 声像 + 弓法状态机"；
#   - 我们的 render 不加 --raw（默认总线缩放 = 参考 Director 的 0.01×pan）。

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$refExe = Join-Path $root "vem/reference/target/release/ref-render.exe"
$ourExe = Join-Path $root "vem/target/release/render.exe"
$metrics = Join-Path $root "scripts/vem-metrics.mjs"
$outDir = Join-Path $root "out"
New-Item -ItemType Directory -Force $outDir | Out-Null
foreach ($p in @($refExe, $ourExe, $metrics)) { if (-not (Test-Path $p)) { throw "缺少: $p" } }

$cases = @(
    @{ id = "arco";     inst = "violin"; note = 69; from = 0.5; to = 2.5; extra = @("--art", "arco") },
    @{ id = "marcato";  inst = "violin"; note = 69; from = 0.0; to = 2.5; extra = @("--art", "marcato") },
    @{ id = "gliss";    inst = "violin"; note = 69; from = 0.5; to = 2.5; extra = @("--art", "glissando", "--note2", "76", "--note2-at", "0.75") },
    @{ id = "spiccato"; inst = "violin"; note = 69; from = 0.0; to = 0.4; extra = @("--art", "spiccato") },
    @{ id = "pizz";     inst = "violin"; note = 69; from = 0.0; to = 0.4; extra = @("--art", "pizzicato") },
    @{ id = "collegno"; inst = "violin"; note = 69; from = 0.0; to = 0.4; extra = @("--art", "collegno") },
    @{ id = "tremolo";  inst = "violin"; note = 69; from = 0.5; to = 2.5; extra = @("--art", "tremolo") },
    @{ id = "viola-pizz";  inst = "viola"; note = 60; from = 0.0; to = 0.4; extra = @("--art", "pizzicato") },
    @{ id = "cello-trem";  inst = "cello"; note = 48; from = 0.5; to = 2.5; extra = @("--art", "tremolo") },
    @{ id = "bass-marcato"; inst = "bass"; note = 36; from = 0.0; to = 2.5; extra = @("--art", "marcato") }
)

$failed = @()
$rows = @()

foreach ($c in $cases) {
    $refWav = Join-Path $outDir "m2-ref-$($c.id).wav"
    $ourWav = Join-Path $outDir "m2-vem-$($c.id).wav"

    $argv = @("--instrument", $c.inst, "--note", "$($c.note)", "--dur", "2", "--tail", "1", "--seed", "12345") + $c.extra

    $env:VEM_BODY = "0"
    $env:VEM_NOISE = "0"
    & $refExe @argv --bow-noise 0 --out $refWav | Out-Null
    $env:VEM_BODY = $null
    $env:VEM_NOISE = $null
    & $ourExe @argv --out $ourWav | Out-Null

    $text = & node $metrics $refWav $ourWav --from $c.from --to $c.to --note $c.note --gate
    $code = $LASTEXITCODE
    if ($code -ne 0) { $failed += $c.id }
    $rows += [pscustomobject]@{
        case = $c.id
        pass = ($code -eq 0)
        detail = (($text | Select-String -Pattern "逐采样差|结论").Line -join " | ")
    }
    Write-Host "=== $($c.id) ==="
    $text | ForEach-Object { Write-Host $_ }
}

Write-Host ""
Write-Host "===== M2 汇总 ====="
$rows | Format-Table -AutoSize
if ($failed.Count -gt 0) {
    Write-Host "FAIL: $($failed -join ', ')"
    exit 1
}
Write-Host "全部 PASS (M2 acceptance)"
