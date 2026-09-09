# Double-Ctrl end-to-end probe for the WH_KEYBOARD_LL thread fix.
#
# WHAT IT PROVES
# --------------
# HotkeyHookThreadTests covers the hook binding deterministically, but it never
# starts the real app, so it cannot see whether double-Ctrl actually puts the
# search window on screen. The reported symptom was exactly that: the window
# stopped appearing after a while, and a tray click revived it for about a
# minute. So this probe drives the shipped exe and asserts window visibility,
# including one round AFTER the 60s hook-refresh timer has fired - the tick that
# used to move the hook onto a thread with no message pump and kill it.
#
# It runs the build output from a temp directory and never writes to an existing
# install. Any Prism already running is stopped first (the single-instance mutex
# would otherwise make the temp copy exit immediately) and restarted at the end.
#
# ASCII only on purpose (see prism-build.ps1): PowerShell 5.1 under a Chinese
# locale reads BOM-less UTF-8 as GBK and corrupts non-ASCII literals.
#
# USAGE
#   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\hotkey-doubleclick-probe.ps1
#   pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\hotkey-doubleclick-probe.ps1 -SkipTimerRound
#
# Takes ~90s with the timer round, ~15s without. Injects Ctrl taps into the
# session, so do not type during the run.

[CmdletBinding()]
param([switch]$SkipTimerRound)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$AppOut   = Join-Path $RepoRoot 'src\Prism\bin\Release\net8.0-windows'
$CoreOut  = Join-Path $RepoRoot 'src\prism-core\target\release'
$TempDir  = Join-Path $env:TEMP 'prism-hotkey-probe'

$code = @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class Probe {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc lp, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);

  // Two Ctrl taps inside DoubleClickMs (400ms), each held under MaxHoldMs (300ms).
  public static void DoubleCtrl() {
    Tap(); System.Threading.Thread.Sleep(80); Tap();
  }
  static void Tap() {
    keybd_event(0x11, 0, 0, IntPtr.Zero);
    System.Threading.Thread.Sleep(30);
    keybd_event(0x11, 0, 2, IntPtr.Zero);
  }

  public static int VisibleWindows(uint pid) {
    int count = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      uint wp; GetWindowThreadProcessId(h, out wp);
      if (wp == pid && IsWindowVisible(h)) count++;
      return true;
    }, IntPtr.Zero);
    return count;
  }
}
'@
Add-Type -TypeDefinition $code

function Wait-Visible([int]$procId, [int]$want, [int]$timeoutMs = 8000) {
    # 8s, not 3s: under CPU load ShowAndFocus can take seconds, and a slow window is a
    # different defect from a window that never appears. A tighter timeout conflates them -
    # three early red runs reported "stuck at 0" on a machine busy with a build, and the same
    # binary then passed repeatedly when idle.
    $deadline = [Environment]::TickCount + $timeoutMs
    $last = -1
    do {
        $last = [Probe]::VisibleWindows([uint32]$procId)
        if ($last -eq $want) { return $true }
        Start-Sleep -Milliseconds 100
    } while ([Environment]::TickCount -lt $deadline)
    Write-Host "  (visible window count stuck at $last, wanted $want)"
    return $false
}

# Restart whatever was running when we are done, from wherever it was running.
$previous = @(Get-Process Prism -ErrorAction SilentlyContinue | ForEach-Object { $_.Path } | Where-Object { $_ })
$failures = New-Object System.Collections.Generic.List[string]

