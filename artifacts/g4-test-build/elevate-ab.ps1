# Run the A/B comparison elevated, capturing all streams to a log.
#
# Start-Process cannot combine -Verb RunAs with -RedirectStandardOutput (different
# parameter sets), so the elevated child redirects its own output via -Command.
$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$log = Join-Path $here 'ab.log'
$script = Join-Path $here 'ab-compare.ps1'
Remove-Item $log -ErrorAction SilentlyContinue

# *> captures every stream (output, error, warning) into one transcript.
$inner = "& '$script' -Rounds 150 *> '$log'; exit `$LASTEXITCODE"

$proc = Start-Process -FilePath 'powershell.exe' `
    -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', $inner) `
    -Verb RunAs -Wait -PassThru

Write-Output ("exit code: " + $proc.ExitCode)
Write-Output ("log: " + $log)
