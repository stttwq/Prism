# G4 root-scope paired comparison.
#
# WHAT THIS ANSWERS
#   G4 PRD line 15: "only if the G0/G1 baseline proves the parent-chain approach
#   cannot meet P95 may we introduce an explicit ancestor/subtree cache."
#   The decision needs one number: does adding `root` to a query make it slower,
#   and does ancestor validation (path_constructions) blow up under a deep,
#   candidate-heavy root?
#
# WHAT THIS IS NOT
#   Not the formal G0 baseline. Invoke-SearchBaseline.ps1 owns that and aborts
#   when the index generation moves, because absolute numbers are only
#   comparable across runs on a quiet machine. This script deliberately
#   tolerates churn: every fixture is measured in the same interleaved pass, so
#   background I/O hits the root and global arms equally and the *paired*
#   difference stays meaningful even when absolute latency is inflated.
#   Generation movement is recorded, never hidden.
#
# ASCII only: PowerShell 5.1 reads BOM-less UTF-8 as GBK under a Chinese locale.

[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(10, 500)][int]$Iterations = 40,
    [ValidateRange(0, 20)][int]$WarmupCycles = 3,
    [ValidateSet(8, 1000)][int[]]$MaxValues = @(8, 1000),
    [ValidateRange(1, 300000)][int]$RequestTimeoutMs = 40000,
    [string]$BrokerPipeName = 'prism-core',
    [string]$IndexerPipeName = 'prism-indexer-v1',
    [string]$Notes = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force

$output = Resolve-BenchmarkOutputDirectory -Path $OutputDirectory
$rawPath = Join-Path $output 'root-comparison-samples.jsonl'
$summaryPath = Join-Path $output 'root-comparison-summary.json'
foreach ($path in @($rawPath, $summaryPath)) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
}

