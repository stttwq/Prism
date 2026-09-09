# Same probe as hotkey-doubleclick-probe.ps1, run while the CPU is saturated.
#
# WHY
# ---
# Windows removes a low-level keyboard hook whose callback overruns
# LowLevelHooksTimeout (~300ms). The callback itself is a few instructions, so the
# realistic way to overrun it is scheduling delay under load - which is exactly
# when the hook died during development (three probe failures in a row landed
# right after a build+test+installer sequence, and the same probe then passed
# eight times in a row on an idle machine).
#
# This wrapper saturates every logical core, then runs the probe. A green run
# means the hook survives load; a red one means the 60s refresh timer is the only
# thing that heals it and the user gets up to a minute of dead double-Ctrl.
#
# ASCII only on purpose (see prism-build.ps1).
#
# USAGE
#   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\hotkey-load-probe.ps1
#   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\hotkey-load-probe.ps1 -SkipTimerRound

[CmdletBinding()]
param([switch]$SkipTimerRound)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$cores = (Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors
Write-Host "saturating $cores logical cores"

$jobs = @()
try {
    for ($i = 0; $i -lt $cores; $i++) {
        $jobs += Start-Job -ScriptBlock {
            $end = [DateTime]::UtcNow.AddMinutes(4)
            $x = 0.0
            while ([DateTime]::UtcNow -lt $end) { $x = [Math]::Sqrt($x + 1.234) }
            $x
        }
    }
    Start-Sleep -Seconds 3   # let the load actually ramp

    $probe = Join-Path $PSScriptRoot 'hotkey-doubleclick-probe.ps1'
    $probeArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $probe)
    if ($SkipTimerRound) { $probeArgs += '-SkipTimerRound' }
    # Run as a native child so its exit code lands in $LASTEXITCODE; StrictMode makes
    # reading $LASTEXITCODE after an in-process script call a hard error.
    & pwsh @probeArgs
    $probeExit = $LASTEXITCODE
}
finally {
    $jobs | Stop-Job -ErrorAction SilentlyContinue
    $jobs | Remove-Job -Force -ErrorAction SilentlyContinue
}

exit $probeExit
