$ErrorActionPreference = 'Stop'
try {
  $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'prism-core', [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
  $c.Connect(2000)
  $u = New-Object System.Text.UTF8Encoding($false)
  $r = New-Object System.IO.StreamReader($c, $u)
  $w = New-Object System.IO.StreamWriter($c, $u)
  $w.NewLine = "`n"
  $w.AutoFlush = $true
  $w.WriteLine('{"type":"search","query":"readme","max":3}')
  $line = $r.ReadLine()
  $n = ([regex]::Matches($line, '"kind"')).Count
  Write-Host "search_items=$n bytes=$($line.Length)"
  if ($n -gt 0) {
    Write-Host $line.Substring(0, [Math]::Min(300, $line.Length))
  } else {
    Write-Host $line
  }
  $c.Dispose()
} catch {
  Write-Host "search_err: $_"
}
