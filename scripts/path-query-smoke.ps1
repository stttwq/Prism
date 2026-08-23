# P1 live smoke: path queries through the real broker (requires running Prism stack).
# ASCII-only source (see prism-build.ps1): PS 5.1 under a Chinese locale reads BOM-less
# UTF-8 as GBK and corrupts non-ASCII literals.
param(
    [string]$DirQuery = "$PSScriptRoot\..\src\Prism",
    [string]$FileQuery = "$PSScriptRoot\..\src\Prism\Models\Settings.cs",
    [string]$PartialQuery = "$PSScriptRoot\..\src\Prism\Models\Set"
)

$ErrorActionPreference = 'Stop'

function Invoke-Search([string]$query) {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'prism-core', [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
    $c.Connect(2000)
    $u = New-Object System.Text.UTF8Encoding($false)
    $r = New-Object System.IO.StreamReader($c, $u)
    $w = New-Object System.IO.StreamWriter($c, $u)
    $w.NewLine = "`n"
    $w.AutoFlush = $true
    $payload = @{ type = 'search'; query = $query; max = 8 } | ConvertTo-Json -Compress
    $w.WriteLine($payload)
    $line = $r.ReadLine()
    $c.Dispose()
    $obj = $null
    try { $obj = $line | ConvertFrom-Json } catch {}
    if ($null -eq $obj -or $null -eq $obj.items) {
        Write-Host "[$query] BAD RESPONSE: $($line.Substring(0, [Math]::Min(200, $line.Length)))"
        return $null
    }
    $titles = @($obj.items | ForEach-Object { "$($_.kind):$($_.subtitle)" })
    Write-Host "[$query] items=$($titles.Count)"
    for ($i = 0; $i -lt [Math]::Min(4, $titles.Count); $i++) {
        Write-Host "    $i $($titles[$i])"
    }
    return $obj
}

$dir = Invoke-Search $DirQuery
if ($null -ne $dir) {
    if (@($dir.items).Count -gt 0 -and $dir.items[0].subtitle -eq $DirQuery) {
        Write-Host 'DIR_OK: first row is the directory itself'
    } else {
        Write-Host 'DIR_FAIL: first row is not the directory'
    }
}

$file = Invoke-Search $FileQuery
if ($null -ne $file) {
    $hit = @($file.items) | Where-Object { $_.subtitle -eq $FileQuery } | Select-Object -First 1
    if ($null -ne $hit -and $file.items[0].subtitle -eq $FileQuery) {
        Write-Host 'FILE_OK: exact file is the first row'
    } else {
        Write-Host 'FILE_FAIL: exact file not first'
    }
}

$partial = Invoke-Search $PartialQuery
if ($null -ne $partial) {
    $narrowed = @(@($partial.items) | Where-Object { $_.subtitle -like '*\Set*' })
    if (@($partial.items).Count -gt 0 -and $narrowed.Count -eq @($partial.items).Count) {
        Write-Host 'PARTIAL_OK: every row narrows to the tail segment'
    } else {
        Write-Host "PARTIAL_FAIL: $($partial.items.Count) rows, $($narrowed.Count) narrowed"
    }
}
