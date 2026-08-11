using System.Runtime.InteropServices;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// Win32WindowActivator 的实机探针。
///
/// 其余所有窗口测试都注入 FakeWindowActivator，所以真正的 SetForegroundWindow /
/// AttachThreadInput / SW_RESTORE 路径在自动化测试里从未执行过一次。这个文件是唯一会
/// 真的去抢前台的地方。
///
/// 全部 [Fact(Skip)]：会短暂抢走当前前台窗口，不适合进常规 gate。手动运行：
///
/// <code>
/// dotnet test src/Prism.Tests/Prism.Tests.csproj -c Release \
///   --filter "FullyQualifiedName~WindowActivatorLiveTests" -- xunit.methodDisplay=method
/// </code>
///
/// 需要临时去掉 Skip 才会执行（xunit 不支持 --ignored 那种运行时开关）。
/// </summary>
public sealed class WindowActivatorLiveTests
{
    [Fact(Skip = "实机探针：会抢前台。手动去掉 Skip 后运行。")]
    public void ActivatesARealWindowAndReportsTheTruth()
    {
        // Must not already be foreground, or TryActivate short-circuits and the real
        // cross-thread foreground path never runs.
        var target = FindSwitchableWindow(excludeForeground: true);
        Assert.NotEqual(IntPtr.Zero, target.Handle);

        var before = GetForegroundWindow();
        Assert.NotEqual(before, target.Handle);
        var activator = new Win32WindowActivator();
        var result = activator.TryActivate(target);
        var foreground = GetForegroundWindow();

        // Distinguish a real switch from a correctly-reported refusal. Without this the
        // agreement assertion below passes either way and proves nothing about activation.
        Console.WriteLine($"target   = 0x{target.Handle:X} pid={target.Pid} {target.Title}");
        Console.WriteLine($"before   = 0x{before:X}");
        Console.WriteLine($"after    = 0x{foreground:X}");
        Console.WriteLine($"returned = {result}");
        Console.WriteLine(
            result
                ? "OUTCOME: activation actually took the foreground"
                : "OUTCOME: activation was refused and reported false (contract held, "
                  + "but the foreground path did NOT succeed here)");

        // The contract is that the return value reflects reality, not that activation
        // always succeeds — Windows may legitimately refuse a non-foreground caller.
        Assert.Equal(result, foreground == target.Handle);
    }

    [Fact(Skip = "实机探针：会抢前台。手动去掉 Skip 后运行。")]
    public void DeadHandleIsRejectedWithoutTouchingTheForeground()
    {
        var before = GetForegroundWindow();
        var activator = new Win32WindowActivator();

        // A handle that is not a window at all.
        var result = activator.TryActivate(
            new WindowHandleInfo(new IntPtr(0x7FFFFFF0), 0, "gone", IsMinimized: false));

        Assert.False(result);
        Assert.Equal(before, GetForegroundWindow());
    }

    [Fact(Skip = "实机探针：会抢前台。手动去掉 Skip 后运行。")]
    public void PidMismatchIsRejectedEvenWhenTheHandleIsAlive()
    {
        var target = FindSwitchableWindow();
        var before = GetForegroundWindow();
        var activator = new Win32WindowActivator();

        // Live handle, wrong pid — the handle-reuse case. Must refuse.
        var result = activator.TryActivate(target with { Pid = 999_999 });

        Assert.False(result);
        Assert.Equal(before, GetForegroundWindow());
    }

    /// <summary>
    /// 找一个可切换的可见顶层窗口，排除本测试进程自身。
    ///
    /// <paramref name="excludeForeground"/> 为真时跳过当前前台窗口——否则探针可能选中
    /// 已经在前台的那个，TryActivate 走早返回分支，真正的跨线程前台路径根本没执行，
    /// 测试却是绿的。
    /// </summary>
    private static WindowHandleInfo FindSwitchableWindow(bool excludeForeground = false)
    {
        var found = new WindowHandleInfo(IntPtr.Zero, 0, "", false);
        var self = (uint)Environment.ProcessId;
        var foreground = excludeForeground ? GetForegroundWindow() : IntPtr.Zero;

        EnumWindows((hwnd, _) =>
        {
            if (found.Handle != IntPtr.Zero) return false;
            if (excludeForeground && hwnd == foreground) return true;
            if (!IsWindowVisible(hwnd)) return true;
            if (GetWindow(hwnd, GW_OWNER) != IntPtr.Zero) return true;
            if ((GetWindowLong(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW) != 0) return true;

            var length = GetWindowTextLength(hwnd);
            if (length <= 0) return true;
            var buffer = new System.Text.StringBuilder(length + 1);
            GetWindowText(hwnd, buffer, buffer.Capacity);
            var title = buffer.ToString();
            if (string.IsNullOrWhiteSpace(title)) return true;

            GetWindowThreadProcessId(hwnd, out var pid);
            if (pid == self) return true;

            found = new WindowHandleInfo(hwnd, pid, title, IsIconic(hwnd));
            return false;
        }, IntPtr.Zero);

        return found;
    }

    private const int GWL_EXSTYLE = -20;
    private const int WS_EX_TOOLWINDOW = 0x00000080;
    private const uint GW_OWNER = 4;

    private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc proc, IntPtr lParam);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);
    [DllImport("user32.dll")] private static extern int GetWindowLong(IntPtr hWnd, int index);
    [DllImport("user32.dll")] private static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder buf, int max);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
}
