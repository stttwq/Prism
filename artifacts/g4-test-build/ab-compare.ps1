# A/B the pipe-busy defect: run the same flood probe against the pre-fix binary
# and the post-fix binary.
#
# Without this, "0 errors after the fix" is unfalsifiable - the quiet-machine run
# also showed 0 errors before any flood existed. Only reproducing the failure with
# the old binary, under the same load, shows the fix is what changed the outcome.
#
# Requires elevation: swaps the service binary and restarts the service.
# Restores the post-fix binary in `finally`, including on Ctrl-C.
#
# ASCII only: PowerShell 5.1 reads BOM-less UTF-8 as GBK under a Chinese locale.
[CmdletBinding()]
param(
    # Empty means "resolve relative to this script" below; $PSScriptRoot is not
    # available while the param block is being evaluated.
    [string]$OldBinary = '',
    [string]$NewBinary = '',
    [string]$Installed = 'C:\Program Files\Prism\prism-indexer-service.exe',
    [int]$Rounds = 150
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $here '..\..')).Path

if ([string]::IsNullOrWhiteSpace($OldBinary)) {
    $OldBinary = Join-Path $repo 'dist\prism-indexer-service.exe'
}
if ([string]::IsNullOrWhiteSpace($NewBinary)) {
    $NewBinary = Join-Path $repo 'src\prism-core\target\release\prism-indexer-service.exe'
}

function Test-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole(
        [Security.Principal.WindowsBuiltinRole]::Administrator)
}
if (-not (Test-Admin)) { throw 'must run elevated (swaps the service binary)' }

function Install-Binary([string]$source, [string]$label) {
    Write-Host ''
    Write-Host "=== installing $label ===" -ForegroundColor Cyan
    Stop-Service PrismIndexer -Force
    $waited = 0
    while ((Get-Service PrismIndexer).Status -ne 'Stopped' -and $waited -lt 40) {
        Start-Sleep -Milliseconds 500
        $waited++
    }
    Get-Process prism-indexer-service -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 800

    Copy-Item -LiteralPath $source -Destination $Installed -Force
    Start-Service PrismIndexer

    # Wait for a usable index; a fresh service start rebuilds from cache.
    Import-Module (Join-Path $repo 'tools\bench\Bench.Common.psm1') -Force
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Seconds 2
        try {
            $s = Get-IndexerStatus -PipeName 'prism-indexer-v1'
            if ($s.ready) {
                Write-Host ("index ready: gen={0} vols={1}" -f $s.generation, $s.volumes)
                return
            }
        } catch { }
    }
    throw "index never became ready with $label"
}

function Restart-Broker {
    Get-Process prism-core -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    Start-Process -FilePath 'C:\Program Files\Prism\prism-core.exe' -WindowStyle Hidden
    Start-Sleep -Seconds 3
}

$results = @{}
try {
    foreach ($arm in @(
        @{ label = 'PRE-FIX (dist, 08-07)'; binary = $OldBinary; key = 'pre' }
        @{ label = 'POST-FIX (current build)'; binary = $NewBinary; key = 'post' }
    )) {
        Install-Binary -source $arm.binary -label $arm.label
        Restart-Broker

        & (Join-Path $here 'probe-under-flood.ps1') -Rounds $Rounds -FloodWriters 4
        $results[$arm.key] = $LASTEXITCODE
        Write-Host ("--> {0} probe exit: {1}" -f $arm.label, $LASTEXITCODE)
    }
} finally {
    Write-Host ''
    Write-Host '=== restoring post-fix binary ===' -ForegroundColor Yellow
    Install-Binary -source $NewBinary -label 'POST-FIX (restore)'
    Restart-Broker
}

Write-Host ''
Write-Host '===== A/B RESULT =====' -ForegroundColor White
Write-Host ("pre-fix  exit: {0}  ({1})" -f $results['pre'], $(if ($results['pre'] -ne 0) { 'FAILED as expected' } else { 'did NOT reproduce' }))
Write-Host ("post-fix exit: {0}  ({1})" -f $results['post'], $(if ($results['post'] -eq 0) { 'PASSED' } else { 'STILL BROKEN' }))
