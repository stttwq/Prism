# Step-1 end-to-end check: launch prism-core.exe, do ping/pong + search round-trip
# over the named pipe, mirroring PipeClient.cs framing: UTF-8 no BOM, newline(\n) delimited.
# ASCII-only source to stay safe under Windows PowerShell's ANSI script decoding.
$ErrorActionPreference = 'Stop'

$exe = Join-Path $PSScriptRoot '..\src\prism-core\target\debug\prism-core.exe'
$exe = [System.IO.Path]::GetFullPath($exe)
if (-not (Test-Path $exe)) { throw "backend not found: $exe (run cargo build first)" }

Write-Host "launch backend: $exe"
$proc = Start-Process -FilePath $exe -PassThru -WindowStyle Hidden
try {
    Start-Sleep -Milliseconds 400  # let the pipe server start listening

    $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'prism-core', [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
    $client.Connect(5000)
    Write-Host "connected to \\.\pipe\prism-core"

    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $reader = New-Object System.IO.StreamReader($client, $utf8)
    $writer = New-Object System.IO.StreamWriter($client, $utf8)
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true

    $ok = $true
    # "WeiXin" as a JSON \u escape so we exercise UTF-8 round-trip without non-ASCII in source.
    $cnQuery = '\u5fae\u4fe1'   # decodes to the Chinese word for WeChat

    # --- ping/pong ---
    $writer.WriteLine('{"type":"ping"}')
    $resp = $reader.ReadLine()
    Write-Host "PING   -> $resp"
    if ($resp -notmatch '"type":"pong"')    { $ok = $false; Write-Host "  FAIL expected pong" }
    if ($resp -notmatch '"version":"0\.1\.0"') { $ok = $false; Write-Host "  FAIL expected version 0.1.0" }

    # --- search (skeleton returns empty items, must echo the query verbatim) ---
    $writer.WriteLine('{"type":"search","query":"' + $cnQuery + '","max":100}')
    $resp = $reader.ReadLine()
    Write-Host "SEARCH -> $resp"
    if ($resp -notmatch '"type":"results"') { $ok = $false; Write-Host "  FAIL expected results" }
    if ($resp -notmatch '"items":')         { $ok = $false; Write-Host "  FAIL expected items field" }
    # Server echoes query as raw UTF-8 bytes; both bytes must survive the pipe.
    $bytes = $utf8.GetBytes($resp)
    if (-not ($resp.Contains([char]0x5fae) -and $resp.Contains([char]0x4fe1))) {
        $ok = $false; Write-Host "  FAIL Chinese query not echoed intact (UTF-8 loss)"
    } else {
        Write-Host "  OK  Chinese query round-tripped intact"
    }

    # --- unknown message -> error, not a crash ---
    $writer.WriteLine('{"type":"bogus"}')
    $resp = $reader.ReadLine()
    Write-Host "BOGUS  -> $resp"
    if ($resp -notmatch '"type":"error"')   { $ok = $false; Write-Host "  FAIL expected error" }

    # reader/writer wrap the same pipe stream; dispose the stream once via the client.
    $client.Dispose()

    if ($ok) { Write-Host "`nStep-1 pipe round-trip: PASS" }
    else     { throw "round-trip had failures" }
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc.Kill() }
}
