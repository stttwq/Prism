# G5 memory soak: does repeated window-mode querying grow the broker without bound?
#
# WHY THIS EXISTS
# ---------------
# The PRD gate is "long-running repeated queries must not cause sustained memory growth
# from a resident window list or event subscriptions" — a *trend*, not a threshold. A
# single sample cannot fail that gate: a broker that leaks 2MB per query looks fine on
# sample one. So this drives N real window-mode queries over the pipe and reports the
# working-set slope.
#
# The design claim being tested: `WindowSnapshotStore` holds only the newest enumeration
# and replaces it wholesale, so memory must plateau rather than climb with query count.
#
# ASCII-only source (see prism-build.ps1): PowerShell 5.1 under a Chinese locale reads
# BOM-less UTF-8 as GBK and corrupts non-ASCII literals.
#
# USAGE
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\g5-memory-soak.ps1 `
#     -BrokerPath "D:\<temp install>\prism-core.exe" [-Queries 400]

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BrokerPath,
    [int]$Queries = 400,
    # Sustained growth allowance across the whole soak. Generous on purpose: this gate is
    # about unbounded growth, not about a few hundred KB of allocator noise.
    [int]$MaxGrowthKB = 4096
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not (Test-Path -LiteralPath $BrokerPath)) { throw "broker not found: $BrokerPath" }

$existing = Get-Process -Name 'prism-core' -ErrorAction SilentlyContinue
if ($existing) { throw "a prism-core is already running (pid $($existing.Id)); refusing to fight over the pipe" }

Write-Host "broker:  $BrokerPath"
Write-Host "queries: $Queries"

$proc = Start-Process -FilePath $BrokerPath -PassThru -WindowStyle Hidden `
    -WorkingDirectory (Split-Path -Parent $BrokerPath)
$samples = New-Object System.Collections.ArrayList
try {
    Start-Sleep -Milliseconds 600

    $client = New-Object System.IO.Pipes.NamedPipeClientStream(
        '.', 'prism-core',
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::Asynchronous)
    $client.Connect(5000)

    $utf8   = New-Object System.Text.UTF8Encoding($false)
    $reader = New-Object System.IO.StreamReader($client, $utf8)
    $writer = New-Object System.IO.StreamWriter($client, $utf8)
    $writer.NewLine = [string][char]10   # broker frames on \n
    $writer.AutoFlush = $true

    function Send([string]$json) {
        $writer.WriteLine($json)
        return $reader.ReadLine()
    }

    $null = Send '{"type":"hello","protocol":1}'

    # Warm up first: the first queries fault in code paths and grow the heap once. Measuring
    # from a cold start would report that one-time cost as a leak.
    for ($i = 0; $i -lt 20; $i++) {
        $null = Send '{"type":"search","query":"e","max":20,"mode":"window"}'
    }
    [void]$proc.Refresh()
    $baselineKB = [int]($proc.WorkingSet64 / 1KB)
    Write-Host "baseline after warmup: ${baselineKB}KB"

    # Vary the query so this is not one cached path repeated: different result counts,
    # empty query (history-gated recent list), and token resolution all allocate.
    $rotation = @('e', 'a', '', 'zzzz-no-match', 'o', 'set')
    for ($i = 1; $i -le $Queries; $i++) {
        $q = $rotation[$i % $rotation.Count]
        $resp = Send ('{"type":"search","query":"' + $q + '","max":20,"mode":"window"}')

        # Resolve a token when one is offered: exercises the snapshot lookup path too.
        if ($resp -match '"target":\{"kind":"window","value":"(\d+)"\}') {
            $null = Send ('{"type":"resolve_window","target":{"kind":"window","value":"' + $Matches[1] + '"}}')
        }

        if ($i % 50 -eq 0) {
            [void]$proc.Refresh()
            $kb = [int]($proc.WorkingSet64 / 1KB)
            [void]$samples.Add([pscustomobject]@{ Query = $i; WorkingSetKB = $kb })
            Write-Host ("  after {0,4} queries: {1}KB  (delta {2:+#;-#;0}KB)" -f $i, $kb, ($kb - $baselineKB))
        }
    }

    $client.Dispose()
}
finally {
    if (-not $proc.HasExited) { $proc.Kill(); $proc.WaitForExit(5000) }
}

if ($samples.Count -lt 2) { throw 'not enough samples to judge a trend' }

$first = $samples[0].WorkingSetKB
$last  = $samples[$samples.Count - 1].WorkingSetKB
$peak  = ($samples | Measure-Object -Property WorkingSetKB -Maximum).Maximum
$growth = $last - $first

Write-Host ''
Write-Host "first sample: ${first}KB"
Write-Host "last sample:  ${last}KB"
Write-Host "peak:         ${peak}KB"
Write-Host "growth:       ${growth}KB over $Queries queries"

# The real question is the shape, not the endpoint: a leak climbs monotonically, whereas
# allocator noise oscillates. Report whether the second half is still climbing.
$half = [int]($samples.Count / 2)
$firstHalfAvg  = [int](($samples[0..($half - 1)] | Measure-Object -Property WorkingSetKB -Average).Average)
$secondHalfAvg = [int](($samples[$half..($samples.Count - 1)] | Measure-Object -Property WorkingSetKB -Average).Average)
Write-Host "first-half avg ${firstHalfAvg}KB vs second-half avg ${secondHalfAvg}KB"

if ($growth -gt $MaxGrowthKB) {
    Write-Host ''
    Write-Host "FAIL sustained growth ${growth}KB exceeds ${MaxGrowthKB}KB" -ForegroundColor Red
    exit 1
}

Write-Host ''
Write-Host "PASS no unbounded growth (${growth}KB over $Queries queries)" -ForegroundColor Green
