<#
.SYNOPSIS
  Runs the canvas spike benchmark.

.DESCRIPTION
  Sweeps a set of scenarios and prints one row per scenario.

  Frame time is measured as CPU submit plus a hard device.poll(Wait), which
  serialises CPU and GPU. That is pessimistic against a real app that runs the
  CPU ahead, which is the right direction to be wrong in.

  Numbers are read from the bench binary's RESULT line, not scraped from the
  human-readable report, so the formatting can change without breaking this.

.PARAMETER Scenario
  quick  one fast scenario
  bench  (default) realistic zoomed views at 2k / 8k / 20k distinct images
  scale  item counts doubled repeatedly with everything on screen at once
#>
param(
  [ValidateSet('quick', 'bench', 'scale')]
  [string]$Scenario = 'bench'
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$BUDGET = 16.67

function Invoke-Scenario {
  param([string]$Label, [string[]]$Extra)

  $out = & cargo run -q -p canvas-harness --bin bench --release -- @Extra 2>&1
  if ($LASTEXITCODE -ne 0) { $out | Write-Host; throw "bench failed: $Label" }

  $line = $out | Select-String '^RESULT ' | Select-Object -Last 1
  if (-not $line) { $out | Write-Host; throw "no RESULT line from: $Label" }

  $kv = @{}
  foreach ($m in [regex]::Matches($line.Line, '(\w+)=([\w.]+)')) {
    $kv[$m.Groups[1].Value] = $m.Groups[2].Value
  }
  if ($kv.Count -eq 0) { throw "unparseable RESULT: $($line.Line)" }

  # A renamed or dropped field used to read as $null and print as 0, which made a
  # broken run look like a fast one. Fail loudly instead.
  foreach ($required in 'mean_ms', 'p99_ms', 'max_ms', 'over', 'frames', 'placeholders_pct', 'peak_visible') {
    if (-not $kv.ContainsKey($required)) { throw "RESULT is missing '$required': $($line.Line)" }
  }

  $mean = [double]$kv['mean_ms']
  [pscustomobject]@{
    Scenario    = $Label
    Visible     = [int]$kv['peak_visible']
    MeanMs      = [math]::Round($mean, 2)
    P99Ms       = [math]::Round([double]$kv['p99_ms'], 2)
    MaxMs       = [math]::Round([double]$kv['max_ms'], 2)
    OverBudget  = "$($kv['over'])/$($kv['frames'])"
    Placeholder = "$($kv['placeholders_pct'])%"
    # The decision rule is zero frames over budget, not a mean under budget.
    # Testing the mean here reported 20k items as 'ok' while a third of its
    # frames were over the threshold.
    Budget      = if ([int]$kv['over'] -eq 0) { 'ok' } else { 'OVER' }
  }
}

# Scenarios are objects, not nested arrays. PowerShell flattens nested arrays
# when they are stored in a variable, which silently passes the wrong arguments.
$realistic    = @('--pan=18', '--zoom=0.7', '--width=2560', '--height=1440', '--frames=200', '--atlas=2048')
$allOnScreen  = @('--pan=0', '--zoom=0.96', '--width=2560', '--height=1440', '--frames=150', '--atlas=2048')

$scenarios = switch ($Scenario) {
  'quick' {
    @( [pscustomobject]@{ Label = '2k items / 2k distinct'
                           Args  = $realistic + @('--items=2000', '--textures=2000') } )
  }
  'bench' {
    @(
      [pscustomobject]@{ Label = '2k items / 2k distinct'
                         Args  = $realistic + @('--items=2000',  '--textures=2000') },
      [pscustomobject]@{ Label = '8k items / 8k distinct'
                         Args  = $realistic + @('--items=8000',  '--textures=8000') },
      [pscustomobject]@{ Label = '20k items / 20k distinct'
                         Args  = $realistic + @('--items=20000', '--textures=20000') }
    )
  }
  'scale' {
    $s = @()
    foreach ($n in 2000, 4000, 8000, 16000, 32000) {
      $s += [pscustomobject]@{
        Label = "$n items / $n distinct, all on screen"
        Args  = $allOnScreen + @("--items=$n", "--textures=$n")
      }
    }
    $s
  }
}

Write-Host ''
Write-Host "canvas spike  |  budget ${BUDGET}ms  |  CPU submit + hard GPU wait" -ForegroundColor Cyan
Write-Host ''

$results = foreach ($s in $scenarios) { Invoke-Scenario -Label $s.Label -Extra $s.Args }
$results | Format-Table -AutoSize

$over = @($results | Where-Object { $_.Budget -eq 'OVER' })
$ph   = @($results | Where-Object { [double]($_.Placeholder -replace '%','') -gt 0.01 })

Write-Host ''
if ($over.Count) { Write-Host "$($over.Count) scenario(s) over budget:" -ForegroundColor Yellow; $over | ForEach-Object { Write-Host "  $($_.Scenario)  $($_.OverBudget) frames" -ForegroundColor Yellow } }
else            { Write-Host 'No frames over budget in any scenario.' -ForegroundColor Green }
if ($ph.Count)   { Write-Host "$($ph.Count) scenario(s) with placeholders: atlas too small for the visible set." -ForegroundColor Yellow }
else            { Write-Host 'No placeholders: every visible item has a real texture.' -ForegroundColor Green }
Write-Host ''
