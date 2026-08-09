# Launch prism-build.ps1 elevated and capture its output.
#
# Start-Process cannot combine -Verb RunAs with -RedirectStandardOutput (they are
# different parameter sets), so the elevated child redirects its own output via
# a nested -Command instead.
$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$log = Join-Path $here 'install.log'
$build = (Resolve-Path (Join-Path $here '..\..\scripts\prism-build.ps1')).Path
Remove-Item $log -ErrorAction SilentlyContinue

# *> captures every stream (output, error, warning) into one transcript.
$inner = "& '$build' -Bootstrap *> '$log'; exit `$LASTEXITCODE"

$proc = Start-Process -FilePath 'powershell.exe' `
    -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', $inner) `
    -Verb RunAs -Wait -PassThru

Write-Output ("exit code: " + $proc.ExitCode)
