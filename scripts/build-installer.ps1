# Build the Prism installer with a version stamped from the last commit.
#
# WHAT THIS DOES
#   1. Build all three binaries (Rust backend + WPF frontend) via prism-build.ps1.
#   2. Compute the short hash of HEAD (e.g. d543ae8) as the build's commit name.
#   3. Stamp the Inno Setup script with version 1.1.<short-hash> in place.
#   4. Copy the freshly built binaries + frontend support files into dist/.
#   5. Run ISCC.exe to produce dist\PrismSetup-1.1.<short-hash>.exe.
#
# WHY A SCRIPT
#   The installer version must carry the last commit so each build is
#   traceable, but stamping .iss by hand every time is the exact class of
#   manual step that rots. This script makes it automatic: the version is
#   always derived from the current HEAD, never hand-edited.
#
# USAGE
#   .\scripts\build-installer.ps1              # build everything, make installer
#   .\scripts\build-installer.ps1 -SkipBuild   # skip the cargo/dotnet build, just
#                                             # re-stamp + re-pack from existing
#                                             # target outputs (fast iteration)
#
# REQUIRES
#   ISCC.exe at D:\LS\Setup 7\ISCC.exe (as referenced in dist\prism.iss comment).
#   The Rust + dotnet + MSVC toolchain is located by prism-build.ps1.

[CmdletBinding()]
param(
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = Split-Path -Parent $PSScriptRoot
$DistDir     = Join-Path $RepoRoot 'dist'
$IssPath     = Join-Path $DistDir 'prism.iss'
$CoreOut     = Join-Path $RepoRoot 'src\prism-core\target\release'
$AppOut      = Join-Path $RepoRoot 'src\Prism\bin\Release\net8.0-windows'
$IsccPath    = 'D:\LS\Setup 7\ISCC.exe'
$VersionBase = '1.1'

function Write-Step([string]$text) {
    Write-Host ''
    Write-Host "==> $text" -ForegroundColor Cyan
}
function Write-Ok([string]$text)   { Write-Host "    OK   $text" -ForegroundColor Green }
function Write-Bad([string]$text)  { Write-Host "    FAIL $text" -ForegroundColor Red }

# --- 1. Build -------------------------------------------------------------

if (-not $SkipBuild) {
    Write-Step 'Building all binaries (prism-build.ps1 -SkipInstall)'
    & (Join-Path $PSScriptRoot 'prism-build.ps1') -SkipInstall
    if ($LASTEXITCODE -ne 0) { throw "prism-build.ps1 failed (exit $LASTEXITCODE)" }
    Write-Ok 'build done'
} else {
    Write-Step 'Skipping build (-SkipBuild); reusing existing target outputs'
}

# --- 2. Commit name -------------------------------------------------------

$shortHash = (git -C $RepoRoot rev-parse --short HEAD).Trim()
if ([string]::IsNullOrEmpty($shortHash)) { throw 'could not resolve HEAD short hash' }
$fullVersion = "$VersionBase.$shortHash"
Write-Step "Installer version = $fullVersion  (from HEAD $shortHash)"

# --- 3. Stamp .iss --------------------------------------------------------

# Rewrite the MyAppVersion define line with the derived version. The .iss file
# keeps a placeholder value in source control; this script overwrites it for the
# real build so the committed .iss never needs a manual edit between builds.
# The committed version is restored after ISCC finishes so git status stays clean.
Write-Step "Stamping $IssPath"
# Read the committed .iss from HEAD (not the on-disk one, which a prior failed
# run may have left stamped). git show prints raw bytes; capture as one string.
$originalContent = & git -C $RepoRoot show 'HEAD:dist/prism.iss' | Out-String
if ([string]::IsNullOrEmpty($originalContent)) { throw 'could not read committed dist/prism.iss from HEAD' }
$content = $originalContent
$pattern = '(?m)^#define MyAppVersion "[^"]*"'
if ($content -notmatch $pattern) { throw 'MyAppVersion define not found in prism.iss' }
$content = $content -replace $pattern, "#define MyAppVersion `"$fullVersion`""
# Inno Setup reads .iss as plain text; keep it UTF-8 without BOM to match the
# committed file and avoid a stray BOM confusing the line parser.
[System.IO.File]::WriteAllText($IssPath, $content, (New-Object System.Text.UTF8Encoding($false)))
Write-Ok "MyAppVersion -> $fullVersion"

# --- 4. Copy binaries into dist ------------------------------------------

Write-Step 'Copying freshly built binaries into dist'
$files = @{
    'prism-core.exe'              = Join-Path $CoreOut 'prism-core.exe'
    'prism-indexer-service.exe'   = Join-Path $CoreOut 'prism-indexer-service.exe'
    'Prism.exe'                   = Join-Path $AppOut  'Prism.exe'
    'Prism.dll'                   = Join-Path $AppOut  'Prism.dll'
    'Prism.runtimeconfig.json'    = Join-Path $AppOut  'Prism.runtimeconfig.json'
    'Prism.deps.json'             = Join-Path $AppOut  'Prism.deps.json'
}
foreach ($name in $files.Keys) {
    $src = $files[$name]
    $dst = Join-Path $DistDir $name
    if (-not (Test-Path $src)) { throw "build output missing: $src (run without -SkipBuild)" }
    Copy-Item -LiteralPath $src -Destination $dst -Force
    Write-Ok "$name"
}

# --- 5. Compile installer --------------------------------------------------

if (-not (Test-Path $IsccPath)) { throw "ISCC.exe not found at $IsccPath" }
Write-Step "Running ISCC"
Push-Location $DistDir
try {
    & $IsccPath 'prism.iss'
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
    # Restore the committed .iss so the working tree matches HEAD (git status clean).
    & git -C $RepoRoot checkout HEAD -- 'dist/prism.iss' 2>$null
    Write-Ok 'prism.iss restored to committed placeholder'
}

$output = Join-Path $DistDir "PrismSetup-$fullVersion.exe"
if (-not (Test-Path $output)) { throw "installer not produced: $output" }
$sizeMb = [math]::Round((Get-Item $output).Length / 1MB, 1)
Write-Host ''
Write-Host "Done: $output ($sizeMb MB)" -ForegroundColor Green
