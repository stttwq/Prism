param(
    [int]$TimeoutMilliseconds = 5000
)

$ErrorActionPreference = 'Stop'
$pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
    '.',
    'prism-indexer-v1',
    [System.IO.Pipes.PipeDirection]::InOut,
    [System.IO.Pipes.PipeOptions]::Asynchronous)
$reader = $null
$writer = $null

try {
    $pipe.Connect($TimeoutMilliseconds)
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    $reader = [System.IO.StreamReader]::new($pipe, $utf8, $false, 1024, $true)
    $writer = [System.IO.StreamWriter]::new($pipe, $utf8, 1024, $true)
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true

    function Invoke-IndexerRequest([hashtable]$Request) {
        $writer.WriteLine(($Request | ConvertTo-Json -Compress))
        $line = $reader.ReadLine()
        if ($null -eq $line) { throw 'Indexer closed the pipe before responding.' }
        $response = $line | ConvertFrom-Json
        if ($response.type -eq 'error') { throw "Indexer error: $($response.message)" }
        return $response
    }

    $hello = Invoke-IndexerRequest @{ type = 'hello'; protocol = 1 }
    if ($hello.type -ne 'hello' -or $hello.protocol -ne 1) {
        throw "Unexpected hello response: $($hello | ConvertTo-Json -Compress)"
    }

    $status = Invoke-IndexerRequest @{ type = 'status' }
    if ($status.type -ne 'status' -or $null -eq $status.generation) {
        throw "Unexpected status response: $($status | ConvertTo-Json -Compress)"
    }

    $generation = Invoke-IndexerRequest @{
        type = 'wait_generation'
        after = [uint64]$status.generation
        timeout_ms = 50
    }
    if ($generation.type -ne 'generation' -or $null -eq $generation.generation) {
        throw "Unexpected generation response: $($generation | ConvertTo-Json -Compress)"
    }

    Write-Host "Indexer protocol roundtrip passed (generation=$($generation.generation))."
}
finally {
    if ($null -ne $writer) { $writer.Dispose() }
    if ($null -ne $reader) { $reader.Dispose() }
    $pipe.Dispose()
}
