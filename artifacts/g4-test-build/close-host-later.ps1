# Delayed host-window close, for the "host closed race" matrix rows (E7 / O5).
#
# Why this exists: Prism hides as soon as it loses focus, so you cannot keep it
# open and click the host's X button at the same time. This script counts down,
# giving you time to summon Prism, then closes the host window by itself
# without ever stealing focus.
#
# ASCII only on purpose: PowerShell 5.1 under a Chinese locale reads BOM-less
# UTF-8 scripts as GBK, which corrupts non-ASCII literals and breaks parsing.
#
# Usage:
#   .\close-host-later.ps1
#   .\close-host-later.ps1 -Seconds 12
#   .\close-host-later.ps1 -HostKind Opus

param(
    [int]$Seconds = 8,
    [ValidateSet('Explorer', 'Opus')]
    [string]$HostKind = 'Explorer'
)

$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class Closer
{
    delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    static extern int GetClassName(IntPtr h, StringBuilder b, int m);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    static extern int GetWindowText(IntPtr h, StringBuilder b, int m);
    [DllImport("user32.dll")]
    static extern IntPtr PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

    const uint WM_CLOSE = 0x0010;

    public static List<string> Find(string[] classes)
    {
        var found = new List<string>();
        EnumWindows((h, p) => {
            var c = new StringBuilder(256);
            GetClassName(h, c, c.Capacity);
            var cls = c.ToString();
            foreach (var want in classes)
            {
                if (cls == want && IsWindowVisible(h))
                {
                    var t = new StringBuilder(512);
                    GetWindowText(h, t, t.Capacity);
                    found.Add(h.ToInt64() + "|" + cls + "|" + t.ToString());
                    break;
                }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static void Close(long hwnd)
    {
        // PostMessage, not SendMessage: non-blocking and never steals focus,
        // which is the closest match to the user clicking the X button.
        PostMessage(new IntPtr(hwnd), WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
    }
}
'@

if ($HostKind -eq 'Opus') {
    $classes = @('dopus.lister')
} else {
    $classes = @('CabinetWClass', 'ExploreWClass')
}

$targets = [Closer]::Find($classes)
if ($targets.Count -eq 0) {
    Write-Output "No $HostKind window found. Open one first (Explorer: Win+E)."
    exit 1
}

Write-Output "Will close these $HostKind window(s):"
foreach ($t in $targets) {
    $parts = $t -split '\|'
    $hex = ([int64]$parts[0]).ToString('X')
    Write-Output ("  0x$hex  [" + $parts[1] + "]  " + $parts[2])
}
Write-Output ''
Write-Output "Closing in $Seconds seconds."
Write-Output 'Now go to the host window, summon Prism (double-tap Ctrl), then touch nothing.'
Write-Output ''

$i = $Seconds
while ($i -gt 0) {
    Write-Output "  $i ..."
    Start-Sleep -Seconds 1
    $i = $i - 1
}

foreach ($t in $targets) {
    $hwnd = [int64](($t -split '\|')[0])
    [Closer]::Close($hwnd)
}

Write-Output ''
Write-Output 'Close message sent. Now look at Prism:'
Write-Output '  PASS -> scope label disappears, back to global search,'
Write-Output '          notice reads like "original window closed, back to global"'
Write-Output '  FAIL -> still shows the old scope label, or search stays limited'
Write-Output '          to the directory of the window that no longer exists'