# Paired fixtures: each root-scoped case has a global twin running the identical
# query string, so any difference is attributable to `root` alone.
$pairs = @(
    @{ pair = 'deep_high_candidate'; query = 'd';     root = 'D:\YX' }
    @{ pair = 'deep_system32';       query = 'win';   root = 'C:\Windows\System32' }
    @{ pair = 'shallow_small';       query = 'prism'; root = 'D:\LS\DM\Listary\src' }
    @{ pair = 'no_hit';              query = 'prism-g0-no-hit-019fabfb9808'; root = 'D:\YX' }
    @{ pair = 'drive_root';          query = 'd';     root = 'D:\' }
)

$cases = foreach ($p in $pairs) {
    [pscustomobject]@{ pair = $p.pair; arm = 'root';   query = $p.query; root = $p.root }
    [pscustomobject]@{ pair = $p.pair; arm = 'global'; query = $p.query; root = $null }
}

function New-Request {
    param([string]$Query, $Root, [int]$Max)
    $r = [ordered]@{ type = 'search'; query = $Query; max = $Max }
    if ($Root) { $r['root'] = [string]$Root }
    return $r
}

function Get-Counter {
    param($Response, [string]$Name)
    $p = $Response.PSObject.Properties[$Name]
    if ($null -eq $p -or $null -eq $p.Value) { return $null }
    return [UInt64]$p.Value
}

$samples = [Collections.Generic.List[object]]::new()
$generations = [Collections.Generic.List[UInt64]]::new()
$indexingHits = 0
$errors = [Collections.Generic.List[string]]::new()

$startStatus = Get-IndexerStatus -PipeName $IndexerPipeName
$session = New-PipeSession -PipeName $BrokerPipeName
try {
    $hello = Send-PipeRequest -Session $session `
        -Request ([ordered]@{ type = 'hello'; protocol = 1 }) -RequestTimeoutMs $RequestTimeoutMs
    if ($hello.Response.type -ne 'hello') { throw 'Broker handshake failed.' }
    $buildId = if ($hello.Response.PSObject.Properties['build_id']) {
        [string]$hello.Response.build_id
    } else { 'unknown (pre-build_id broker)' }
    Write-Host "broker build_id: $buildId"

    for ($cycle = 1; $cycle -le $WarmupCycles; $cycle++) {
        foreach ($max in $MaxValues) {
            foreach ($case in $cases) {
                Send-PipeRequest -Session $session `
                    -Request (New-Request -Query $case.query -Root $case.root -Max $max) `
                    -RequestTimeoutMs $RequestTimeoutMs | Out-Null
            }
        }
    }

    for ($iteration = 1; $iteration -le $Iterations; $iteration++) {
        foreach ($max in $MaxValues) {
            # Interleaved within the iteration so drift affects both arms alike.
            foreach ($case in $cases) {
                $result = Send-PipeRequest -Session $session `
                    -Request (New-Request -Query $case.query -Root $case.root -Max $max) `
                    -RequestTimeoutMs $RequestTimeoutMs
                $response = $result.Response

                if ($response.type -ne 'results') {
                    $errors.Add(('{0}/{1} max={2} iter={3}: type={4}' -f `
                        $case.pair, $case.arm, $max, $iteration, $response.type))
                    continue
                }
                $indexError = ''
                if ($response.PSObject.Properties['index_error'] -and $response.index_error) {
                    $indexError = [string]$response.index_error
                    $errors.Add(('{0}/{1} max={2} iter={3}: {4}' -f `
                        $case.pair, $case.arm, $max, $iteration, $indexError))
                }
                if ($response.PSObject.Properties['is_indexing'] -and $response.is_indexing) {
                    $indexingHits++
                }
                if ($response.PSObject.Properties['index_generation']) {
                    $generations.Add([UInt64]$response.index_generation)
                }

                $rejection = ''
                if ($response.PSObject.Properties['root_rejection'] -and $response.root_rejection) {
                    $rejection = [string]$response.root_rejection
                }

                $samples.Add([ordered]@{
                    schema_version = 1
                    pair = $case.pair
                    arm = $case.arm
                    query = $case.query
                    root = if ($case.root) { [string]$case.root } else { $null }
                    root_rejection = $rejection
                    max = $max
                    iteration = $iteration
                    elapsed_ms = $result.ElapsedMs
                    result_count = @($response.items).Count
                    index_error = $indexError
                    is_indexing = [bool]($response.PSObject.Properties['is_indexing'] -and $response.is_indexing)
                    scanned_nodes = Get-Counter -Response $response -Name 'scanned_nodes'
                    name_candidates = Get-Counter -Response $response -Name 'name_candidates'
                    matched_count = Get-Counter -Response $response -Name 'matched_count'
                    entered_top_k = Get-Counter -Response $response -Name 'entered_top_k'
                    path_constructions = Get-Counter -Response $response -Name 'path_constructions'
                })
            }
        }
    }
} finally {
    Close-PipeSession -Session $session
}

Write-JsonLines -Path $rawPath -Values $samples

# --- aggregate -------------------------------------------------------------

function Get-Stats {
    param([object[]]$Rows, [string]$Field)
    $values = [double[]]@($Rows | ForEach-Object {
        $v = $_[$Field]
        if ($null -ne $v) { [double]$v }
    })
    if ($values.Count -eq 0) { return $null }
    return [ordered]@{
        p50 = [Math]::Round((Get-NearestRankPercentile -Values $values -Percentile 0.50), 1)
        p95 = [Math]::Round((Get-NearestRankPercentile -Values $values -Percentile 0.95), 1)
        max = [Math]::Round(($values | Measure-Object -Maximum).Maximum, 1)
    }
}

$comparisons = foreach ($p in $pairs) {
    foreach ($max in $MaxValues) {
        $rootRows = @($samples | Where-Object { $_.pair -eq $p.pair -and $_.arm -eq 'root' -and $_.max -eq $max })
        $globalRows = @($samples | Where-Object { $_.pair -eq $p.pair -and $_.arm -eq 'global' -and $_.max -eq $max })
        if ($rootRows.Count -eq 0 -or $globalRows.Count -eq 0) { continue }

        $rootLatency = Get-Stats -Rows $rootRows -Field 'elapsed_ms'
        $globalLatency = Get-Stats -Rows $globalRows -Field 'elapsed_ms'
        $rejections = @($rootRows | ForEach-Object { $_.root_rejection } |
            Where-Object { $_ -ne '' } | Select-Object -Unique)

        [ordered]@{
            pair = $p.pair
            query = $p.query
            root = $p.root
            max = $max
            sample_count = $rootRows.Count
            root_rejections = $rejections
            latency_ms = [ordered]@{
                root = $rootLatency
                global = $globalLatency
                # Positive means the scoped query was SLOWER at P95, which is the
                # condition that would justify an ancestor cache.
                p95_delta = [Math]::Round($rootLatency.p95 - $globalLatency.p95, 1)
            }
            path_constructions = [ordered]@{
                root = Get-Stats -Rows $rootRows -Field 'path_constructions'
                global = Get-Stats -Rows $globalRows -Field 'path_constructions'
            }
            entered_top_k = [ordered]@{
                root = Get-Stats -Rows $rootRows -Field 'entered_top_k'
                global = Get-Stats -Rows $globalRows -Field 'entered_top_k'
            }
            result_count = [ordered]@{
                root_max = [int](($rootRows.result_count | Measure-Object -Maximum).Maximum)
                global_max = [int](($globalRows.result_count | Measure-Object -Maximum).Maximum)
            }
        }
    }
}

$endStatus = Get-IndexerStatus -PipeName $IndexerPipeName
$genValues = @($generations)
$summary = [ordered]@{
    schema_version = 1
    kind = 'g4_root_scope_paired_comparison'
    not_a_formal_g0_baseline = $true
    comparability_note = 'Absolute latencies are inflated by background I/O and are NOT comparable to formal G0 runs. Root and global arms are interleaved within each iteration, so the paired delta remains valid.'
    generated_at_utc = [DateTime]::UtcNow.ToString('o')
    broker_build_id = $buildId
    percentile_method = 'nearest_rank'
    iterations = $Iterations
    warmup_cycles = $WarmupCycles
    sample_count = $samples.Count
    notes = $Notes
    index_stability = [ordered]@{
        generation_start = [UInt64]$startStatus.generation
        generation_end = [UInt64]$endStatus.generation
        generation_distinct = @($genValues | Select-Object -Unique).Count
        generation_moved = ([UInt64]$startStatus.generation -ne [UInt64]$endStatus.generation)
        is_indexing_responses = $indexingHits
        volumes = [int]$endStatus.volumes
        memory_bytes = [UInt64]$endStatus.memory_bytes
    }
    errors = @($errors)
    comparisons = @($comparisons)
}

Write-Utf8NoBom -Path $summaryPath -Value (($summary | ConvertTo-Json -Depth 20) + "`n")
Write-Host "Root-scope comparison complete: $summaryPath"
if ($errors.Count -gt 0) {
    Write-Warning ("{0} request-level errors recorded; see summary.errors" -f $errors.Count)
}
