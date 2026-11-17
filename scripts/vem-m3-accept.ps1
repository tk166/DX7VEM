# M3 验收：琴体 IR + 弓噪（完整 Director 链路）与总线冒烟
#
# 前置:
#   cargo run --release --manifest-path vem/tools/gen-data/Cargo.toml
#   （或手动指定 out/vem-data，里面要有 body-<inst>.bin 和 noise.bin）
#
# 用法:
#   powershell -ExecutionPolicy Bypass -File scripts/vem-m3-accept.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$refExe = Join-Path $root "vem/reference/target/release/ref-render.exe"
$ourExe = Join-Path $root "vem/target/release/render.exe"
$metrics = Join-Path $root "scripts/vem-metrics.mjs"
$dataDir = Join-Path $root "out/vem-data"
$outDir = Join-Path $root "out"
New-Item -ItemType Directory -Force $outDir | Out-Null

foreach ($p in @($refExe, $ourExe, $metrics, (Join-Path $dataDir "noise.bin"))) {
    if (-not (Test-Path $p)) { throw "缺少: $p（先跑 gen-data）" }
}

$cases = @(
    @{ id = "violin-arco"; inst = "violin"; note = 69; extra = @("--art", "arco"); from = 0.5; to = 2.5 },
    @{ id = "viola-arco";  inst = "viola";  note = 60; extra = @("--art", "arco"); from = 0.5; to = 2.5 },
    @{ id = "cello-arco";  inst = "cello";  note = 48; extra = @("--art", "arco"); from = 0.5; to = 2.5 },
    @{ id = "bass-arco";   inst = "bass";   note = 36; extra = @("--art", "arco"); from = 0.5; to = 2.5 },
    @{ id = "violin-spiccato"; inst = "violin"; note = 69; extra = @("--art", "spiccato"); from = 0.0; to = 0.4 },
    @{ id = "violin-pizz";     inst = "violin"; note = 69; extra = @("--art", "pizzicato"); from = 0.0; to = 0.4 },
    @{ id = "violin-tremolo";  inst = "violin"; note = 69; extra = @("--art", "tremolo"); from = 0.5; to = 2.5 }
)

$failed = @()
$rows = @()

foreach ($c in $cases) {
    $refWav = Join-Path $outDir "m3-ref-$($c.id).wav"
    $ourWav = Join-Path $outDir "m3-vem-$($c.id).wav"
    $argv = @("--instrument", $c.inst, "--note", "$($c.note)", "--dur", "2", "--tail", "1", "--seed", "12345") + $c.extra

    & $refExe @argv --out $refWav | Out-Null
    & $ourExe @argv --data $dataDir --out $ourWav | Out-Null

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

# 总线冒烟：开压缩/混响/软限幅，检查不 NaN、不超 0 dBFS
Write-Host ""
Write-Host "===== 总线冒烟（--bus）====="
$busWav = Join-Path $outDir "m3-bus.wav"
& $ourExe --instrument violin --note 69 --dur 2 --tail 1 --data $dataDir --bus --out $busWav | Out-Null
$check = & node -e @"
const fs=require('fs');
const b=fs.readFileSync(process.argv[1]);
let off=12,doff=0,dl=0,f=null;
while(off+8<=b.length){const id=b.toString('ascii',off,off+4),l=b.readUInt32LE(off+4);
 if(id==='fmt ')f={ch:b.readUInt16LE(off+10),bits:b.readUInt16LE(off+22)};
 if(id==='data'){doff=off+8;dl=l;break;} off+=8+l+(l&1);}
const n=Math.floor(dl/(f.bits/8*f.ch)); let peak=0,bad=0;
for(let i=0;i<n;i++){const v=b.readFloatLE(doff+i*f.bits/8*f.ch); if(!Number.isFinite(v))bad++; peak=Math.max(peak,Math.abs(v));}
console.log(JSON.stringify({peak,bad,frames:n}));
"@ $busWav
$obj = $check | ConvertFrom-Json
if ($obj.bad -gt 0) { $failed += "bus-nan" }
if ($obj.peak -gt 1.0001) { $failed += "bus-peak" }
Write-Host "peak=$($obj.peak) bad=$($obj.bad) frames=$($obj.frames)"

Write-Host ""
Write-Host "===== M3 汇总 ====="
$rows | Format-Table -AutoSize
if ($failed.Count -gt 0) {
    Write-Host "FAIL: $($failed -join ', ')"
    exit 1
}
Write-Host "全部 PASS (M3 acceptance)"
