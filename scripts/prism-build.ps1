# Prism build + install, in one step, with hash verification.
#
# WHY THIS EXISTS
# ---------------
# During G4 a bug was chased for hours because the running code was not the
# code being edited. Three separate copies of the backend existed at once:
#
#   src/prism-core/target/debug/prism-core.exe    Aug 5  <- what the app loaded
#   src/prism-core/target/release/prism-core.exe  Aug 6  <- where edits went
#   C:\Program Files\Prism\prism-core.exe         Aug 1  <- what was installed
#
# Nothing reported a mismatch. Builds silently no-op'd ("Finished in 0.37s"),
# and the frontend preferred target\debug unconditionally, so a stale binary
# masked every change. This script makes that class of failure impossible:
# it builds from source, installs, then verifies by SHA-256 that the bytes on
# disk match the bytes just built. Any drift is a hard error.
#
# ASCII only on purpose: PowerShell 5.1 under a Chinese locale reads BOM-less
# UTF-8 scripts as GBK, which corrupts non-ASCII literals and breaks parsing.
#
# USAGE
#   .\scripts\prism-build.ps1              # build + install + verify (needs admin)
#   .\scripts\prism-build.ps1 -Bootstrap   # same, on a machine with no install yet
#   .\scripts\prism-build.ps1 -VerifyOnly  # just check for drift, changes nothing
#   .\scripts\prism-build.ps1 -SkipInstall # build + verify build outputs only
#   .\scripts\prism-build.ps1 -Clean       # force full rebuild first
#
# TOOLCHAIN
#   Located automatically by Initialize-Toolchain; install with winget if absent:
#     winget install --id Microsoft.DotNet.SDK.8
#     winget install --id Microsoft.VisualStudio.2022.BuildTools --override
#       "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
#   The MSVC linker is needed even for pure Rust. Without it rustc uses whatever
#   `link` is on PATH - Git ships a coreutils one - and fails with
#   "link: extra operand". Library-only builds still succeed, so the breakage only
#   surfaces when producing an .exe, which lets it hide for a long time.
#   From Git Bash, `source scripts/msvc-env.sh` sets up cargo/dotnet/link the same way.

