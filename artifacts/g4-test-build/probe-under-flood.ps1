# Same regression probe, but with a synthetic USN flood running underneath.
#
# The 2026-08-07 failure only appeared while the index was absorbing thousands of
# filesystem changes per second (Windows Update rewriting the disk). A quiet
# machine cannot reproduce it, so a clean run without load proves nothing.
#
# The flood writer creates/renames/deletes many small files on the indexed volume
# to force USN records, making the watcher hold index.write() in tight batches -
# the exact contention that used to starve the pipe accept loop.
#
# Everything is written under one scratch directory and removed afterwards.
#
# ASCII only: PowerShell 5.1 reads BOM-less UTF-8 as GBK under a Chinese locale.
[CmdletBinding()]
param(
    [int]$Rounds = 150,
    [int]$FloodWriters = 3,
    # Must sit on an INDEXED volume, otherwise no USN records reach the watcher and
    # the flood is silently a no-op. Empty resolves to a scratch dir next to this
    # script; override when the repo lives on an unindexed volume.
    [string]$ScratchRoot = ''
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($ScratchRoot)) {
    $ScratchRoot = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'flood-scratch'
}
if (Test-Path $ScratchRoot) { Remove-Item $ScratchRoot -Recurse -Force }
New-Item -ItemType Directory -Path $ScratchRoot -Force | Out-Null

$floodScript = {
    param($dir, $seconds)
    $deadline = (Get-Date).AddSeconds($seconds)
    $n = 0
    while ((Get-Date) -lt $deadline) {
        $a = Join-Path $dir ("f{0}.tmp" -f $n)
        $b = Join-Path $dir ("f{0}.ren" -f $n)
        try {
            Set-Content -LiteralPath $a -Value 'x' -NoNewline
            Rename-Item -LiteralPath $a -NewName (Split-Path $b -Leaf)
            Remove-Item -LiteralPath $b -Force
        } catch {
            # A racing writer may already have removed it; irrelevant to the probe.
        }
        $n++
    }
    return $n
}

Write-Host "starting $FloodWriters flood writers ..."
$jobs = @()
for ($w = 1; $w -le $FloodWriters; $w++) {
    $dir = Join-Path $ScratchRoot "w$w"
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $jobs += Start-Job -ScriptBlock $floodScript -ArgumentList $dir, 180
}

# Let the watcher actually start seeing the churn before measuring.
Start-Sleep -Seconds 8

try {
    Import-Module (Join-Path $PSScriptRoot '..\..\tools\bench\Bench.Common.psm1') -Force
    $before = (Get-IndexerStatus -PipeName 'prism-indexer-v1').generation
    Start-Sleep -Seconds 4
    $after = (Get-IndexerStatus -PipeName 'prism-indexer-v1').generation
    $rate = [Math]::Round(($after - $before) / 4.0, 1)
    Write-Host ("index churn: {0} generations/s (flood is {1})" -f $rate, $(if ($rate -gt 20) { 'ACTIVE' } else { 'TOO WEAK' }))
    Write-Host ''

    & (Join-Path $PSScriptRoot 'probe-pipe-busy.ps1') -Rounds $Rounds
    $probeExit = $LASTEXITCODE

    $after2 = (Get-IndexerStatus -PipeName 'prism-indexer-v1').generation
    Write-Host ("index advanced {0} generations during the probe" -f ($after2 - $after))
    exit $probeExit
} finally {
    $jobs | Stop-Job -ErrorAction SilentlyContinue
    $jobs | Remove-Job -Force -ErrorAction SilentlyContinue
    Remove-Item $ScratchRoot -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host 'flood stopped, scratch removed'
}
