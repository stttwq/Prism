[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$ReleaseDirectory,
    [string]$RunId = ('rebuild-gate-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')),
    [string]$ServiceName = 'PrismIndexer',
    [string]$PipeName = 'prism-indexer-v1',
    [string]$CachePath = (Join-Path $env:ProgramData 'Prism\index-v5.bin'),
    # Active mode: full rebuild driven by cache delete + service restart (the only
    # scriptable full-rebuild channel; the IPC protocol is read-only). Passive
    # mode: no service control, no admin - waits for a naturally occurring
    # rebuild window (the pre-fix rebuild storm provides these) and samples it.
    [switch]$Passive,
    [ValidateRange(60, 3600)][int]$TimeoutSeconds = 900,
    [ValidateRange(20, 1000)][int]$PollMilliseconds = 100
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force

# Rebuild-window memory gate. Necessary differences from Measure-ProcessMemory.ps1
# (the idle gate stays byte-for-byte unchanged as the regression reference):
#   - ready=false / building=true are ALLOWED - the window under test is exactly
#     "a rebuild is in flight"; only the END condition requires ready && !building.
#   - The indexer PID may change across the stop/start boundary (active mode),
#     but within one sampling round (between two consecutive successful samples)
#     it must be stable. Frontend/broker PIDs must stay stable throughout.
#   - Status polls are best-effort while the service is down (active mode) - a
#     failed status read is not an error there.
$release = Assert-ReleaseDirectory -Path $ReleaseDirectory
$output = Resolve-BenchmarkOutputDirectory -Path $OutputDirectory -AdditionalProtectedRoots @($release)
$rawPath = Join-Path $output 'rebuild-memory-samples.jsonl'
$summaryPath = Join-Path $output 'rebuild-memory-summary.json'
foreach ($path in @($rawPath, $summaryPath)) {
    if (Test-Path -LiteralPath $path) { throw "Refusing to overwrite an existing baseline artifact: $path" }
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $Passive -and -not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "Active rebuild mode must run from an elevated PowerShell process (service stop/start). Use -Passive to sample a naturally occurring rebuild without elevation."
}

$targets = @(
    [ordered]@{ label = 'frontend'; process_name = 'Prism'; binary = 'Prism.exe' },
    [ordered]@{ label = 'broker'; process_name = 'prism-core'; binary = 'prism-core.exe' },
    [ordered]@{ label = 'indexer'; process_name = 'prism-indexer-service'; binary = 'prism-indexer-service.exe' }
)

function Get-ServiceStateText {
    return (sc.exe queryex $ServiceName | Out-String)
}

function Stop-IndexerService {
    $query = Get-ServiceStateText
    if ($query -match 'STATE\s+:\s+1\s+STOPPED') { return }
    $null = sc.exe stop $ServiceName
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $query = Get-ServiceStateText
        if ($query -match 'STATE\s+:\s+1\s+STOPPED') { return }
        Start-Sleep -Milliseconds 25
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "$ServiceName did not stop within 15 seconds. Last state:`n$query"
}

function Start-IndexerService {
    $query = Get-ServiceStateText
    if ($query -match 'STATE\s+:\s+4\s+RUNNING') { return }
    $null = sc.exe start $ServiceName
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $query = Get-ServiceStateText
        if ($query -match 'STATE\s+:\s+4\s+RUNNING') { return }
        Start-Sleep -Milliseconds 25
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "$ServiceName did not start within 15 seconds. Last state:`n$query"
}

function Get-StatusOrNull {
    try {
        return Get-IndexerStatus -PipeName $PipeName -ConnectTimeoutMs 250
    } catch {
        return $null
    }
}

