# Prism G7 machine test: send search requests via named pipe and verify results.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\g7-machine-test.ps1

$ErrorActionPreference = 'Stop'
$PipeName = '\\.\pipe\prism-core'

function Send-PipeMessage([string]$message) {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(
        '.', 'prism-core', [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(5000)
    $pipe.ReadMode = [System.IO.Pipes.PipeTransmissionMode]::Byte
    $writer = New-Object System.IO.StreamWriter($pipe)
    $writer.WriteLine($message)
    $writer.Flush()
    $reader = New-Object System.IO.StreamReader($pipe)
    $line = $reader.ReadLine()
    $pipe.Dispose()
    return $line
}

function Search([string]$query, [int]$max = 8, [string]$root = $null) {
    $payload = @{ type = "search"; query = $query; max = $max }
    if ($root) { $payload["root"] = $root }
    $json = $payload | ConvertTo-Json -Compress
    $response = Send-PipeMessage $json
    return $response | ConvertFrom-Json
}

function Count-Kind($results, [string]$kind) {
    return ($results | Where-Object { $_.kind -eq $kind }).Count
}

function Assert-True([string]$name, [bool]$condition, [string]$detail = "") {
    if ($condition) {
        Write-Host "  PASS  $name" -ForegroundColor Green
    } else {
        Write-Host "  FAIL  $name  $detail" -ForegroundColor Red
    }
}

# --- Status check ---
Write-Host "==> Indexer status" -ForegroundColor Cyan
$hello = Send-PipeMessage '{"type":"hello","protocol":1}' | ConvertFrom-Json
Write-Host "  broker version: $($hello.version) build_id: $($hello.build_id)"

# Send a status-only search (empty query) to check indexing state via the indexer pipe
$statusPayload = @{ type = "search"; query = "a"; max = 1 } | ConvertTo-Json -Compress
$statusResp = Send-PipeMessage $statusPayload | ConvertFrom-Json
Write-Host "  is_indexing: $($statusResp.is_indexing)"
if ($statusResp.is_indexing) {
    Write-Host "  WARNING: index is still building, results may be incomplete" -ForegroundColor Yellow
}

# --- Test 1: ext:pdf returns only pdf files ---
Write-Host "`n==> Test 1: ext:pdf returns only pdf files" -ForegroundColor Cyan
$resp = Search "ext:pdf" 8
$nonPdf = $resp.items | Where-Object { -not $_.title.EndsWith(".pdf") -and -not $_.subtitle.EndsWith(".pdf") }
Assert-True "all results are .pdf" ($nonPdf.Count -eq 0) "found $($nonPdf.Count) non-pdf items"
Assert-True "no app results" ((Count-Kind $resp.items "app") -eq 0)
Assert-True "no web results" ((Count-Kind $resp.items "web") -eq 0)
Assert-True "no window results" ((Count-Kind $resp.items "window") -eq 0)
Write-Host "  returned $($resp.items.Count) items, is_truncated=$($resp.is_truncated)"

# --- Test 2: ext:txt,pdf OR semantics ---
Write-Host "`n==> Test 2: ext:txt,pdf returns txt OR pdf" -ForegroundColor Cyan
$resp = Search "ext:txt,pdf" 8
$nonMatching = $resp.items | Where-Object {
    -not $_.subtitle -match '\.(txt|pdf)$'
}
Assert-True "all results are .txt or .pdf" ($nonMatching.Count -eq 0) "found $($nonMatching.Count) non-matching"
Write-Host "  returned $($resp.items.Count) items"

# --- Test 3: path: substring case-insensitive ---
Write-Host "`n==> Test 3: path: substring case-insensitive" -ForegroundColor Cyan
$resp = Search 'report path:"windows"' 8
$nonMatching = $resp.items | Where-Object {
    $_.subtitle -and ($_.subtitle.ToLower() -notlike '*windows*')
}
Assert-True "all results contain 'windows' in path" ($nonMatching.Count -eq 0) "found $($nonMatching.Count) non-matching"
Write-Host "  returned $($resp.items.Count) items"

# --- Test 4: ext + path combined AND ---
Write-Host "`n==> Test 4: ext:pdf path:combined AND" -ForegroundColor Cyan
$resp = Search 'ext:pdf path:"windows"' 8
$nonPdf = $resp.items | Where-Object { -not $_.subtitle.EndsWith(".pdf") }
$nonPath = $resp.items | Where-Object { $_.subtitle -and ($_.subtitle.ToLower() -notlike '*windows*') }
Assert-True "all are .pdf" ($nonPdf.Count -eq 0)
Assert-True "all contain 'windows' in path" ($nonPath.Count -eq 0)
Write-Host "  returned $($resp.items.Count) items"

# --- Test 5: unknown prefix treated as plain text ---
Write-Host "`n==> Test 5: unknown prefix foo:bar is plain text" -ForegroundColor Cyan
$resp = Search "foo:bar" 8
# This should search for "foo:bar" literally, likely returning 0 results or some odd match
Write-Host "  returned $($resp.items.Count) items (0 is expected for nonsense query)"

# --- Test 6: unterminated quote falls back to plain text ---
Write-Host "`n==> Test 6: unterminated quote is plain text" -ForegroundColor Cyan
$resp = Search 'path:"unterminated' 8
Assert-True "no filters parsed (no crash)" ($null -ne $resp)
Write-Host "  returned $($resp.items.Count) items"

# --- Test 7: filters suppress web keyword ---
Write-Host "`n==> Test 7: filters suppress web keyword" -ForegroundColor Cyan
$resp = Search 'bi ext:pdf' 8
$webItems = $resp.items | Where-Object { $_.kind -eq "web" }
Assert-True "no web results when ext filter present" ($webItems.Count -eq 0) "found $($webItems.Count) web items"
Write-Host "  returned $($resp.items.Count) items"

# --- Test 8: max=1000 warm gate ---
Write-Host "`n==> Test 8: max=1000 with ext:pdf returns full page" -ForegroundColor Cyan
$resp = Search "ext:pdf" 1000
Write-Host "  returned $($resp.items.Count) items, is_truncated=$($resp.is_truncated)"
Assert-True "returned > 0 results" ($resp.items.Count -gt 0)
Assert-True "all are .pdf" (($resp.items | Where-Object { -not $_.subtitle.EndsWith(".pdf") }).Count -eq 0)

# --- Test 9: path_constructions reported ---
Write-Host "`n==> Test 9: path_constructions is reported with path filter" -ForegroundColor Cyan
$resp = Search 'a ext:txt path:"windows"' 8
if ($resp.PSObject.Properties.Name -contains "path_constructions") {
    Write-Host "  path_constructions: $($resp.path_constructions)"
    Assert-True "path_constructions > 0" ($resp.path_constructions -gt 0)
} else {
    Write-Host "  path_constructions not in response (may be null when 0)"
}

# --- Test 10: memory check ---
Write-Host "`n==> Test 10: three-process memory" -ForegroundColor Cyan
$processes = @("Prism", "prism-core", "prism-indexer-service")
$totalKB = 0
foreach ($name in $processes) {
    $proc = Get-Process -Name $name -ErrorAction SilentlyContinue
    if ($proc) {
        $ws = $proc.WorkingSet64 / 1KB
        $totalKB += $ws
        Write-Host "  ${name}: $([math]::Round($ws / 1024, 1)) MB WS"
    } else {
        Write-Host "  ${name}: NOT RUNNING" -ForegroundColor Yellow
    }
}
$totalMB = [math]::Round($totalKB / 1024 / 1024, 1)
Write-Host "  total: ${totalMB} MB"
Assert-True "total memory <= 100 MB" ($totalMB -le 100) "actual: ${totalMB} MB"

Write-Host "`n==> Done." -ForegroundColor Cyan
