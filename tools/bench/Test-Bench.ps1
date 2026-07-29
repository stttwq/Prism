[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Bench.Common.psm1') -Force

function Assert-Equal($Expected, $Actual, [string]$Message) {
    if ($Expected -ne $Actual) { throw "$Message Expected '$Expected', got '$Actual'." }
}

$values = [double[]](1..20)
Assert-Equal 10 (Get-NearestRankPercentile -Values $values -Percentile 0.50) 'P50 nearest-rank mismatch.'
Assert-Equal 19 (Get-NearestRankPercentile -Values $values -Percentile 0.95) 'P95 nearest-rank mismatch.'

Assert-MemoryAcceptance -MaximumPrivateWorkingSetBytes (100 * 1024 * 1024)
$memoryGateRejected = $false
try {
    Assert-MemoryAcceptance -MaximumPrivateWorkingSetBytes ((100 * 1024 * 1024) + 1)
} catch {
    $memoryGateRejected = $true
}
if (-not $memoryGateRejected) { throw 'Memory hard-gate excess was not rejected.' }

$samples = @()
foreach ($max in @(8, 1000)) {
    for ($i = 1; $i -le 30; $i++) {
        $samples += [pscustomobject]@{ run_id = 'test'; query_set_version = 'test-v1'; query_id = 'q'; max = $max; phase = 'warm'; iteration = $i; elapsed_ms = $i; result_count = 1 }
    }
}
$samples += [pscustomobject]@{ run_id = 'test'; query_set_version = 'test-v1'; query_id = 'q'; max = 8; phase = 'first_ready'; iteration = 0; elapsed_ms = 999; result_count = 1 }
$summary = New-SearchSummary -Samples $samples -RunId 'test' -QuerySetVersion 'test-v1'
Assert-Equal 60 $summary.warm_sample_count 'Warm sample filtering mismatch.'
Assert-Equal 15 $summary.aggregates[0].p50_ms 'Summary P50 mismatch.'
Assert-Equal 29 $summary.aggregates[0].p95_ms 'Summary P95 mismatch.'
Assert-Equal 30 $summary.aggregates[0].max_ms 'Summary max mismatch.'

$incompleteMatrix = @()
foreach ($queryId in @('q1', 'q2')) {
    foreach ($max in @(8, 1000)) {
        if ($queryId -eq 'q2' -and $max -eq 1000) { continue }
        for ($i = 1; $i -le 30; $i++) {
            $incompleteMatrix += [pscustomobject]@{ run_id = 'test'; query_set_version = 'test-v1'; query_id = $queryId; max = $max; phase = 'warm'; iteration = $i; elapsed_ms = $i; result_count = 1 }
        }
    }
}
$matrixRejected = $false
try { New-SearchSummary -Samples $incompleteMatrix -RunId 'test' -QuerySetVersion 'test-v1' | Out-Null } catch { $matrixRejected = $true }
if (-not $matrixRejected) { throw 'Incomplete query/max sample matrix was not rejected.' }

$fixture = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'queries.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Assert-Equal 1 $fixture.schema_version 'Fixture schema mismatch.'
$categories = @($fixture.queries.categories | ForEach-Object { $_ })
foreach ($required in @('ascii', 'chinese', 'exact', 'prefix', 'contains', 'rare_term', 'no_hit', 'cross_volume')) {
    if ($categories -notcontains $required) { throw "Fixture is missing category: $required" }
}
$ids = @($fixture.queries.id)
Assert-Equal $ids.Count @($ids | Select-Object -Unique).Count 'Fixture ids are not unique.'

$fixtureDocument = $fixture
function Test-FixtureScope([object]$Fixture) {
    return [string]$fixtureDocument.query_set_version
}
Assert-Equal 'g0-v1' (Test-FixtureScope -Fixture $fixture.queries[0]) `
    'A query parameter must not shadow the query-set document.'

$jsonLinesPath = Join-Path $env:TEMP ('PrismBenchJsonLines-' + [guid]::NewGuid().ToString('N') + '.jsonl')
try {
    Write-JsonLines -Path $jsonLinesPath -Values @(
        [ordered]@{ id = 1; value = 'first' },
        [ordered]@{ id = 2; value = 'second' }
    )
    $jsonLines = @(Get-Content -LiteralPath $jsonLinesPath | ForEach-Object { $_ | ConvertFrom-Json })
    Assert-Equal 2 $jsonLines.Count 'Buffered JSONL line count mismatch.'
    Assert-Equal 'second' $jsonLines[1].value 'Buffered JSONL content mismatch.'
} finally {
    Remove-Item -LiteralPath $jsonLinesPath -Force -ErrorAction SilentlyContinue
}

$productPathRejected = $false
try { Resolve-BenchmarkOutputDirectory -Path (Join-Path $env:ProgramData 'Prism\bench-test') | Out-Null } catch { $productPathRejected = $true }
if (-not $productPathRejected) { throw 'Product data output path was not rejected.' }

$releasePathRejected = $false
$releaseRoot = Join-Path $env:TEMP 'PrismBenchReleaseRoot'
try {
    Resolve-BenchmarkOutputDirectory -Path (Join-Path $releaseRoot 'data\bench') `
        -AdditionalProtectedRoots @($releaseRoot) | Out-Null
} catch {
    $releasePathRejected = $true
}
if (-not $releasePathRejected) { throw 'Release-root output path was not rejected.' }

Write-Host 'Bench self-tests passed.'
