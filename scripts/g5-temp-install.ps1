# G5 end-to-end probe: install Prism to a deliberately hostile temp path.
#
# ASCII-only source on purpose (see prism-build.ps1): PowerShell 5.1 under a
# Chinese locale reads BOM-less UTF-8 as GBK and corrupts non-ASCII literals.
# The target directory name therefore contains non-ASCII characters that are
# built from char codes rather than written literally.
#
# Deliberately does NOT touch C:\Program Files\Prism or the PrismIndexer
# service. G5 window enumeration lives entirely in the broker, so only
# prism-core.exe + Prism.exe are needed. File search will be unavailable in
# this sandbox, which is expected and irrelevant to window mode.
#
# The path exercises four separate hazards at once:
#   spaces      - naive quote concatenation breaks
#   non-ASCII   - GBK/UTF-8 confusion, W-vs-A API mistakes
#   parentheses - special to PowerShell parsing
#   ampersand   - cmd.exe treats it as a command separator; the project spec
#                 forbids `cmd /c` concatenation, so this verifies that rule
#
# USAGE
#   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\g5-temp-install.ps1
#   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\g5-temp-install.ps1 -Remove

[CmdletBinding()]
param([switch]$Remove)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$CoreOut  = Join-Path $RepoRoot 'src\prism-core\target\release'
$AppOut   = Join-Path $RepoRoot 'src\Prism\bin\Release\net8.0-windows'

# "prism <ce4b><8bd5> (G5) & <8def><5f84>"  == "prism 测试 (G5) & 路径"
$dirName = 'prism ' +
    [char]0x6D4B + [char]0x8BD5 +
    ' (G5) & ' +
    [char]0x8DEF + [char]0x5F84
$InstallDir = Join-Path 'D:\' $dirName

# Get-FileHash is absent in this environment despite PS 5.1, so fall back to certutil
# exactly as prism-build.ps1 does. certutil receives the path as a single argument, which
# also proves the ampersand is never handed to a shell for parsing.
function Get-Sha([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) {
        return (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    }
    $line = (& certutil.exe -hashfile $path SHA256 | Select-Object -Skip 1 -First 1)
    return ($line -replace '\s', '').ToUpperInvariant()
}

Write-Host "target: $InstallDir"

if ($Remove) {
    if (Test-Path -LiteralPath $InstallDir) {
        Get-Process -Name 'Prism', 'prism-core' -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, 'OrdinalIgnoreCase') } |
            ForEach-Object { Write-Host "stopping $($_.Name) ($($_.Id))"; $_.Kill(); $_.WaitForExit(5000) }
        Remove-Item -LiteralPath $InstallDir -Recurse -Force
        Write-Host 'removed'
    } else {
        Write-Host 'nothing to remove'
    }
    return
}

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

# -LiteralPath everywhere: the ampersand and parentheses must never be parsed.
$copied = 0
foreach ($exe in @('prism-core.exe')) {
    Copy-Item -LiteralPath (Join-Path $CoreOut $exe) -Destination (Join-Path $InstallDir $exe) -Force
    $copied++
}
foreach ($item in Get-ChildItem -LiteralPath $AppOut -File) {
    if ($item.Extension -in @('.exe', '.dll', '.json')) {
        Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $InstallDir $item.Name) -Force
        $copied++
    }
}
Write-Host "copied $copied file(s)"

# Verify by hash that what landed is what was built.
foreach ($pair in @(
    @{ Built = (Join-Path $CoreOut 'prism-core.exe'); Name = 'prism-core.exe' },
    @{ Built = (Join-Path $AppOut  'Prism.exe');      Name = 'Prism.exe' }
)) {
    $installed = Join-Path $InstallDir $pair.Name
    $a = Get-Sha $pair.Built
    $b = Get-Sha $installed
    if ($a -ne $b) { throw "$($pair.Name): hash drift" }
    Write-Host "verified $($pair.Name) $($a.Substring(0,12))"
}

Write-Host 'done'
