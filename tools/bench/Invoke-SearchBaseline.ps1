[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$ReleaseDirectory,
    [Parameter(Mandatory)][string]$SecuritySoftwareNotes,
    [Parameter(Mandatory)][string]$BackgroundIoNotes,
    [Parameter(Mandatory)][ValidateSet('z', '3')][string]$EffectiveOptLevel,
    [string]$FixturePath,
    [ValidateRange(30, 10000)][int]$Iterations = 30,
    [ValidateRange(1, 100)][int]$WarmupCycles = 3,
    [ValidateSet(8, 1000)][int[]]$MaxValues = @(8, 1000),
    [ValidateRange(1, 600)][int]$ReadyTimeoutSeconds = 120,
    [ValidateRange(1, 30)][int]$GenerationStableSeconds = 5,
    [ValidateRange(1, 300000)][int]$RequestTimeoutMs = 30000,
    [string]$BrokerPipeName = 'prism-core',
    [string]$IndexerPipeName = 'prism-indexer-v1',
    [string]$RunId = ('g0-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')),
    [switch]$StartBroker
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force
if (-not $FixturePath) { $FixturePath = Join-Path $PSScriptRoot 'queries.json' }

$release = Assert-ReleaseDirectory -Path $ReleaseDirectory
$output = Resolve-BenchmarkOutputDirectory -Path $OutputDirectory `
    -AdditionalProtectedRoots @($release)
$rawPath = Join-Path $output 'search-samples.jsonl'
$summaryPath = Join-Path $output 'search-summary.json'
$environmentPath = Join-Path $output 'environment.json'
foreach ($path in @($rawPath, $summaryPath, $environmentPath)) {
    if (Test-Path -LiteralPath $path) {
        throw "Refusing to overwrite an existing baseline artifact: $path"
    }
}

$fixtureDocument = Get-Content -LiteralPath $FixturePath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($fixtureDocument.schema_version -ne 1 -or -not $fixtureDocument.query_set_version) {
    throw 'Unsupported or incomplete query fixture schema.'
}
$queries = @($fixtureDocument.queries)
if ($queries.Count -eq 0) { throw 'The query fixture is empty.' }
$duplicateIds = @($queries | Group-Object id | Where-Object Count -gt 1)
if ($duplicateIds.Count -gt 0) { throw 'Query fixture ids must be unique.' }
foreach ($query in $queries) {
    if ([string]::IsNullOrWhiteSpace([string]$query.id) -or
        [string]::IsNullOrWhiteSpace([string]$query.query)) {
        throw 'Every query fixture needs a non-empty id and query.'
    }
}
$fixtureCategories = @($queries.categories | ForEach-Object { $_ })
foreach ($requiredCategory in @('ascii', 'chinese', 'exact', 'prefix', 'contains', 'rare_term', 'no_hit', 'cross_volume')) {
    if ($fixtureCategories -notcontains $requiredCategory) {
        throw "Query fixture is missing required category: $requiredCategory"
    }
}

$manifest = Get-BenchmarkEnvironment -RunId $RunId -ReleaseDirectory $release `
    -EffectiveOptLevel $EffectiveOptLevel -SecuritySoftwareNotes $SecuritySoftwareNotes `
    -BackgroundIoNotes $BackgroundIoNotes
Write-Utf8NoBom -Path $environmentPath -Value (($manifest | ConvertTo-Json -Depth 20) + "`n")

$ownedBroker = $null
$brokerStartup = [Diagnostics.Stopwatch]::StartNew()
$session = $null
$samples = [Collections.Generic.List[object]]::new()

function Assert-SearchResponse {
    param($Result, [string]$ExpectedQuery)
    $response = $Result.Response
    if ($response.type -ne 'results') { throw "Expected results, got $($response.type)." }
    if ([string]$response.query -ne $ExpectedQuery) { throw 'Broker response/query pairing failed.' }
    $errorProperty = $response.PSObject.Properties['index_error']
    if ($null -ne $errorProperty -and $errorProperty.Value) {
        throw "Indexer reported an error: $($errorProperty.Value)"
    }
    return $response
}

