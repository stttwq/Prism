param(
    [Parameter(Mandatory=$true)][string[]]$Queries,
    [int]$Max = 8,
    [int]$TimeoutMs = 30000
)
$stream = [IO.Pipes.NamedPipeClientStream]::new('.', 'prism-core', [IO.Pipes.PipeDirection]::InOut, [IO.Pipes.PipeOptions]::Asynchronous)
$stream.Connect(10000)
$utf8 = [Text.UTF8Encoding]::new($false)
$reader = [IO.StreamReader]::new($stream, $utf8, $false, 4096, $true)
$writer = [IO.StreamWriter]::new($stream, $utf8, 4096, $true)
$writer.NewLine = "`n"
$writer.AutoFlush = $true
function Send([object]$request) {
    $script:writer.WriteLine(($request | ConvertTo-Json -Depth 10 -Compress))
    $task = $script:reader.ReadLineAsync()
    if (-not $task.Wait($TimeoutMs)) { throw "timeout" }
    return ($task.Result | ConvertFrom-Json)
}
$hello = Send @{ type = 'hello'; protocol = 1 }
if ($hello.type -ne 'hello') { throw "handshake failed: $($hello | ConvertTo-Json -Compress)" }
foreach ($query in $Queries) {
    $response = Send @{ type = 'search'; query = $query; max = $Max }
    if ($response.type -ne 'results') { Write-Output "[$query] ERROR: $($response.message)"; continue }
    $names = @($response.items | ForEach-Object { $_.title })
    Write-Output ("[{0}] {1} hits: {2}" -f $query, $names.Count, ($names -join ' | '))
}
$reader.Dispose(); $writer.Dispose(); $stream.Dispose()