function Get-OptionalPropertyValue {
    param(
        [AllowNull()][object]$InputObject,
        [Parameter(Mandatory)][string]$Name
    )
    if ($null -eq $InputObject) { return $null }
    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

# One synchronized three-process WorkingSetPrivate snapshot (same CIM formatted
# provider + IDProcess mapping as Measure-ProcessMemory.ps1). Returns $null when
# a process is momentarily absent (indexer restarting) - the caller decides
# whether absence is legal at that point in the run.
function Get-ProcessMemorySnapshot {
    param([Parameter(Mandatory)][hashtable]$ExpectedPids)

    $counterSamples = @(Get-CimInstance -ClassName Win32_PerfFormattedData_PerfProc_Process)
    $processSamples = [Collections.Generic.List[object]]::new()
    foreach ($target in $targets) {
        $processes = @(Get-Process -Name $target.process_name -ErrorAction SilentlyContinue)
        if ($processes.Count -ne 1) { return $null }
        $process = $processes[0]
        $label = $target.label
        if ($ExpectedPids.ContainsKey($label) -and $ExpectedPids[$label] -ne $process.Id) {
            throw "$label PID changed within the sampling round: $($ExpectedPids[$label]) -> $($process.Id)"
        }
        $matchingCounters = @($counterSamples | Where-Object { [int]$_.IDProcess -eq $process.Id })
        if ($matchingCounters.Count -ne 1) { return $null }
        if ($label -ne 'indexer') {
            $expectedPath = Join-Path $release $target.binary
            if ([string]::IsNullOrWhiteSpace([string]$process.Path) -or
                -not $process.Path.Equals($expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
                throw "$label process does not match ReleaseDirectory: $($process.Path)"
            }
        }
        $processSamples.Add([ordered]@{
            label = $label
            process_name = $process.ProcessName
            pid = $process.Id
            private_working_set_bytes = [UInt64]$matchingCounters[0].WorkingSetPrivate
            working_set_bytes = [UInt64]$process.WorkingSet64
            private_bytes = [UInt64]$process.PrivateMemorySize64
        })
    }
    return $processSamples
}

$records = [Collections.Generic.List[object]]::new()
$expectedPids = @{}
$cacheBackup = Join-Path $output 'index-v5-before.bin'
$serviceWasRunning = $false
$windowObserved = $false
$GateSucceeded = $false
$runTimer = [Diagnostics.Stopwatch]::StartNew()
$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)

try {
    if (-not $Passive) {
        $serviceWasRunning = (Get-ServiceStateText) -match 'STATE\s+:\s+4\s+RUNNING'
        if (-not $serviceWasRunning) { throw "$ServiceName must be RUNNING before the rebuild gate starts." }
        $null = Assert-IndexerServiceReleaseBinary -ReleaseDirectory $release
        Stop-IndexerService
        if (Test-Path -LiteralPath $CachePath) {
            Copy-Item -LiteralPath $CachePath -Destination $cacheBackup -Force
            Remove-Item -LiteralPath $CachePath -Force
        }
        Start-IndexerService
        $windowObserved = $true
    }

    while ([DateTime]::UtcNow -lt $deadline) {
        $status = Get-StatusOrNull
        $building = $false
        $ready = $false
        if ($null -ne $status) {
            $building = [bool]$status.building
            $ready = [bool]$status.ready
        }
        if ($building) { $windowObserved = $true }
        if (-not $windowObserved -and $Passive) {
            Start-Sleep -Milliseconds $PollMilliseconds
            continue
        }

        $snapshot = Get-ProcessMemorySnapshot -ExpectedPids $expectedPids
        if ($null -ne $snapshot) {
            foreach ($sample in $snapshot) { $expectedPids[$sample.label] = $sample.pid }
            $record = [ordered]@{
                schema_version = 1
                run_id = $RunId
                mode = if ($Passive) { 'passive' } else { 'active' }
                elapsed_ms = [Math]::Round($runTimer.Elapsed.TotalMilliseconds, 3)
                captured_at_utc = [DateTime]::UtcNow.ToString('o')
                ready = $ready
                building = $building
                generation = if ($null -ne $status) { [UInt64]$status.generation } else { $null }
                process_samples = @($snapshot)
                totals = [ordered]@{
                    private_working_set_bytes = [UInt64](($snapshot.private_working_set_bytes | Measure-Object -Sum).Sum)
                    working_set_bytes = [UInt64](($snapshot.working_set_bytes | Measure-Object -Sum).Sum)
                    private_bytes = [UInt64](($snapshot.private_bytes | Measure-Object -Sum).Sum)
                }
            }
            $records.Add($record)
        }

        if ($windowObserved -and $ready -and -not $building) { break }
        Start-Sleep -Milliseconds $PollMilliseconds
    }

    if (-not $windowObserved) {
        throw "No rebuild window was observed within $TimeoutSeconds seconds."
    }
    if ($records.Count -eq 0) { throw 'No memory samples were captured.' }

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
        mode = if ($Passive) { 'passive' } else { 'active' }
        sample_count = $records.Count
        window_wall_ms = [Math]::Round($runTimer.Elapsed.TotalMilliseconds, 3)
        memory_gate_metric = 'sum(private_working_set_bytes) for Prism.exe + prism-core.exe + prism-indexer-service.exe during a rebuild window'
        memory_gate_limit_bytes = $memoryGateLimitBytes
        memory_gate_passed = $maximumPrivateWorkingSetBytes -le $memoryGateLimitBytes
        process_aggregates = @($processSummary)
        totals = [ordered]@{
            private_working_set_p50_bytes = [UInt64](Get-NearestRankPercentile -Values $totalPrivateWs -Percentile 0.50)
            private_working_set_max_bytes = $maximumPrivateWorkingSetBytes
            working_set_p50_bytes = [UInt64](Get-NearestRankPercentile -Values $totalWs -Percentile 0.50)
            working_set_max_bytes = [UInt64](($totalWs | Measure-Object -Maximum).Maximum)
        }
    }
    Write-JsonLines -Path $rawPath -Values @($records)
    Write-Utf8NoBom -Path $summaryPath -Value (($summary | ConvertTo-Json -Depth 20) + "`n")
    # Same contract as the idle gate: Assert-MemoryAcceptance throws when the
    # maximum exceeds the limit and the catch removes the summary (no success
    # summary on failure) - but the raw JSONL stays as the provable A-side
    # evidence, and the failing maximum is in the thrown message.
    Assert-MemoryAcceptance -MaximumPrivateWorkingSetBytes $maximumPrivateWorkingSetBytes `
        -LimitBytes $memoryGateLimitBytes
    $GateSucceeded = $true
    $summary | ConvertTo-Json -Depth 20
} catch {
    if (Test-Path -LiteralPath $summaryPath) { Remove-Item -LiteralPath $summaryPath -Force }
    throw
} finally {
    # Passive mode never touches the service. Active mode leaves the freshly
    # rebuilt cache in place on success (it carries the same on-disk state the
    # next rebuild would produce); only a failure path restores the pre-run
    # backup and returns the service to RUNNING, matching G9's finally.
    if (-not $Passive -and -not $GateSucceeded) {
        try {
            $null = Stop-IndexerService
            if (-not (Test-Path -LiteralPath $CachePath) -and (Test-Path -LiteralPath $cacheBackup)) {
                Copy-Item -LiteralPath $cacheBackup -Destination $CachePath -Force
            }
        } catch { }
        if ($serviceWasRunning) {
            try { $null = Start-IndexerService } catch { }
        }
    }
}
