# Check Prism top-level windows
$p = Get-Process Prism -ErrorAction SilentlyContinue
if (-not $p) { Write-Host 'NO PROCESS'; exit 1 }
Write-Host "PID=$($p.Id) WS=$([int]($p.WorkingSet64/1KB))KB"

$code = @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinEnum {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc lp, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public struct RECT { public int L,T,R,B; }
}
'@
Add-Type -TypeDefinition $code -ErrorAction SilentlyContinue

$pidTarget = [uint32]$p.Id
$script:found = 0
[WinEnum]::EnumWindows({
  param($h, $l)
  $wp = [uint32]0
  [void][WinEnum]::GetWindowThreadProcessId($h, [ref]$wp)
  if ($wp -eq $pidTarget) {
    $vis = [WinEnum]::IsWindowVisible($h)
    $sb = New-Object System.Text.StringBuilder 256
    [void][WinEnum]::GetWindowText($h, $sb, 256)
    $r = New-Object 'WinEnum+RECT'
    [void][WinEnum]::GetWindowRect($h, [ref]$r)
    $title = $sb.ToString()
    Write-Host ("hwnd=$h visible=$vis title=[$title] rect=$($r.L),$($r.T)-$($r.R),$($r.B)")
    if ($vis) { $script:found++ }
  }
  return $true
}, [IntPtr]::Zero) | Out-Null
Write-Host "visible_count=$($script:found)"