[CmdletBinding()]
param(
    [switch]$VerifyOnly,
    [switch]$SkipInstall,
    [switch]$Clean,
    # Create the install directory and register the service when they are absent
    # (fresh machine / after an OS reinstall).
    [switch]$Bootstrap
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = Split-Path -Parent $PSScriptRoot
$CoreManifest = Join-Path $RepoRoot 'src\prism-core\Cargo.toml'
$CoreOut     = Join-Path $RepoRoot 'src\prism-core\target\release'
$CoreDebug   = Join-Path $RepoRoot 'src\prism-core\target\debug'
$AppProject  = Join-Path $RepoRoot 'src\Prism\Prism.csproj'
$AppOut      = Join-Path $RepoRoot 'src\Prism\bin\Release\net8.0-windows'
$ServiceName = 'PrismIndexer'
# L28（全仓复审 2026-08-22）：安装目录优先取现有服务注册路径——安装器允许
# 自选目录（本机装在 D:\LS\Prism），硬编码 C:\Program Files\Prism 会让
# -Bootstrap 在 C 盘造出第二份安装并把服务指过去，而真实安装与自启项
# 仍指着旧盘。服务不存在（全新机器）时保持默认。
$InstallDir  = 'C:\Program Files\Prism'
$svcRegistered = Get-CimInstance Win32_Service -Filter "Name='$ServiceName'" -ErrorAction SilentlyContinue
if ($null -ne $svcRegistered -and $svcRegistered.PathName) {
    if ($svcRegistered.PathName -match '^"([^"]+\\)[^"\\]+\.exe"') {
        $InstallDir = $Matches[1].TrimEnd('\')
    } elseif ($svcRegistered.PathName -match '^(.*?\\)[^"\\]+\.exe(?:\s|$)') {
        $InstallDir = $Matches[1].TrimEnd('\')
    }
}

# name -> @{ Built = <path>; Installed = <path> }
$Artifacts = [ordered]@{
    'prism-core.exe' = @{
        Built     = Join-Path $CoreOut 'prism-core.exe'
        Installed = Join-Path $InstallDir 'prism-core.exe'
    }
    'prism-indexer-service.exe' = @{
        Built     = Join-Path $CoreOut 'prism-indexer-service.exe'
        Installed = Join-Path $InstallDir 'prism-indexer-service.exe'
    }
    'Prism.exe' = @{
        Built     = Join-Path $AppOut 'Prism.exe'
        Installed = Join-Path $InstallDir 'Prism.exe'
    }
}

# cargo and dotnet write progress to stderr. With $ErrorActionPreference='Stop'
# PowerShell turns any native stderr line into a terminating NativeCommandError,
# so a perfectly healthy build blows up. Run externals with stderr merged into
# stdout and judge success solely by the exit code.
function Invoke-Native([string]$what, [scriptblock]$command) {
    $global:LASTEXITCODE = 0
    & {
        $ErrorActionPreference = 'Continue'
        & $command 2>&1 | ForEach-Object { Write-Host "    $_" }
    }
    if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" }
}

function Write-Step([string]$text) {
    Write-Host ''
    Write-Host "==> $text" -ForegroundColor Cyan
}

function Write-Ok([string]$text)   { Write-Host "    OK   $text" -ForegroundColor Green }
function Write-Warn2([string]$text) { Write-Host "    WARN $text" -ForegroundColor Yellow }
function Write-Bad([string]$text)  { Write-Host "    FAIL $text" -ForegroundColor Red }

# A PowerShell 7 install can inject its Modules directory into PSModulePath,
# shadowing 5.1's built-in Microsoft.PowerShell.Utility so that Get-FileHash is
# reported as "not recognized". Importing explicitly restores it; without this
# the whole verification step fails on an otherwise healthy machine.
Import-Module Microsoft.PowerShell.Utility -ErrorAction SilentlyContinue

function Get-Sha([string]$path) {
    if (-not (Test-Path $path)) { return $null }
    if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) {
        return (Get-FileHash -Path $path -Algorithm SHA256).Hash
    }
    # Last resort: certutil is always present and needs no modules.
    # L26（全仓复审 2026-08-22）：解析结果必须是 64 位十六进制——certutil 输出
    # 布局随区域设置变化时，按位置取行可能拿到本地化文案，哈希比较静默失配
    # （好在会用 Substring 抛错，但那是碰运气）。显式校验，坏输入立刻报真错。
    $line = (& certutil.exe -hashfile $path SHA256 | Select-Object -Skip 1 -First 1)
    $hash = ($line -replace '\s', '').ToUpperInvariant()
    if ($hash -notmatch '^[0-9A-F]{64}$') {
        throw "certutil hash parse failed for ${path}: $line"
    }
    return $hash
}

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole(
        [Security.Principal.WindowsBuiltinRole]::Administrator)
}

# ---------------------------------------------------------------- build ----

# A running Prism.exe locks its own build output, so dotnet build fails with
# MSB3027 ("being used by another process"). Must happen BEFORE building, not
# just before copying: otherwise the build dies and, worse, a partially written
# output could look like a success.
function Stop-PrismProcesses {
    $stopped = @()
    foreach ($procName in @('Prism', 'prism-core')) {
        $running = Get-Process -Name $procName -ErrorAction SilentlyContinue
        if ($null -ne $running) {
            $running | Stop-Process -Force
            $stopped += $procName
        }
    }
    if ($stopped.Count -gt 0) {
        Start-Sleep -Milliseconds 800
        Write-Ok "stopped: $($stopped -join ', ')"
    } else {
        Write-Ok 'no Prism processes running'
    }
}

# Make cargo, dotnet and the MSVC linker reachable regardless of how this shell
# was started. An elevated PowerShell often lacks the user's PATH entries, and
# Git Bash puts its own coreutils `link` ahead of MSVC's link.exe - which makes
# rustc fail with "link: extra operand" only when producing an .exe.
function Initialize-Toolchain {
    Write-Step 'Locating toolchain'

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
        if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
            $env:PATH = "$cargoBin;$env:PATH"
        } else {
            throw 'cargo not found (install Rust via rustup)'
        }
    }
    Write-Ok "cargo  $((Get-Command cargo).Source)"

    if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
        $dotnetDir = 'C:\Program Files\dotnet'
        if (Test-Path (Join-Path $dotnetDir 'dotnet.exe')) {
            $env:PATH = "$dotnetDir;$env:PATH"
        } else {
            throw 'dotnet not found (winget install --id Microsoft.DotNet.SDK.8)'
        }
    }
    Write-Ok "dotnet $((Get-Command dotnet).Source)"

    # Prepend MSVC so its link.exe wins over any coreutils `link` on PATH.
    $vsRoot = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools'
    $msvcRoot = Join-Path $vsRoot 'VC\Tools\MSVC'
    if (-not (Test-Path $msvcRoot)) {
        throw "MSVC toolset not found under $msvcRoot (winget install --id Microsoft.VisualStudio.2022.BuildTools --override `"--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended`")"
    }
    $msvcVer = (Get-ChildItem $msvcRoot -Directory | Sort-Object Name -Descending | Select-Object -First 1).Name
    $msvcBin = Join-Path $msvcRoot "$msvcVer\bin\Hostx64\x64"
    if (-not (Test-Path (Join-Path $msvcBin 'link.exe'))) {
        throw "link.exe not found in $msvcBin"
    }
    $env:PATH = "$msvcBin;$env:PATH"

    # Highest SDK that actually ships the x64 import libraries we link against.
    $kitsLib = 'C:\Program Files (x86)\Windows Kits\10\Lib'
    $sdkVer = Get-ChildItem $kitsLib -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        Where-Object { Test-Path (Join-Path $_.FullName 'um\x64\kernel32.lib') } |
        Select-Object -First 1
    if ($null -eq $sdkVer) { throw "no Windows SDK with um\x64\kernel32.lib under $kitsLib" }

    $env:LIB = @(
        (Join-Path $msvcRoot "$msvcVer\lib\x64")
        (Join-Path $sdkVer.FullName 'ucrt\x64')
        (Join-Path $sdkVer.FullName 'um\x64')
    ) -join ';'
    Write-Ok "MSVC   $msvcVer / SDK $($sdkVer.Name)"
}

function Invoke-Build {
    Initialize-Toolchain

    Write-Step 'Releasing file locks'
    Stop-PrismProcesses

    if ($Clean) {
        Write-Step 'Cleaning previous build output'
        Invoke-Native 'cargo clean' { cargo clean --manifest-path $CoreManifest --release }
        Remove-Item -Recurse -Force (Join-Path $RepoRoot 'src\Prism\obj\Release') -ErrorAction SilentlyContinue
        Remove-Item -Recurse -Force (Join-Path $RepoRoot 'src\Prism\bin\Release') -ErrorAction SilentlyContinue
        Write-Ok 'clean done'
    }

    Write-Step 'Building Rust backend (release)'
    Invoke-Native 'cargo build' { cargo build --manifest-path $CoreManifest --release }
    Write-Ok 'cargo build done'

    Write-Step 'Building WPF frontend (Release)'
    Invoke-Native 'dotnet build' { dotnet build $AppProject -c Release --nologo -v minimal }
    Write-Ok 'dotnet build done'

    Assert-OutputsFresh
}

# A no-op build is the exact failure this script exists to prevent: cargo and
# dotnet happily print "Finished in 0.37s" / "up to date" and exit 0 while
# leaving a stale binary in place. Existence is not enough - the output must be
# at least as new as the newest source file feeding it.
function Assert-OutputsFresh {
    Write-Step 'Checking build outputs are newer than sources'

    $sourceSets = @{
        'prism-core.exe'            = @((Join-Path $RepoRoot 'src\prism-core\src'), (Join-Path $RepoRoot 'src\prism-core\Cargo.toml'))
        'prism-indexer-service.exe' = @((Join-Path $RepoRoot 'src\prism-core\src'), (Join-Path $RepoRoot 'src\prism-core\Cargo.toml'))
        'Prism.exe'                 = @((Join-Path $RepoRoot 'src\Prism'))
    }

    $missing = @()
    $stale   = @()
    foreach ($name in $Artifacts.Keys) {
        $path = $Artifacts[$name].Built
        if (-not (Test-Path $path)) {
            Write-Bad "$name missing at $path"
            $missing += $name
            continue
        }

        $builtAt  = (Get-Item $path).LastWriteTime
        $newestSrc = Get-NewestSourceTime $sourceSets[$name]

        # L25（全仓复审 2026-08-22）：newestSrc 为空必须报错——排除规则按
        # 「路径相对仓库根」匹配后，唯一剩下的空集成因是源路径本身配错
        #（或仓库被克隆进名字含 bin/obj/target 的目录）。原先 null 直接
        # 跳过检查，任意陈旧二进制都报 OK。
        if ($null -eq $newestSrc) {
            Write-Bad "$name found no source files (source set misconfigured?)"
            $stale += $name
        } elseif ($builtAt -lt $newestSrc) {
            Write-Bad "$name is OLDER than its sources"
            Write-Host "         built  $($builtAt.ToString('MM-dd HH:mm:ss'))" -ForegroundColor Red
            Write-Host "         source $($newestSrc.ToString('MM-dd HH:mm:ss'))" -ForegroundColor Red
            $stale += $name
        } else {
            Write-Ok "$name  ($($builtAt.ToString('yyyy-MM-dd HH:mm:ss')))"
        }
    }

    if ($missing.Count -gt 0) { throw "build produced no output for: $($missing -join ', ')" }
    if ($stale.Count -gt 0) {
        throw "build silently did nothing for: $($stale -join ', ') - rerun with -Clean"
    }
}

# Newest LastWriteTime across the given files/directories, ignoring build
# output dirs so bin/obj/target do not mask a genuinely stale artifact.
# L25：排除匹配用「相对仓库根」的路径——原先在 FullName 上匹配 \\(bin|obj|target)\\，
# 仓库克隆进含这些词的目录（如 D:\obj\listary）时所有源文件都被排除，
# $newest 恒为 null，新鲜度门整体短路。
function Get-NewestSourceTime([string[]]$paths) {
    $newest = $null
    $rootPrefix = $RepoRoot.TrimEnd('\') + '\'
    foreach ($path in $paths) {
        if (-not (Test-Path $path)) { continue }
        $item = Get-Item $path
        if (-not $item.PSIsContainer) {
            if ($null -eq $newest -or $item.LastWriteTime -gt $newest) { $newest = $item.LastWriteTime }
            continue
        }
        Get-ChildItem -Path $path -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object {
                $rel = $_.FullName
                if ($rel.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
                    $rel = $rel.Substring($rootPrefix.Length)
                }
                $rel -notmatch '(^|\\)(bin|obj|target)(\\|$)'
            } |
            ForEach-Object {
                if ($null -eq $newest -or $_.LastWriteTime -gt $newest) { $newest = $_.LastWriteTime }
            }
    }
    return $newest
}

# ------------------------------------------------------- stale debug ----

# The frontend now prefers the profile it was built with, but a leftover debug
# broker is still a trap for anyone running a Debug build of the app.
function Test-StaleDebug {
    $debugExe = Join-Path $CoreDebug 'prism-core.exe'
    if (-not (Test-Path $debugExe)) { return }

    $debugTime   = (Get-Item $debugExe).LastWriteTime
    $releaseExe  = $Artifacts['prism-core.exe'].Built
    if (-not (Test-Path $releaseExe)) { return }
    $releaseTime = (Get-Item $releaseExe).LastWriteTime

    if ($debugTime -lt $releaseTime) {
        Write-Warn2 "target\debug\prism-core.exe is older than release ($($debugTime.ToString('MM-dd HH:mm')) vs $($releaseTime.ToString('MM-dd HH:mm')))"
        Write-Warn2 'A Debug build of Prism.exe would load that stale broker.'
        Write-Warn2 'Remove it with:  cargo clean --manifest-path src\prism-core\Cargo.toml'
    }
}

# -------------------------------------------------------------- install ----

function Invoke-Install {
    if (-not (Test-Admin)) {
        Write-Bad 'Installing needs an elevated shell.'
        Write-Host ''
        Write-Host '    Right-click PowerShell -> Run as administrator, then:' -ForegroundColor Yellow
        Write-Host "      cd $RepoRoot" -ForegroundColor Yellow
        Write-Host '      .\scripts\prism-build.ps1' -ForegroundColor Yellow
        throw 'not elevated'
    }

    # A wiped machine (OS reinstall) has neither the directory nor the service.
    # Refusing to run there would defeat the point of this script, so -Bootstrap
    # creates both. Without the switch we still refuse, to avoid silently
    # scattering binaries on a machine that was never meant to host them.
    if (-not (Test-Path $InstallDir)) {
        if (-not $Bootstrap) {
            throw "install dir not found: $InstallDir (pass -Bootstrap to create it and register the service)"
        }
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Write-Ok "created $InstallDir"
    }

    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue

    Write-Step 'Stopping service and running processes'
    if ($null -ne $svc -and $svc.Status -ne 'Stopped') {
        Stop-Service -Name $ServiceName -Force
        # Stop-Service returns before the process actually exits; the file stays
        # locked meanwhile and Copy-Item would fail (or silently no-op earlier).
        $waited = 0
        while ((Get-Service -Name $ServiceName).Status -ne 'Stopped' -and $waited -lt 30) {
            Start-Sleep -Milliseconds 500
            $waited++
        }
        Write-Ok "service stopped"
    } else {
        Write-Ok 'service already stopped (or not installed)'
    }

    Stop-PrismProcesses

    # The service host may linger even after the service reports Stopped.
    $orphan = Get-Process -Name 'prism-indexer-service' -ErrorAction SilentlyContinue
    if ($null -ne $orphan) {
        $orphan | Stop-Process -Force
        Start-Sleep -Milliseconds 500
        Write-Ok 'stopped orphaned prism-indexer-service'
    }

    Write-Step 'Copying binaries'
    foreach ($name in $Artifacts.Keys) {
        $src = $Artifacts[$name].Built
        $dst = $Artifacts[$name].Installed
        Copy-Item -Path $src -Destination $dst -Force
        Write-Ok "$name -> $dst"
    }

    # The frontend also needs its managed assemblies, not just the exe.
    Write-Step 'Copying frontend assemblies'
    $copied = 0
    Get-ChildItem -Path $AppOut -Filter '*.dll' | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination (Join-Path $InstallDir $_.Name) -Force
        $copied++
    }
    foreach ($extra in @('Prism.runtimeconfig.json', 'Prism.deps.json')) {
        $path = Join-Path $AppOut $extra
        if (Test-Path $path) {
            Copy-Item -Path $path -Destination (Join-Path $InstallDir $extra) -Force
            $copied++
        }
    }
    Write-Ok "$copied support files"

    Write-Step 'Starting service'
    if ($null -eq $svc -and $Bootstrap) {
        # Matches the original installer's registration: own-process, auto-start,
        # LocalSystem. Quoting the path matters - "Program Files" contains a space
        # and sc.exe would otherwise treat the tail as arguments.
        $binPath = '"' + $Artifacts['prism-indexer-service.exe'].Installed + '"'
        & sc.exe create $ServiceName binPath= $binPath start= auto DisplayName= 'Prism Indexer' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "sc.exe create failed (exit $LASTEXITCODE)" }
        & sc.exe description $ServiceName 'Maintains the Prism file index.' | Out-Null
        Write-Ok "registered service $ServiceName"
        $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    }

    if ($null -ne $svc) {
        Start-Service -Name $ServiceName
        $status = (Get-Service -Name $ServiceName).Status
        if ($status -ne 'Running') { throw "service did not start (status: $status)" }
        Write-Ok "service running"
    } else {
        Write-Warn2 "service $ServiceName is not installed; skipped start (use -Bootstrap to register)"
    }
}

# --------------------------------------------------------------- verify ----

function Invoke-Verify {
    Write-Step 'Verifying installed bytes match built bytes (SHA-256)'

    $drift = @()
    foreach ($name in $Artifacts.Keys) {
        $builtPath     = $Artifacts[$name].Built
        $installedPath = $Artifacts[$name].Installed

        $builtHash     = Get-Sha $builtPath
        $installedHash = Get-Sha $installedPath

        if ($null -eq $builtHash) {
            Write-Warn2 "$name not built yet - skipped"
            continue
        }
        if ($null -eq $installedHash) {
            Write-Bad "$name is not installed"
            $drift += $name
            continue
        }

        if ($builtHash -eq $installedHash) {
            Write-Ok "$name  $($builtHash.Substring(0,12))"
        } else {
            Write-Bad "$name DIFFERS"
            Write-Host "         built     $($builtHash.Substring(0,12))  $((Get-Item $builtPath).LastWriteTime.ToString('MM-dd HH:mm'))" -ForegroundColor Red
            Write-Host "         installed $($installedHash.Substring(0,12))  $((Get-Item $installedPath).LastWriteTime.ToString('MM-dd HH:mm'))" -ForegroundColor Red
            $drift += $name
        }
    }

    # Cross-check that the service really points at the file we verified.
    $svcWmi = Get-CimInstance Win32_Service -Filter "Name='$ServiceName'" -ErrorAction SilentlyContinue
    if ($null -ne $svcWmi) {
        # L27（全仓复审 2026-08-22）：先剥出可执行路径再比对——原先只 Trim('"')
        # 后整串 -ieq，注册路径带参数（"…exe" -flag）或未加引号含空格时必报
        # 假 drift 并 exit 1。解析规则与 Bench.Common.psm1 的同款。
        $rawPath = [string]$svcWmi.PathName
        $svcPath = if ($rawPath -match '^"([^"]+\.exe)"') { $Matches[1] }
                   elseif ($rawPath -match '^(.*?\.exe)(?:\s|$)') { $Matches[1] }
                   else { $rawPath.Trim('"') }
        $expected = $Artifacts['prism-indexer-service.exe'].Installed
        if ($svcPath -ieq $expected) {
            Write-Ok "service binary path matches"
        } else {
            Write-Bad "service points at $svcPath, expected $expected"
            $drift += 'service path'
        }
    }

    # L36（全仓复审 2026-08-22）：dist/ 的追踪二进制也纳入比对——它们是安装包
    # 的直接来源，且 git 里追踪（曾让一份陈旧副本盖住新构建好几天）。
    # Invoke-Verify 从来只比 target/release 与安装目录，dist/ 无人看守。
    $distDir = Join-Path $RepoRoot 'dist'
    foreach ($name in @('prism-core.exe', 'prism-indexer-service.exe', 'Prism.exe', 'Prism.dll')) {
        $distPath = Join-Path $distDir $name
        $builtPath = if ($name -eq 'Prism.dll') { Join-Path $AppOut 'Prism.dll' }
                     else { $Artifacts[$name].Built }
        $builtHash = Get-Sha $builtPath
        if ($null -eq $builtHash) { continue }
        $distHash = Get-Sha $distPath
        if ($null -eq $distHash) {
            Write-Warn2 "dist\$name missing (run scripts\build-installer.ps1 to refresh dist)"
        } elseif ($distHash -ne $builtHash) {
            Write-Bad "dist\$name DIFFERS from build (stale dist copy would ship)"
            $drift += "dist\$name"
        } else {
            Write-Ok "dist\$name  $($distHash.Substring(0,12))"
        }
    }

    Test-StaleDebug

    Write-Host ''
    if ($drift.Count -gt 0) {
        Write-Host "DRIFT DETECTED: $($drift -join ', ')" -ForegroundColor Red
        Write-Host 'The running code is NOT the code you just built.' -ForegroundColor Red
        Write-Host 'Fix with:  .\scripts\prism-build.ps1   (in an elevated shell)' -ForegroundColor Yellow
        return $false
    }

    Write-Host 'All good: installed binaries match the current build.' -ForegroundColor Green
    return $true
}

# ----------------------------------------------------------------- main ----

Write-Host "Prism build/install  ($RepoRoot)" -ForegroundColor White

if ($VerifyOnly) {
    # Answers both drift questions without changing anything:
    #   1. is the build current with the sources?   (Assert-OutputsFresh)
    #   2. does what is installed match the build?  (Invoke-Verify)
    $fresh = $true
    try { Assert-OutputsFresh } catch { Write-Bad $_.Exception.Message; $fresh = $false }
    $ok = Invoke-Verify
    if (-not $fresh -or -not $ok) { exit 1 }
    exit 0
}

Invoke-Build

if ($SkipInstall) {
    Write-Step 'Skipping install (-SkipInstall)'
    Test-StaleDebug
    Write-Host ''
    Write-Host 'Build complete. Nothing was installed.' -ForegroundColor Green
    exit 0
}

Invoke-Install
$ok = Invoke-Verify
if (-not $ok) { exit 1 }

Write-Host ''
Write-Host 'Done. Launch Prism from the Start menu or:' -ForegroundColor White
Write-Host "  & '$InstallDir\Prism.exe'" -ForegroundColor White
exit 0