try {
    if ($previous.Count -gt 0) {
        Write-Host "stopping running Prism: $($previous -join ', ')"
        Get-Process Prism, prism-core -ErrorAction SilentlyContinue | Stop-Process -Force
        Start-Sleep -Milliseconds 800
    }

    if (Test-Path $TempDir) { Remove-Item $TempDir -Recurse -Force }
    New-Item -ItemType Directory -Path $TempDir -Force | Out-Null
    Copy-Item (Join-Path $AppOut '*') $TempDir -Recurse -Force
    $core = Join-Path $CoreOut 'prism-core.exe'
    if (Test-Path $core) { Copy-Item $core $TempDir -Force }
    Write-Host "temp install: $TempDir"

    $app = Start-Process (Join-Path $TempDir 'Prism.exe') -PassThru
    Start-Sleep -Seconds 4   # tray-resident startup; window is lazily created
    if ($app.HasExited) { throw "Prism exited at startup (code $($app.ExitCode))" }
    Write-Host "started pid=$($app.Id)"

    # Round 1-3: repeated invocations, which is when the user saw it degrade.
    # A failed round is retried once: a retry that works means a single tap pair was lost
    # (transient), a retry that also fails means the hook is gone (the reported symptom).
    for ($i = 1; $i -le 3; $i++) {
        [Probe]::DoubleCtrl()
        if (Wait-Visible $app.Id 1) { Write-Host "round $i show: OK" }
        else {
            [Probe]::DoubleCtrl()
            if (Wait-Visible $app.Id 1) { $failures.Add("round $i - first double-Ctrl lost, retry showed the window") }
            else { $failures.Add("round $i - window did not become visible, retry failed too") }
        }

        [Probe]::DoubleCtrl()
        if (Wait-Visible $app.Id 0) { Write-Host "round $i hide: OK" }
        else { $failures.Add("round $i - window did not hide") }
    }

    # Control path: a second Prism.exe signals the running instance to toggle, which reaches
    # ToggleSearchWindow without touching the keyboard hook. If this flakes too, the fault is in
    # show/hide rather than in the hook.
    for ($i = 1; $i -le 3; $i++) {
        Start-Process (Join-Path $TempDir 'Prism.exe') | Out-Null
        if (Wait-Visible $app.Id 1) { Write-Host "signal $i show: OK" }
        else { $failures.Add("signal $i - window did not become visible (hook not involved)") }
        Start-Process (Join-Path $TempDir 'Prism.exe') | Out-Null
        if (Wait-Visible $app.Id 0) { Write-Host "signal $i hide: OK" }
        else { $failures.Add("signal $i - window did not hide (hook not involved)") }
    }

    if (-not $SkipTimerRound) {
        # Idle rounds. 30s is before the 60s refresh timer fires, 65s after it. Testing both
        # separates "the hook dies while idle" from "the refresh is what kills it" - a single
        # post-refresh check cannot tell those apart.
        Write-Host 'waiting 30s (before the hook refresh timer fires)...'
        Start-Sleep -Seconds 30
        [Probe]::DoubleCtrl()
        if (Wait-Visible $app.Id 1) {
            Write-Host 'idle-30s show: OK'
            [Probe]::DoubleCtrl()
            if (-not (Wait-Visible $app.Id 0)) { $failures.Add('idle-30s - window did not hide') }
        }
        else { $failures.Add('after 30s idle (pre-refresh) - window did not become visible') }

        Write-Host 'waiting 35s more for the hook refresh timer to fire...'
        Start-Sleep -Seconds 35
        [Probe]::DoubleCtrl()
        if (Wait-Visible $app.Id 1) { Write-Host 'post-refresh show: OK' }
        else { $failures.Add('after the 60s hook refresh - window did not become visible') }
    }
}
finally {
    Get-Process Prism, prism-core -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    if (Test-Path $TempDir) { Remove-Item $TempDir -Recurse -Force -ErrorAction SilentlyContinue }
    foreach ($path in $previous) {
        if (Test-Path $path) { Start-Process $path | Out-Null; Write-Host "restarted $path" }
    }
}

if ($failures.Count -gt 0) {
    Write-Host ''
    Write-Host 'FAIL' -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    exit 1
}
Write-Host ''
Write-Host 'PASS - double-Ctrl showed and hid the window every round' -ForegroundColor Green
