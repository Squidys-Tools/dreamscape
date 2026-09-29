<#
.SYNOPSIS
  Builds the canvas renderer for the browser and hosts it for measurement.

.DESCRIPTION
  The web counterpart to bench.ps1. Same renderer source, same scene
  parameters, same RESULT line, so the two lines can be read next to each other.

  The page posts the line it measured back to the server, which appends it to
  -ResultLog. Reading the number off the screen instead is a transcription, and
  an unreproducible figure is worse than no figure.

  Frame time here is draw_frame plus the queue reporting all submitted work
  done, which is the browser's equivalent of the harness's device.poll(Wait).
  Both serialise CPU and GPU, so both are ceilings.

.PARAMETER Scenario
  quick  2k items, 120 warmup, 200 frames
  bench  (default) 2k / 8k / 20k distinct, the same rows as the native bench
  scale  item counts doubled with everything on screen

.PARAMETER Atlas
  Edge length of the texture atlas. Defaults to 2048, to match the native runs.

.PARAMETER ResultLog
  Append each run's RESULT line to this file.

.PARAMETER Backend
  auto (default), webgpu or webgl. Forcing one is how the fallback gets tested:
  the same seam and the same renderer, one different field.

.PARAMETER Width
  Drawing-buffer width in device pixels. Defaults to 2560, the native viewport.

.PARAMETER Height
  Drawing-buffer height in device pixels. Defaults to 1440.

.PARAMETER NoOpen
  Start the server without opening a browser.
#>
param(
  [ValidateSet('quick', 'bench', 'scale')]
  [string]$Scenario = 'bench',

  [int]$Atlas = 2048,

  [string]$ResultLog,

  [ValidateSet('auto', 'webgpu', 'webgl')]
  [string]$Backend = 'auto',

  [int]$Width = 2560,

  [int]$Height = 1440,

  [switch]$NoOpen
)

$ErrorActionPreference = 'Stop'
$crate = Join-Path $PSScriptRoot '..\crates\canvas-wasm'
Set-Location (Split-Path -Parent $PSScriptRoot)

if ($ResultLog) {
  $dir = Split-Path -Parent $ResultLog
  if (-not (Test-Path -LiteralPath $dir)) { throw "ResultLog directory does not exist: $dir" }
}

# The WebGL2 fallback needs its backend compiled in, which drags GLES and wgpu-hal
# into the bundle. Off by default, so this is the only thing that turns it on.
$features = if ($Backend -eq 'webgl') { '--features webgl' } else { '' }

Write-Host ''
Write-Host 'building the renderer for wasm32-unknown-unknown...' -ForegroundColor Cyan
& wasm-pack build $crate --target web --release --out-dir pkg --out-name canvas_wasm $features
if ($LASTEXITCODE -ne 0) { throw "wasm-pack build failed" }

# Reported because it is a result, not a detail: the download size is different
# for a local desktop file and a network-fetched hosted tier.
$wasm = Get-Item (Join-Path $crate 'pkg\canvas_wasm_bg.wasm')
Write-Host ("  wasm      {0:N0} bytes ({1:N2} MB)" -f $wasm.Length, ($wasm.Length / 1MB))
Write-Host ''

# Port 0 lets the OS pick, so a second run never collides with the first and no
# port is ever committed.
$serverLog = Join-Path $env:TEMP "dreamscape-web-bench-$PID.log"
$server = Start-Process -PassThru -NoNewWindow -FilePath 'bun' `
  -ArgumentList @((Join-Path $crate 'web\serve.js'), '0', $(if ($ResultLog) { (Join-Path (Get-Location) $ResultLog) } else { '' })) `
  -RedirectStandardOutput $serverLog -RedirectStandardError "$serverLog.err"

try {
  # Poll the log for the bound port rather than sleeping a fixed amount.
  $url = $null
  for ($i = 0; $i -lt 100 -and -not $url; $i++) {
    if (Test-Path $serverLog) {
      $line = Get-Content $serverLog | Select-String 'http://127\.0\.0\.1:(\d+)' | Select-Object -First 1
      if ($line) { $url = "http://127.0.0.1:$($line.Matches[0].Groups[1].Value)" }
    }
    if (-not $url) { Start-Sleep -Milliseconds 100 }
  }
  if (-not $url) { Get-Content $serverLog, "$serverLog.err" -ErrorAction SilentlyContinue; throw 'the page server did not report a port' }

  $common = "atlas=$Atlas&backend=$Backend&width=$Width&height=$Height&bench=1"
  $rows = switch ($Scenario) {
    'quick' { @(2000) }
    'bench' { @(2000, 8000, 20000) }
    'scale' { @(2000, 4000, 8000, 16000, 32000) }
  }

  # Every run needs the board full, so the pan is zeroed in `scale` exactly as
  # bench.ps1 zeroes it, and the two runners are describing the same scene.
  $urls = foreach ($n in $rows) {
    $motion = if ($Scenario -eq 'scale') { 'pan=0&zoom=0.96&frames=150' } else { 'pan=18&zoom=0.7&frames=200' }
    "$url/web/index.html?$common&items=$n&textures=$n&$motion"
  }
  $interactive = "$url/web/index.html?atlas=$Atlas&backend=$Backend&width=$Width&height=$Height"

  if ($ResultLog) {
    "load before: $((Get-CimInstance Win32_Processor).LoadPercentage)% cpu" | Add-Content -Path $ResultLog
  }

  Write-Host 'browser canvas' -ForegroundColor Cyan
  Write-Host "  interactive  $interactive"
  foreach ($u in $urls) { Write-Host "  measure      $u" }
  Write-Host ''

  if (-not $NoOpen) {
    Start-Process $urls[0]
    if ($urls.Count -gt 1) {
      Write-Host "opened the first scenario; the rest are one navigation away." -ForegroundColor DarkGray
      Write-Host 'run every row by opening each measure URL above, or pick one and re-run with -Scenario quick.' -ForegroundColor DarkGray
    }
  }

  if ($ResultLog) {
    Write-Host "posting results to $ResultLog once each measure URL has run." -ForegroundColor DarkGray
  } else {
    Write-Host 'running. ctrl-c to stop.' -ForegroundColor DarkGray
  }
  # Deliberately an idle wait rather than polling for a fixed count: the number of
  # scenarios is the user's choice, and the RESULT lines are already on disk by
  # the time the last one arrives.
  while ($true) { Start-Sleep -Seconds 1 }
}
finally {
  # Only the process this script started, never a name match.
  if ($server -and -not $server.HasExited) { $server.Kill() }
  Remove-Item $serverLog, "$serverLog.err" -ErrorAction SilentlyContinue
}
