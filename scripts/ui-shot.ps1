# Drive the running Prism window: open, type a query, press Down N times, screenshot.
# ASCII-only source (see prism-build.ps1): PowerShell 5.1 under a Chinese locale reads
# BOM-less UTF-8 as GBK and corrupts non-ASCII literals.
param(
    [string]$Query = 'wub',
    [int]$Down = 8,
    [string]$Out = 'E:\LS\DM\listary\artifacts\ui-shot.png'
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$code = @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc lp, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int L,T,R,B; }
}
public class K {
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  // Without this the PowerShell host stays DPI-unaware: GetWindowRect returns
  // virtualized (logical) coordinates and CopyFromScreen grabs a scaled-down region,
  // so the capture clips the right/bottom of a 125% window.
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
}
'@
Add-Type -TypeDefinition $code -ErrorAction SilentlyContinue

# DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = -4
[void][K]::SetProcessDpiAwarenessContext([IntPtr](-4))

$p = Get-Process Prism -ErrorAction Stop
$target = [uint32]$p.Id

function Get-PrismWindow {
    $script:hit = [IntPtr]::Zero
    $script:rect = New-Object 'W+RECT'
    [W]::EnumWindows({
        param($h, $l)
        $wp = [uint32]0
        [void][W]::GetWindowThreadProcessId($h, [ref]$wp)
        if ($wp -eq $target -and [W]::IsWindowVisible($h)) {
            $r = New-Object 'W+RECT'
            [void][W]::GetWindowRect($h, [ref]$r)
            # Skip the 0-size hotkey message window and the off-screen tray helper.
            if (($r.R - $r.L) -gt 200 -and ($r.B - $r.T) -gt 60) {
                $script:hit = $h
                $script:rect = $r
            }
        }
        return $true
    }, [IntPtr]::Zero) | Out-Null
    return @{ Handle = $script:hit; Rect = $script:rect }
}

# Default HotkeyMode is DoubleCtrl (Settings.Default), so Alt+Space is not registered
# unless the user switched to Combo mode. SendKeys cannot express "Ctrl pressed and
# released twice" -- it only sends Ctrl as a modifier for another key -- so drive the
# raw key events through keybd_event.
function Invoke-DoubleCtrl {
    $VK_CONTROL = 0x11
    $KEYUP = 0x0002
    for ($i = 0; $i -lt 2; $i++) {
        [K]::keybd_event($VK_CONTROL, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 40
        [K]::keybd_event($VK_CONTROL, 0, $KEYUP, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 90
    }
}
Invoke-DoubleCtrl
Start-Sleep -Milliseconds 900

$win = Get-PrismWindow
if ($win.Handle -eq [IntPtr]::Zero) {
    # Double-Ctrl toggles: if a previous run left the window open, the first tap closed it.
    Invoke-DoubleCtrl
    Start-Sleep -Milliseconds 900
    $win = Get-PrismWindow
}
if ($win.Handle -eq [IntPtr]::Zero) { Write-Host 'NO VISIBLE WINDOW'; exit 1 }
[void][W]::SetForegroundWindow($win.Handle)
Start-Sleep -Milliseconds 300

# The IME can be in Chinese mode, which turns "wub" into a pinyin candidate list
# instead of a query (and the candidate window then covers the results). A bare
# Shift tap is Microsoft Pinyin's Chinese/English toggle. Detect by checking the
# window height after typing: an IME-eaten query leaves the panel collapsed.
function Send-ShiftTap {
    $VK_SHIFT = 0x10
    $KEYUP = 0x0002
    [K]::keybd_event($VK_SHIFT, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 30
    [K]::keybd_event($VK_SHIFT, 0, $KEYUP, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
}

function Send-Query {
    [System.Windows.Forms.SendKeys]::SendWait($Query)
    Start-Sleep -Milliseconds 2200
    $w = Get-PrismWindow
    return ($w.Rect.B - $w.Rect.T)
}

$height = Send-Query
if ($height -lt 300) {
    Write-Host "panel height $height -- query likely eaten by IME, toggling to English"
    # Clear whatever the IME left behind, flip to English, retype.
    [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
    Start-Sleep -Milliseconds 200
    Send-ShiftTap
    for ($i = 0; $i -lt 12; $i++) { [System.Windows.Forms.SendKeys]::SendWait('{BACKSPACE}') }
    Start-Sleep -Milliseconds 200
    $win = Get-PrismWindow
    if ($win.Handle -eq [IntPtr]::Zero) {
        Invoke-DoubleCtrl
        Start-Sleep -Milliseconds 900
        $win = Get-PrismWindow
        if ($win.Handle -eq [IntPtr]::Zero) { Write-Host 'WINDOW GONE'; exit 1 }
        [void][W]::SetForegroundWindow($win.Handle)
        Start-Sleep -Milliseconds 300
    }
    $height = Send-Query
    Write-Host "panel height after retry: $height"
}

for ($i = 0; $i -lt $Down; $i++) {
    [System.Windows.Forms.SendKeys]::SendWait('{DOWN}')
    Start-Sleep -Milliseconds 160
}
Start-Sleep -Milliseconds 500

$win = Get-PrismWindow
$r = $win.Rect
$pad = 30
$x = [Math]::Max(0, $r.L - $pad)
$y = [Math]::Max(0, $r.T - $pad)
$w = ($r.R - $r.L) + $pad * 2
$h = ($r.B - $r.T) + $pad * 2
Write-Host ("rect={0},{1}-{2},{3} size={4}x{5}" -f $r.L, $r.T, $r.R, $r.B, ($r.R-$r.L), ($r.B-$r.T))

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size $w, $h))
New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Host "saved $Out"
