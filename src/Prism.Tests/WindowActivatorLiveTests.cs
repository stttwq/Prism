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

    /// <summary>
    /// 挂起的 UWP（`ApplicationFrameWindow`，如「设置」）能否真的被激活。
    ///
    /// G5 修 cloaked 过滤后，这类窗口才第一次进入候选列表——列出来却切不过去是没有意义的，
    /// 而 shell-cloaked 窗口正是 SetForegroundWindow 最可能拒绝的地方，所以单独探一次。
    ///
    /// 需要先手动打开「设置」；没开就跳过而不是失败（这是探针，不是 gate）。
    /// </summary>
    [Fact(Skip = "实机探针：会抢前台，且需先打开「设置」。手动去掉 Skip 后运行。")]
    public void ActivatesASuspendedUwpWindow()
    {
        var target = FindApplicationFrameWindow();
        if (target.Handle == IntPtr.Zero)
        {
            Console.WriteLine("SKIP: 没找到 ApplicationFrameWindow，请先打开「设置」再跑");
            return;
        }

        var before = GetForegroundWindow();
        var activator = new Win32WindowActivator();
        var result = activator.TryActivate(target);
        var foreground = GetForegroundWindow();

        Console.WriteLine($"target   = 0x{target.Handle:X} pid={target.Pid} {target.Title}");
        Console.WriteLine($"cloaked  = 0x{GetCloaked(target.Handle):X}");
        Console.WriteLine($"before   = 0x{before:X}");
        Console.WriteLine($"after    = 0x{foreground:X}");
        Console.WriteLine($"returned = {result}");
        Console.WriteLine(
            result
                ? "OUTCOME: suspended UWP really came to the foreground"
                : "OUTCOME: refused and reported false — listing it is then useless, "
                  + "the cloaked filter fix needs revisiting");

        // Same contract as the general probe: the return value must match reality.
        Assert.Equal(result, foreground == target.Handle);
    }

    /// <summary>找一个 ApplicationFrameWindow（UWP 的外壳窗口，Alt-Tab 显示的就是它）。</summary>
    private static WindowHandleInfo FindApplicationFrameWindow()
    {
        var found = new WindowHandleInfo(IntPtr.Zero, 0, "", false);
        var foreground = GetForegroundWindow();

        EnumWindows((hwnd, _) =>
        {
            if (found.Handle != IntPtr.Zero) return false;
            if (hwnd == foreground) return true;
            if (!IsWindowVisible(hwnd)) return true;

            var cls = new System.Text.StringBuilder(257);
            GetClassName(hwnd, cls, cls.Capacity);
            if (cls.ToString() != "ApplicationFrameWindow") return true;

            var length = GetWindowTextLength(hwnd);
            if (length <= 0) return true;
            var buffer = new System.Text.StringBuilder(length + 1);
            GetWindowText(hwnd, buffer, buffer.Capacity);
            if (string.IsNullOrWhiteSpace(buffer.ToString())) return true;

            GetWindowThreadProcessId(hwnd, out var pid);
            found = new WindowHandleInfo(hwnd, pid, buffer.ToString(), IsIconic(hwnd));
            return false;
        }, IntPtr.Zero);

        return found;
    }

    private static int GetCloaked(IntPtr hwnd)
    {
        // DWMWA_CLOAKED = 14
        return DwmGetWindowAttribute(hwnd, 14, out var value, sizeof(int)) == 0 ? value : -1;
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
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr hWnd, System.Text.StringBuilder buf, int max);
    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(IntPtr hWnd, int attr, out int value, int size);
}
