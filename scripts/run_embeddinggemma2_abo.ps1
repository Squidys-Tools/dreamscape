<#
.SYNOPSIS
Runs the pinned EmbeddingGemma 2 ABO CPU evaluation.
.DESCRIPTION
Uses uv and an already-installed Python interpreter, including one already managed by uv. It does not download another Python version.
The virtual environment, uv cache, Hugging Face model cache, sample images, progress and results
are kept under .local-evaluations/embeddinggemma2-abo in the repository root.
.PARAMETER Part
Run just one resumable part. Without this parameter, all four parts run in order.
.EXAMPLE
.\scripts\run_embeddinggemma2_abo.ps1
.EXAMPLE
.\scripts\run_embeddinggemma2_abo.ps1 -Part main
#>
[CmdletBinding()]
param(
    [ValidateSet('main', 'alt', 'texts', 'assemble')]
    [string]$Part
)

$ProjectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$PackageRoot = Join-Path $PSScriptRoot 'embeddinggemma2-abo'
$WorkRoot = Join-Path $ProjectRoot '.local-evaluations\embeddinggemma2-abo'
$SampleRoot = Join-Path $WorkRoot 'abo-sample'
$Venv = Join-Path $WorkRoot '.venv'
$Python = Join-Path $Venv 'Scripts\python.exe'
$Runner = Join-Path $PackageRoot 'run_embeddinggemma2_abo.py'
$SourceManifest = Join-Path $PackageRoot 'manifest.jsonl'
$Manifest = Join-Path $SampleRoot 'manifest.jsonl'
$ExpectedManifestHash = 'a61f64fb245a23440b2a00c3eabac5cda7a493571b64a78b4629ba85146b91bd'
$Result = Join-Path $SampleRoot 'embeddinggemma2-abo-results-2026-10-13.json'

foreach ($RequiredCommand in @('uv')) {
    if (-not (Get-Command $RequiredCommand -ErrorAction SilentlyContinue)) {
        throw "Required command '$RequiredCommand' was not found on PATH. Install uv, then rerun this script."
    }
}

$EnvironmentNames = @('UV_CACHE_DIR', 'UV_PYTHON_DOWNLOADS', 'HF_HOME', 'ABO_SAMPLE_ROOT')
$PreviousEnvironment = @{}
foreach ($Name in $EnvironmentNames) {
    $PreviousEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
}

try {
    New-Item -ItemType Directory -Force -Path $WorkRoot, $SampleRoot, (Join-Path $SampleRoot 'images') -ErrorAction Stop | Out-Null
    $env:UV_CACHE_DIR = Join-Path $WorkRoot 'uv-cache'
    $env:UV_PYTHON_DOWNLOADS = 'never'
    $env:HF_HOME = Join-Path $WorkRoot 'huggingface'
    $env:ABO_SAMPLE_ROOT = $SampleRoot

    $UvInstallHelp = & uv pip install --help
    if ($LASTEXITCODE -ne 0 -or -not ($UvInstallHelp | Select-String -SimpleMatch '--torch-backend')) {
        throw 'This script needs a uv version with CPU backend support. Run "uv self update" (this updates uv, not Python) and rerun.'
    }

    foreach ($Path in @($Runner, $SourceManifest)) {
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "Required project input is missing: $Path"
        }
    }
    $ActualManifestHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $SourceManifest -ErrorAction Stop).Hash.ToLowerInvariant()
    if ($ActualManifestHash -ne $ExpectedManifestHash) {
        throw "Pinned manifest hash mismatch: $ActualManifestHash"
    }
    Copy-Item -LiteralPath $SourceManifest -Destination $Manifest -Force -ErrorAction Stop

    $NeedsVenv = -not (Test-Path -LiteralPath $Python -PathType Leaf)
    if (-not $NeedsVenv) {
        $VenvPythonVersion = (& $Python -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')").Trim()
        $NeedsVenv = ($LASTEXITCODE -ne 0) -or ($VenvPythonVersion -notmatch '^3\.(12|13|14)$')
    }
    if ($NeedsVenv) {
        $BasePython = (& uv python find '>=3.12,<3.15' --no-python-downloads).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $BasePython) {
            throw 'uv could not find an already-installed Python 3.12-3.14. Install one yourself; this script will not download Python.'
        }
        & uv venv --clear --force --python $BasePython --no-python-downloads $Venv
        if ($LASTEXITCODE -ne 0) { throw 'uv could not create the project-local virtual environment.' }
    }

    $Packages = @('torch==2.14.0+cpu', 'sentence-transformers==6.1.0', 'transformers==5.19.0', 'huggingface_hub==1.33.0', 'numpy==2.5.4', 'Pillow==12.3.0')
    & uv pip install --python $Python --torch-backend cpu @Packages
    if ($LASTEXITCODE -ne 0) { throw 'uv could not install the pinned CPU dependencies. The failure output above includes the cause.' }
    & uv pip check --python $Python
    if ($LASTEXITCODE -ne 0) { throw 'The project-local Python environment has dependency conflicts.' }

    $Downloader = @'
import concurrent.futures, json, os, pathlib, time, urllib.request
root = pathlib.Path(os.environ["ABO_SAMPLE_ROOT"])
rows = [json.loads(line) for line in (root / "manifest.jsonl").read_text(encoding="utf-8").splitlines() if line.strip()]
images = [image for row in rows for image in row["images"]]
if len(rows) != 300 or len(images) != 600:
    raise SystemExit(f"Unexpected sample size: {len(rows)} products, {len(images)} images")
image_dir = root / "images"
def fetch(image):
    target = image_dir / f"{image['image_id']}.jpg"
    if target.exists() and target.stat().st_size > 0:
        return
    request = urllib.request.Request(image["url"], headers={"User-Agent": "Mozilla/5.0"})
    for attempt in range(4):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                data = response.read()
            if not data.startswith(b"\xff\xd8\xff"):
                raise ValueError(f"Invalid JPEG response for {image['image_id']}")
            temporary = target.with_suffix(".jpg.part")
            temporary.write_bytes(data)
            temporary.replace(target)
            return
        except Exception:
            if attempt == 3:
                raise
            time.sleep(2 ** attempt)
with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
    list(pool.map(fetch, images))
print(f"Ready: {len(list(image_dir.glob('*.jpg')))} of {len(images)} images in {image_dir}")
'@
    $Downloader | & $Python -
    if ($LASTEXITCODE -ne 0) { throw 'Image download failed. Rerun the script to resume the download.' }

    $Parts = if ($PSBoundParameters.ContainsKey('Part')) { @($Part) } else { @('main', 'alt', 'texts', 'assemble') }
    foreach ($CurrentPart in $Parts) {
        Write-Host "Running EmbeddingGemma 2 part '$CurrentPart'..."
        & $Python $Runner --part $CurrentPart
        if ($LASTEXITCODE -ne 0) { throw "Part '$CurrentPart' failed. Rerun the script to resume from saved batches." }
    }

    if ($Parts -contains 'assemble') {
        if (-not (Test-Path -LiteralPath $Result -PathType Leaf)) {
            throw "The assemble step finished without producing the result file: $Result"
        }
        Write-Host "Evaluation complete. Result: $Result"
    }
}
finally {
    foreach ($Name in $EnvironmentNames) {
        if ($null -eq $PreviousEnvironment[$Name]) {
            Remove-Item "Env:$Name" -ErrorAction SilentlyContinue
        }
        else {
            Set-Item "Env:$Name" $PreviousEnvironment[$Name]
        }
    }
}
