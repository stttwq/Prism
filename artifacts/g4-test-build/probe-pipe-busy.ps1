# Regression probe for the ERROR_PIPE_BUSY defect (08-07-prism-ipc-resilience).
#
# Original failure (2026-08-07): 517 of 800 searches came back as
#   "indexer service is unavailable: ... (os error 231)"
# while the index was absorbing a USN flood.
#
# Root cause was NOT connection volume - sequential single-client load never
# reproduced it. It was worker-thread starvation: `IndexerRequest::Status`
# called `state.status()` directly on a runtime worker, and `status()` takes
# `index.read()` synchronously. With worker_threads(2) and the USN watcher
# holding `index.write()` in tight batches, two concurrent Status requests
# blocked both workers, so the pipe accept loop never got scheduled to re-arm a
# listener.
#
# This probe recreates that shape: interleaved Status polls (a separate pipe,
# like the bench driver does) plus heavy searches, run concurrently.
#
# ASCII only: PowerShell 5.1 reads BOM-less UTF-8 as GBK under a Chinese locale.
[CmdletBinding()]
param(
    [int]$Rounds = 150,
    # Scope for the heavy search arm. Pick a large indexed subtree: the point is to
    # make each search expensive enough to contend with the USN watcher. A small or
    # unindexed root makes searches cheap and the probe proves nothing.
    [string]$Root = 'C:\Windows'
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot '..\..\tools\bench\Bench.Common.psm1') -Force

$searchSession = New-PipeSession -PipeName 'prism-core'
try {
    Send-PipeRequest -Session $searchSession `
        -Request ([ordered]@{ type = 'hello'; protocol = 1 }) -RequestTimeoutMs 10000 | Out-Null

    $busy = 0
    $timeout = 0
    $otherError = 0
    $ok = 0
    $statusFail = 0
    $latencies = New-Object System.Collections.Generic.List[double]

    for ($i = 1; $i -le $Rounds; $i++) {
        # Status poll on its own connection, exactly what the bench driver does
        # between samples. This is the request that used to block a worker.
        try {
            Get-IndexerStatus -PipeName 'prism-indexer-v1' | Out-Null
        } catch {
            $statusFail++
        }

        $max = if ($i % 2 -eq 0) { 1000 } else { 8 }
        $request = [ordered]@{ type = 'search'; query = 'e'; max = $max; root = $Root }
        $result = Send-PipeRequest -Session $searchSession -Request $request -RequestTimeoutMs 40000
        $response = $result.Response
        $latencies.Add([double]$result.ElapsedMs)

        $indexError = ''
        if ($response.PSObject.Properties['index_error'] -and $response.index_error) {
            $indexError = [string]$response.index_error
        }

        if ($indexError -eq '') {
            $ok++
        } elseif ($indexError -match '231|unavailable') {
            $busy++
        } elseif ($indexError -match 'timed out') {
            $timeout++
        } else {
            $otherError++
            if ($otherError -le 3) { Write-Host "  unexpected: $indexError" }
        }
    }

    $sorted = $latencies | Sort-Object
    $p95Index = [Math]::Max(0, [int][Math]::Ceiling(0.95 * $sorted.Count) - 1)

    Write-Host ''
    Write-Host ('rounds        : {0}' -f $Rounds)
    Write-Host ('ok            : {0}' -f $ok)
    Write-Host ('PIPE_BUSY     : {0}' -f $busy)
    Write-Host ('timeout       : {0}' -f $timeout)
    Write-Host ('other errors  : {0}' -f $otherError)
    Write-Host ('status failures: {0}' -f $statusFail)
    Write-Host ('search p50/p95: {0:N0} / {1:N0} ms' -f $sorted[[int]($sorted.Count / 2)], $sorted[$p95Index])
    Write-Host ''
    if ($busy -eq 0 -and $timeout -eq 0 -and $statusFail -eq 0) {
        Write-Host 'PASS: no pipe-busy, no timeout, no status failure.' -ForegroundColor Green
    } else {
        Write-Host 'FAIL: IPC still degrades under this load.' -ForegroundColor Red
        exit 1
    }
} finally {
    Close-PipeSession -Session $searchSession
}