function New-SearchSample {
    param($Fixture, [int]$Max, [string]$Phase, [int]$Iteration, $Result)
    $response = Assert-SearchResponse -Result $Result -ExpectedQuery ([string]$Fixture.query)
    $items = @($response.items)
    if ($null -eq $response.PSObject.Properties['index_generation']) {
        throw 'Broker response did not include an index generation.'
    }
    $generation = [UInt64]$response.index_generation
    if ($generation -eq 0) { throw 'Broker response reported generation zero.' }
    $truncatedProperty = $response.PSObject.Properties['is_truncated']
    $matchedProperty = $response.PSObject.Properties['matched_count']
    $scannedProperty = $response.PSObject.Properties['scanned_nodes']
    $candidateProperty = $response.PSObject.Properties['name_candidates']
    $enteredProperty = $response.PSObject.Properties['entered_top_k']
    $pathProperty = $response.PSObject.Properties['path_constructions']
    $measured = { param($Property) [ordered]@{ value = [UInt64]$Property.Value; status = 'measured' } }
    $pending = { [ordered]@{ value = $null; status = 'g1_pending' } }
    return [ordered]@{
        schema_version = 1
        run_id = $RunId
        query_set_version = [string]$fixtureDocument.query_set_version
        query_id = [string]$Fixture.id
        categories = @($Fixture.categories)
        volume_scope = @($Fixture.volume_scope)
        max = $Max
        phase = $Phase
        iteration = $Iteration
        started_at_utc = [DateTime]::UtcNow.AddMilliseconds(-$Result.ElapsedMs).ToString('o')
        elapsed_ms = $Result.ElapsedMs
        result_count = $items.Count
        is_indexing = [bool]$response.is_indexing
        generation = $generation
        response_bytes = $Result.ResponseBytes
        result_count_reached_max = ($items.Count -eq $Max)
        truncated = if ($null -ne $truncatedProperty) {
            [ordered]@{ value = [bool]$truncatedProperty.Value; status = 'measured' }
        } else { & $pending }
        workload = [ordered]@{
            scanned_nodes = if ($null -ne $scannedProperty) { & $measured $scannedProperty } else { & $pending }
            name_candidates = if ($null -ne $candidateProperty) { & $measured $candidateProperty } else { & $pending }
            matching_names = if ($null -ne $matchedProperty) { & $measured $matchedProperty } else { & $pending }
            entered_top_k = if ($null -ne $enteredProperty) { & $measured $enteredProperty } else { & $pending }
            path_constructions = if ($null -ne $pathProperty) { & $measured $pathProperty } else { & $pending }
        }
    }
}

