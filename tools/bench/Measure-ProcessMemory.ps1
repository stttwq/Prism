[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$ReleaseDirectory,
    [ValidateRange(1, 10000)][int]$SampleCount = 5,
    [ValidateRange(0, 60000)][int]$IntervalMilliseconds = 1000,
    [string]$IndexerPipeName = 'prism-indexer-v1',
    [string]$RunId = ('g0-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ'))
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force

$release = Assert-ReleaseDirectory -Path $ReleaseDirectory
$output = Resolve-BenchmarkOutputDirectory -Path $OutputDirectory `
    -AdditionalProtectedRoots @($release)
$rawPath = Join-Path $output 'memory-samples.jsonl'
$summaryPath = Join-Path $output 'memory-summary.json'
foreach ($path in @($rawPath, $summaryPath)) {
    if (Test-Path -LiteralPath $path) { throw "Refusing to overwrite an existing baseline artifact: $path" }
}

$targets = @(
    [ordered]@{ label = 'frontend'; process_name = 'Prism'; binary = 'Prism.exe' },
    [ordered]@{ label = 'broker'; process_name = 'prism-core'; binary = 'prism-core.exe' },
    [ordered]@{ label = 'indexer'; process_name = 'prism-indexer-service'; binary = 'prism-indexer-service.exe' }
)

$serviceText = sc.exe queryex PrismIndexer | Out-String
if ($serviceText -notmatch 'STATE\s+:\s+4\s+RUNNING' -or $serviceText -notmatch 'PID\s+:\s+(\d+)') {
    throw 'PrismIndexer service must be RUNNING before memory sampling.'
}
$servicePid = [int]$Matches[1]
$null = Assert-IndexerServiceReleaseBinary -ReleaseDirectory $release
$records = [Collections.Generic.List[object]]::new()
$expectedPids = @{}

for ($sampleNumber = 1; $sampleNumber -le $SampleCount; $sampleNumber++) {
    $sampleTimer = [Diagnostics.Stopwatch]::StartNew()
    $capturedAt = [DateTime]::UtcNow
    $processByLabel = @{}
    foreach ($target in $targets) {
        $processes = @(Get-Process -Name $target.process_name -ErrorAction SilentlyContinue)
        if ($processes.Count -ne 1) {
            throw "Expected exactly one $($target.process_name) process; found $($processes.Count)."
        }
        $process = $processes[0]
        if ($expectedPids.ContainsKey($target.label) -and $expectedPids[$target.label] -ne $process.Id) {
            throw "$($target.label) process changed during memory sampling."
        }
        $expectedPids[$target.label] = $process.Id
        if ($target.label -eq 'indexer') {
            if ($process.Id -ne $servicePid) { throw 'Indexer process does not match the PrismIndexer service PID.' }
        } else {
            $expectedPath = Join-Path $release $target.binary
            if ([string]::IsNullOrWhiteSpace([string]$process.Path) -or
                -not $process.Path.Equals($expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
                throw "$($target.label) process does not match ReleaseDirectory: $($process.Path)"
            }
        }
        $processByLabel[$target.label] = $process
    }
    # The CIM formatted provider avoids localized Get-Counter path names and
    # returns IDProcess and WorkingSetPrivate in one provider snapshot.
    $counterSamples = @(Get-CimInstance -ClassName Win32_PerfFormattedData_PerfProc_Process)
    $processSamples = foreach ($target in $targets) {
        $process = $processByLabel[$target.label]
        $matchingCounters = @($counterSamples | Where-Object { [int]$_.IDProcess -eq $process.Id })
        if ($matchingCounters.Count -ne 1) {
            throw "Expected one private-working-set counter for PID $($process.Id) ($($target.process_name)); got $($matchingCounters.Count)."
        }
        [ordered]@{
            label = $target.label
            process_name = $process.ProcessName
            pid = $process.Id
            private_working_set_bytes = [UInt64]$matchingCounters[0].WorkingSetPrivate
            working_set_bytes = [UInt64]$process.WorkingSet64
            private_bytes = [UInt64]$process.PrivateMemorySize64
        }
    }

    $status = Get-IndexerStatus -PipeName $IndexerPipeName
    if (-not $status.ready -or $status.building -or $status.degraded) {
        $statusMessage = if ($null -ne $status.PSObject.Properties['message']) { $status.message } else { 'no detail' }
        throw "Indexer is not healthy at the memory sample point: $statusMessage"
    }
    if ([UInt64]$status.generation -eq 0) { throw 'Indexer reported generation zero at the memory sample point.' }
    $sampleTimer.Stop()
    $record = [ordered]@{
        schema_version = 1
        run_id = $RunId
        sample_number = $sampleNumber
        captured_at_utc = $capturedAt.ToString('o')
        capture_duration_ms = [Math]::Round($sampleTimer.Elapsed.TotalMilliseconds, 3)
        process_samples = @($processSamples)
        totals = [ordered]@{
            private_working_set_bytes = [UInt64](($processSamples.private_working_set_bytes | Measure-Object -Sum).Sum)
            working_set_bytes = [UInt64](($processSamples.working_set_bytes | Measure-Object -Sum).Sum)
            private_bytes = [UInt64](($processSamples.private_bytes | Measure-Object -Sum).Sum)
        }
        indexer_status = [ordered]@{
            ready = [bool]$status.ready
            building = [bool]$status.building
            degraded = [bool]$status.degraded
            generation = [UInt64]$status.generation
            volume_count = [int]$status.volumes
            reported_index_memory_bytes = [UInt64]$status.memory_bytes
            node_count = [ordered]@{ value = $null; status = 'g1_pending' }
            name_pool_capacity_bytes = [ordered]@{ value = $null; status = 'g1_pending' }
            cache_version = [ordered]@{ value = $null; status = 'g1_pending_runtime_protocol'; source_expected = 5 }
        }
    }
    $records.Add($record)
    if ($sampleNumber -lt $SampleCount -and $IntervalMilliseconds -gt 0) {
        Start-Sleep -Milliseconds $IntervalMilliseconds
    }
}

$processSummary = foreach ($target in $targets) {
    $values = [double[]]@($records | ForEach-Object {
        @($_.process_samples | Where-Object label -eq $target.label)[0].private_working_set_bytes
    })
    [ordered]@{
        label = $target.label
        private_working_set_p50_bytes = [UInt64](Get-NearestRankPercentile -Values $values -Percentile 0.50)
        private_working_set_max_bytes = [UInt64](($values | Measure-Object -Maximum).Maximum)
    }
}
$totalPrivateWs = [double[]]@($records | ForEach-Object { $_.totals.private_working_set_bytes })
$totalWs = [double[]]@($records | ForEach-Object { $_.totals.working_set_bytes })
$memoryGateLimitBytes = [UInt64](100 * 1024 * 1024)
$maximumPrivateWorkingSetBytes = [UInt64](($totalPrivateWs | Measure-Object -Maximum).Maximum)
$summary = [ordered]@{
    schema_version = 1
    run_id = $RunId
    sample_count = $records.Count
    memory_gate_metric = 'sum(private_working_set_bytes) for Prism.exe + prism-core.exe + prism-indexer-service.exe'
    memory_gate_limit_bytes = $memoryGateLimitBytes
    memory_gate_passed = $maximumPrivateWorkingSetBytes -le $memoryGateLimitBytes
    diagnostic_metric = 'working_set_bytes and private_bytes are reported separately; private_bytes is not private working set'
    process_aggregates = @($processSummary)
    totals = [ordered]@{
        private_working_set_p50_bytes = [UInt64](Get-NearestRankPercentile -Values $totalPrivateWs -Percentile 0.50)
        private_working_set_max_bytes = $maximumPrivateWorkingSetBytes
        working_set_p50_bytes = [UInt64](Get-NearestRankPercentile -Values $totalWs -Percentile 0.50)
        working_set_max_bytes = [UInt64](($totalWs | Measure-Object -Maximum).Maximum)
    }
    indexer = [ordered]@{
        generations = @($records.indexer_status.generation | Select-Object -Unique)
        generation_changed_during_sampling = @($records.indexer_status.generation | Select-Object -Unique).Count -gt 1
        consistency = 'Each memory record includes a healthy indexer status captured at that sample point.'
        volume_count = [int]$records[0].indexer_status.volume_count
        reported_index_memory_bytes = [UInt64]$records[0].indexer_status.reported_index_memory_bytes
    }
}
try {
    Write-JsonLines -Path $rawPath -Values @($records)
    Assert-MemoryAcceptance -MaximumPrivateWorkingSetBytes $maximumPrivateWorkingSetBytes `
        -LimitBytes $memoryGateLimitBytes
    Write-Utf8NoBom -Path $summaryPath -Value (($summary | ConvertTo-Json -Depth 20) + "`n")
} catch {
    if (Test-Path -LiteralPath $summaryPath) { Remove-Item -LiteralPath $summaryPath -Force }
    throw
}
Write-Host "Memory baseline complete: $summaryPath"
