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
#   -IsccPath <path>                           # override the ISCC location
#
# REQUIRES
#   ISCC.exe (auto-discovered from the registry install location; falls back
#   to D:\LS\Setup 7\ISCC.exe as referenced in dist\prism.iss comment).
#   The Rust + dotnet + MSVC toolchain is located by prism-build.ps1.

# L29 (quanguo fushi 2026-08-22): -IsccPath injectable; auto-discovery first.
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$IsccPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = Split-Path -Parent $PSScriptRoot
$DistDir     = Join-Path $RepoRoot 'dist'
$IssPath     = Join-Path $DistDir 'prism.iss'
$CoreOut     = Join-Path $RepoRoot 'src\prism-core\target\release'
$AppOut      = Join-Path $RepoRoot 'src\Prism\bin\Release\net8.0-windows'
if ([string]::IsNullOrEmpty($IsccPath)) {
    $IsccPath = 'D:\LS\Setup 7\ISCC.exe'
    # StrictMode: property may be absent -- probe PSObject.Properties first.
    $regKey = Get-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup 6_is1' -ErrorAction SilentlyContinue
    $regLoc = $null
    if ($regKey -and $regKey.PSObject.Properties['InstallLocation']) {
        $regLoc = [string]$regKey.InstallLocation
    }
    if ($regLoc) {
        $candidate = Join-Path $regLoc 'ISCC.exe'
        if (Test-Path $candidate) { $IsccPath = $candidate }
    }
}
$VersionBase = '1.1'

function Write-Step([string]$text) {
    Write-Host ''
    Write-Host "==> $text" -ForegroundColor Cyan
}
function Write-Ok([string]$text)   { Write-Host "    OK   $text" -ForegroundColor Green }
function Write-Bad([string]$text)  { Write-Host "    FAIL $text" -ForegroundColor Red }

# --- 1. Build -------------------------------------------------------------

# L30 (quanguo fushi 2026-08-22): try/catch instead of $LASTEXITCODE alone --
# prism-build reports failures via `throw` and ends success with `exit 0`,
# so $LASTEXITCODE stays 0 under `&` invocation and the old guard could not
# catch a failed build. Terminating errors abort this script under
# $ErrorActionPreference='Stop'; the exit-code check stays as belt-and-braces.
if (-not $SkipBuild) {
    Write-Step 'Building all binaries (prism-build.ps1 -SkipInstall)'
    try {
        & (Join-Path $PSScriptRoot 'prism-build.ps1') -SkipInstall
        if ($LASTEXITCODE -ne 0) { throw "prism-build.ps1 failed (exit $LASTEXITCODE)" }
    } catch {
        throw "prism-build.ps1 failed: $($_.Exception.Message)"
    }
    Write-Ok 'build done'
} else {
    Write-Step 'Skipping build (-SkipBuild); reusing existing target outputs'
}

# --- 2. Commit name -------------------------------------------------------

# 2026-08-24 全仓检验（M2）：版本号钉的是 HEAD，但 cargo/dotnet 编的是工作区——
# 带未提交改动出包会得到「版本指向 A 提交、字节却不存在于任何提交」的不可追溯
# 产物。src/ 的任何状态变化（含未跟踪新源文件，它们同样参与编译）与 dist/
# 已跟踪产物的改动都必须先提交。dist/ 的未跟踪文件（上一次的 PrismSetup-*.exe、
# data/、Output/）不参与编译，不算脏。
$dirtyTracked = (git -C $RepoRoot status --porcelain --untracked-files=no -- src dist) -join "`n"
$dirtySrcAll  = (git -C $RepoRoot status --porcelain -- src) -join "`n"
if ($dirtyTracked -or $dirtySrcAll) {
    Write-Bad 'Working tree is dirty; the installer would ship bytes that match no commit:'
    if ($dirtyTracked) { Write-Host $dirtyTracked -ForegroundColor Yellow }
    if ($dirtySrcAll)  { Write-Host $dirtySrcAll -ForegroundColor Yellow }
    throw 'commit or stash the changes above first, then rerun'
}

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
# 2026-08-24 全仓检验（M3）：盲 checkout 会无警告销毁未提交的 prism.iss 编辑
#（本文件最常被改的东西）。先确认工作区副本与 HEAD 一致；不一致就中止，
# 让用户自己决定保留还是丢弃——上一轮 stamp 残留同样会走到这里，按提示
# `git checkout -- dist/prism.iss` 清掉即可。
& git -C $RepoRoot diff --quiet HEAD -- 'dist/prism.iss'
if ($LASTEXITCODE -ne 0) {
    throw 'dist/prism.iss has uncommitted edits; commit them, or discard with: git checkout -- dist/prism.iss'
}
# Read on-disk UTF-8 (NOT `git show | Out-String`, which re-encodes Chinese via
# the console code page and corrupts quoted values for ISCC).
$content = Get-Content -LiteralPath $IssPath -Raw -Encoding UTF8
$pattern = '(?m)^#define MyAppVersion "[^"]*"'
if ($content -notmatch $pattern) { throw 'MyAppVersion define not found in prism.iss' }
$content = $content -replace $pattern, "#define MyAppVersion `"$fullVersion`""
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
    # L29: a failed restore in finally must be visible (no throw here -- it would
    # mask the original exception); print FAIL and leave the file for manual fix.
    & git -C $RepoRoot checkout HEAD -- 'dist/prism.iss' 2>$null
    if ($LASTEXITCODE -ne 0) {
        Write-Bad 'git checkout dist/prism.iss failed in finally -- working tree may hold a stamped .iss; restore manually'
    } else {
        Write-Ok 'prism.iss restored to committed placeholder'
    }
}

$output = Join-Path $DistDir "PrismSetup-$fullVersion.exe"
if (-not (Test-Path $output)) { throw "installer not produced: $output" }
$sizeMb = [math]::Round((Get-Item $output).Length / 1MB, 1)

# 2026-08-24 全仓检验（M6）：清掉旧版安装包——dist 里的 PrismSetup-*.exe 会被
# 整体提交（既有工作流），不清就只能靠人肉记着删（cf. 68d9897 手工清理），
# 多留一代就多一个「装到旧代码」的入口。每轮构建后 dist 恰好只剩当前版本。
$stale = @(Get-ChildItem -LiteralPath $DistDir -Filter 'PrismSetup-*.exe' |
    Where-Object { $_.Name -ne "PrismSetup-$fullVersion.exe" })
if ($stale.Count -gt 0) {
    $stale | Remove-Item -Force
    Write-Ok ("removed {0} stale installer(s): {1}" -f $stale.Count, (($stale | ForEach-Object Name) -join ', '))
}

Write-Host ''
Write-Host "Done: $output ($sizeMb MB)" -ForegroundColor Green
