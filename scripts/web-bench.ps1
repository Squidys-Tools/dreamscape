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
  Start the server without launching a browser. For driving it yourself, or on a
  headless machine.

.PARAMETER Measure
  Open the first measure URL instead of the interactive board. The interactive
  page does not post a result, so a run meant to capture a number needs this.
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

  [switch]$NoOpen,

  [switch]$Measure
)

$ErrorActionPreference = 'Stop'
$crate = Join-Path $PSScriptRoot '..\crates\canvas-wasm'
Set-Location (Split-Path -Parent $PSScriptRoot)

if ($ResultLog) {
  # Only check when the path actually has a directory part. A bare filename is
  # the most natural invocation and `Split-Path -Parent` returns nothing for it,
  # which then looks like a missing directory.
  $dir = Split-Path -Parent $ResultLog
  if ($dir -and -not (Test-Path -LiteralPath $dir)) {
    throw "ResultLog directory does not exist: $dir"
  }
}

# The two external tools, resolved rather than assumed.
#
# `wasm-pack` is a cargo install, so it lands in the per-user cargo bin, and `bun`
# is a per-user install too. Both directories are on PATH in some shells and not
# in others, and a bare "the term is not recognized" tells nobody what to do. The
# browser tier is supposed to work on a fresh clone on another Windows machine,
# and a script that dies on a PATH quirk is that machine's first impression.
function Resolve-External {
  param([string]$Name, [string]$Hint)

  $onPath = Get-Command $Name -ErrorAction SilentlyContinue
  if ($onPath) { return $onPath.Source }

  foreach ($dir in @("$env:USERPROFILE\.cargo\bin", "$env:USERPROFILE\.bun\bin")) {
    $candidate = Join-Path $dir "$Name.exe"
    if (Test-Path -LiteralPath $candidate) { return $candidate }
  }
  throw "$Name was not found. $Hint"
}

$wasmPack = Resolve-External 'wasm-pack' 'Install it with: cargo install wasm-pack'
$bun = Resolve-External 'bun' 'Install it from https://bun.sh, or add %USERPROFILE%\.bun\bin to PATH.'

# The WebGL2 fallback needs its backend compiled in, which drags GLES and wgpu-hal
# into the bundle. Off by default, so this is the only thing that turns it on.
# Built as an array because an empty string variable still becomes an argument,
# and cargo rejects a blank one.
$wasmArgs = @('build', $crate, '--target', 'web', '--release', '--out-dir', 'pkg', '--out-name', 'canvas_wasm')
if ($Backend -eq 'webgl') { $wasmArgs += @('--features', 'webgl') }

Write-Host ''
Write-Host 'building the renderer for wasm32-unknown-unknown...' -ForegroundColor Cyan
& $wasmPack @wasmArgs
if ($LASTEXITCODE -ne 0) { throw "wasm-pack build failed" }

# Reported because it is a result, not a detail: the download size is different
# for a local desktop file and a network-fetched hosted tier.
$wasm = Get-Item (Join-Path $crate 'pkg\canvas_wasm_bg.wasm')
Write-Host ("  wasm      {0:N0} bytes ({1:N2} MB)" -f $wasm.Length, ($wasm.Length / 1MB))
Write-Host ''

# Port 0 lets the OS pick, so a second run never collides with the first and no
# port is ever committed.
$serverLog = Join-Path $env:TEMP "dreamscape-web-bench-$PID.log"
# Same reason as the wasm arguments: a blank argument is worse than no argument.
$serveArgs = @((Join-Path $crate 'web\serve.js'), '0')
if ($ResultLog) { $serveArgs += (Join-Path (Get-Location) $ResultLog) }
$server = Start-Process -PassThru -NoNewWindow -FilePath $bun `
  -ArgumentList $serveArgs `
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
  # `@()` matters. A `foreach` that yields one item assigns a bare string, and
  # `$urls[0]` on a string is its first character, not the first element. So
  # `-Scenario quick`, the one-row case, tried to launch a program called "h".
  # The three-row `bench` case happened to work, which is what made this look
  # like a browser problem rather than an array problem.
  $urls = @(
    foreach ($n in $rows) {
      $motion = if ($Scenario -eq 'scale') { 'pan=0&zoom=0.96&frames=150' } else { 'pan=18&zoom=0.7&frames=200' }
      "$url/web/index.html?$common&items=$n&textures=$n&$motion"
    }
  )
  $interactive = "$url/web/index.html?atlas=$Atlas&backend=$Backend&width=$Width&height=$Height"

  if ($ResultLog) {
    "load before: $((Get-CimInstance Win32_Processor).LoadPercentage)% cpu" | Add-Content -Path $ResultLog
  }

  Write-Host 'browser canvas' -ForegroundColor Cyan
  Write-Host "  interactive  $interactive"
  foreach ($u in $urls) { Write-Host "  measure      $u" }
  Write-Host ''

  # The interactive page by default, since someone running this usually wants a
  # board they can drag around. `-Measure` opens a scenario instead, and that is
  # the only path that produces a captured result: the interactive page has no
  # frame count and posts nothing, so a run with `-ResultLog` and no `-Measure`
  # writes a load bracket and no number, which looks like a failure.
  if (-not $NoOpen) {
    if ($Measure) {
      Start-Process $urls[0]
      if ($urls.Count -gt 1) {
        Write-Host 'opened the first measure URL; the rest are one navigation away.' -ForegroundColor DarkGray
      }
    } else {
      Start-Process $interactive
    }
  }

  if ($ResultLog) {
    Write-Host "posting results to $ResultLog once each measure URL has run." -ForegroundColor DarkGray
    if (-not $Measure) {
      Write-Host 'note: the interactive page posts nothing. re-run with -Measure to capture a number.' -ForegroundColor DarkGray
    }
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
