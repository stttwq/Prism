param(
    [int]$Iterations = 30,
    [int]$TimeoutMilliseconds = 1000
)

$ErrorActionPreference = 'Stop'
$pipeName = 'prism-indexer-v1'
$testRoot = Join-Path ([IO.Path]::GetTempPath()) (
    'PrismUsnAcceptance-{0}-{1}' -f $PID, [Guid]::NewGuid().ToString('N'))
$movedRoot = Join-Path $testRoot 'moved'
$samples = [System.Collections.Generic.List[double]]::new()

function Invoke-IndexerSearch([string]$Query) {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new(
        '.', $pipeName, [IO.Pipes.PipeDirection]::InOut,
        [IO.Pipes.PipeOptions]::Asynchronous)
    try {
        $pipe.Connect(2000)
        $utf8 = [Text.UTF8Encoding]::new($false)
        $reader = [IO.StreamReader]::new($pipe, $utf8, $false, 1024, $true)
        $writer = [IO.StreamWriter]::new($pipe, $utf8, 1024, $true)
        $writer.NewLine = "`n"
        $writer.AutoFlush = $true
        try {
            $writer.WriteLine('{"type":"hello","protocol":1}')
            $hello = $reader.ReadLine() | ConvertFrom-Json
            if ($hello.type -ne 'hello') { throw 'Indexer hello failed.' }
            $writer.WriteLine((@{ type = 'search'; query = $Query; max = 20 } |
                ConvertTo-Json -Compress))
            $response = $reader.ReadLine() | ConvertFrom-Json
            if ($response.type -eq 'error') { throw $response.message }
            if ($response.type -ne 'results') { throw 'Unexpected search response.' }
            return @($response.items | ForEach-Object { $_.path })
        }
        finally {
            $writer.Dispose()
            $reader.Dispose()
        }
    }
    finally {
        $pipe.Dispose()
    }
}

function Wait-IndexState(
    [string]$Query,
    [scriptblock]$Predicate,
    [string]$Description
) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $paths = @(Invoke-IndexerSearch $Query)
        if (& $Predicate $paths) {
            $timer.Stop()
            $samples.Add($timer.Elapsed.TotalMilliseconds)
            return
        }
        Start-Sleep -Milliseconds 10
    } while ($timer.ElapsedMilliseconds -lt $TimeoutMilliseconds)
    throw "$Description was not visible within ${TimeoutMilliseconds}ms."
}

try {
    New-Item -ItemType Directory -Path $movedRoot -Force | Out-Null
    for ($i = 0; $i -lt $Iterations; $i++) {
        $token = 'prism-usn-{0}-{1}' -f $i, [Guid]::NewGuid().ToString('N')
        $created = Join-Path $testRoot "$token-a.txt"
        $renamed = Join-Path $testRoot "$token-b.txt"
        $moved = Join-Path $movedRoot "$token-b.txt"

        Set-Content -LiteralPath $created -Value $token -Encoding utf8
        Wait-IndexState "$token-a" { param($p) $p -contains $created } 'create'

        Rename-Item -LiteralPath $created -NewName (Split-Path $renamed -Leaf)
        Wait-IndexState $token {
            param($p) ($p -contains $renamed) -and ($p -notcontains $created)
        } 'rename'

        Move-Item -LiteralPath $renamed -Destination $moved
        Wait-IndexState $token {
            param($p) ($p -contains $moved) -and ($p -notcontains $renamed)
        } 'move'

        Remove-Item -LiteralPath $moved
        Wait-IndexState $token { param($p) $p.Count -eq 0 } 'delete'
    }

    $ordered = @($samples | Sort-Object)
    $p95Index = [Math]::Max(0, [Math]::Ceiling($ordered.Count * 0.95) - 1)
    $p95 = $ordered[$p95Index]
    $max = $ordered[-1]
    Write-Host ('USN latency passed: samples={0}, P95={1:N1}ms, max={2:N1}ms' -f
        $ordered.Count, $p95, $max)
    if ($p95 -gt 500 -or $max -gt 1000) {
        throw 'USN latency exceeded the R1 acceptance threshold.'
    }
}
finally {
    $fullRoot = [IO.Path]::GetFullPath($testRoot)
    $fullTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $insideTemp = $fullRoot.StartsWith($fullTemp, [StringComparison]::OrdinalIgnoreCase)
    $hasTestPrefix = (Split-Path $fullRoot -Leaf) -like 'PrismUsnAcceptance-*'
    if ($insideTemp -and $hasTestPrefix) {
        Remove-Item -LiteralPath $fullRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