try {
    $existing = @(Get-Process -Name 'prism-core' -ErrorAction SilentlyContinue)
    if ($StartBroker) {
        if ($existing.Count -ne 0) { throw '-StartBroker requires that no prism-core process is already running.' }
        $brokerPath = Join-Path $release 'prism-core.exe'
        $ownedBroker = Start-Process -FilePath $brokerPath -PassThru -WindowStyle Hidden
    } elseif ($existing.Count -ne 1) {
        throw "Expected exactly one running prism-core process; found $($existing.Count). Use -StartBroker for an isolated broker run."
    } else {
        $processPath = $existing[0].Path
        if ([string]::IsNullOrWhiteSpace([string]$processPath) -or
            -not $processPath.Equals((Join-Path $release 'prism-core.exe'), [StringComparison]::OrdinalIgnoreCase)) {
            throw "Running broker does not match ReleaseDirectory: $processPath"
        }
    }

    $connectDeadline = [DateTime]::UtcNow.AddSeconds([Math]::Min(30, $ReadyTimeoutSeconds))
    do {
        try {
            $session = New-PipeSession -PipeName $BrokerPipeName -ConnectTimeoutMs 1000
        } catch {
            if ([DateTime]::UtcNow -ge $connectDeadline) { throw }
            Start-Sleep -Milliseconds 100
        }
    } while ($null -eq $session)
    $brokerStartup.Stop()

    $ping = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'ping' }) -RequestTimeoutMs $RequestTimeoutMs
    if ($ping.Response.type -ne 'pong') { throw 'Broker ping failed.' }

    $readyDeadline = [DateTime]::UtcNow.AddSeconds($ReadyTimeoutSeconds)
    $readyProbe = [ordered]@{ id = 'readiness_probe'; query = 'prism-g0-ready-probe-019fabfb'; categories = @('synthetic'); volume_scope = @('all_indexed_volumes') }
    $readyStatus = $null
    $stableGeneration = [UInt64]0
    $stableSince = $null
    do {
        $readyStatus = Get-IndexerStatus -PipeName $IndexerPipeName
        $probe = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'search'; query = $readyProbe.query; max = 8 }) -RequestTimeoutMs $RequestTimeoutMs
        $probeResponse = Assert-SearchResponse -Result $probe -ExpectedQuery $readyProbe.query
        $probeGeneration = if ($null -ne $probeResponse.PSObject.Properties['index_generation']) { [UInt64]$probeResponse.index_generation } else { 0 }
        $isReady = [bool]$readyStatus.ready -and -not [bool]$readyStatus.building -and
            -not [bool]$readyStatus.degraded -and -not [bool]$probeResponse.is_indexing -and
            [UInt64]$readyStatus.generation -ne 0 -and $probeGeneration -eq [UInt64]$readyStatus.generation
        if ($isReady) {
            if ($stableGeneration -ne [UInt64]$readyStatus.generation) {
                $stableGeneration = [UInt64]$readyStatus.generation
                $stableSince = [DateTime]::UtcNow
            }
            $isStable = ([DateTime]::UtcNow - $stableSince).TotalSeconds -ge $GenerationStableSeconds
        } else {
            $stableGeneration = [UInt64]0
            $stableSince = $null
            $isStable = $false
        }
        if (-not $isReady -or -not $isStable) {
            if ([DateTime]::UtcNow -ge $readyDeadline) { throw "Index did not become ready within $ReadyTimeoutSeconds seconds." }
            Start-Sleep -Milliseconds 100
        }
    } while (-not $isReady -or -not $isStable)
    $readyGeneration = $stableGeneration

    foreach ($max in $MaxValues) {
        $first = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'search'; query = $queries[0].query; max = $max }) -RequestTimeoutMs $RequestTimeoutMs
        $sample = New-SearchSample -Fixture $queries[0] -Max $max -Phase 'first_ready' -Iteration 0 -Result $first
        $samples.Add($sample)
    }

    for ($cycle = 1; $cycle -le $WarmupCycles; $cycle++) {
        foreach ($max in $MaxValues) {
            foreach ($query in $queries) {
                $warmup = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'search'; query = $query.query; max = $max }) -RequestTimeoutMs $RequestTimeoutMs
                $warmupResponse = Assert-SearchResponse -Result $warmup -ExpectedQuery $query.query
                if ($warmupResponse.is_indexing) { throw 'Index returned to an indexing state during warmup.' }
            }
        }
    }

    for ($iteration = 1; $iteration -le $Iterations; $iteration++) {
        foreach ($max in $MaxValues) {
            foreach ($query in $queries) {
                $result = Send-PipeRequest -Session $session -Request ([ordered]@{ type = 'search'; query = $query.query; max = $max }) -RequestTimeoutMs $RequestTimeoutMs
                $sample = New-SearchSample -Fixture $query -Max $max -Phase 'warm' -Iteration $iteration -Result $result
                if ($sample.is_indexing) { throw 'Index returned to an indexing state during formal sampling.' }
                $samples.Add($sample)
            }
        }
    }

    $summary = New-SearchSummary -Samples @($samples) -RunId $RunId -QuerySetVersion $fixtureDocument.query_set_version
    $summary['startup'] = [ordered]@{
        broker_started_by_runner = [bool]$StartBroker
        broker_start_to_pipe_ms = [Math]::Round($brokerStartup.Elapsed.TotalMilliseconds, 3)
        broker_version = [string]$ping.Response.version
        ready_generation = [UInt64]$readyStatus.generation
        indexer_volumes = [int]$readyStatus.volumes
        indexer_reported_memory_bytes = [UInt64]$readyStatus.memory_bytes
    }
    $sampleGenerations = @($samples.generation | Sort-Object -Unique)
    $summary['index_generations'] = [ordered]@{
        initial_ready_generation = $readyGeneration
        distinct_count = $sampleGenerations.Count
        minimum = [UInt64]$sampleGenerations[0]
        maximum = [UInt64]$sampleGenerations[-1]
        changed_during_sampling = $sampleGenerations.Count -gt 1
        consistency = 'Each result and generation are captured atomically under the index read lock.'
    }
    $finalStatus = Get-IndexerStatus -PipeName $IndexerPipeName
    if (-not $finalStatus.ready -or $finalStatus.building -or $finalStatus.degraded) {
        throw 'Indexer was not healthy after formal sampling.'
    }
    Write-JsonLines -Path $rawPath -Values @($samples)
    Write-Utf8NoBom -Path $summaryPath -Value (($summary | ConvertTo-Json -Depth 20) + "`n")
    Write-Host "Search baseline complete: $summaryPath"
} catch {
    if (Test-Path -LiteralPath $summaryPath) { Remove-Item -LiteralPath $summaryPath -Force }
    throw
} finally {
    if ($null -ne $session) { Close-PipeSession -Session $session }
    if ($null -ne $ownedBroker -and -not $ownedBroker.HasExited) {
        Stop-Process -Id $ownedBroker.Id -ErrorAction SilentlyContinue
        $ownedBroker.WaitForExit(5000) | Out-Null
    }
}
