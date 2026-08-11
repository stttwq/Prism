using System.Runtime.InteropServices;

namespace Prism.Services;

/// <summary>
/// 前台窗口激活（G5）。
///
/// 为什么在 WPF 而不是 broker：`SetForegroundWindow` 只对前台进程（或刚收到输入的进程）
/// 生效。按下 Enter 时前台进程是本进程，broker 是后台进程——它的前台调用会被 Windows
/// 静默降级成任务栏闪烁。这里复用 <c>SearchWindow.ForceActivate</c> 已验证的那条路径
/// （含 <c>AttachThreadInput</c> 兜底）。
///
/// 复核在这里再做一次：broker 的 resolve 与真正激活之间仍有一个窗口，目标可能已关闭或
/// 句柄已被复用。不使用注入、不模拟任意输入、不结束进程。
/// </summary>
public sealed class Win32WindowActivator : IWindowActivator
{
    private readonly INativeWindowQuery _query;

    public Win32WindowActivator(INativeWindowQuery? query = null)
    {
        _query = query ?? new Win32NativeWindowQuery();
    }

    public bool TryActivate(WindowHandleInfo window)
    {
        if (window.Handle == IntPtr.Zero) return false;

        // Re-verify: the handle must still be a live window belonging to the same process.
        if (!_query.IsAlive(window.Handle)) return false;
        if (window.Pid != 0 && _query.GetProcessId(window.Handle) != window.Pid) return false;
        if (!IsWindowVisible(window.Handle)) return false;

        try
        {
            // Restore before activating: a minimized window cannot take the foreground.
            if (IsIconic(window.Handle))
                ShowWindow(window.Handle, SW_RESTORE);

            BringWindowToTop(window.Handle);
            SetForegroundWindow(window.Handle);

            if (GetForegroundWindow() == window.Handle)
                return true;

            // Windows refuses cross-thread foreground changes unless the calling thread is
            // attached to the current foreground thread. This is the same fallback the
            // search window itself uses to summon reliably.
            var foreground = GetForegroundWindow();
            var foreThread = GetWindowThreadProcessId(foreground, IntPtr.Zero);
            var currentThread = GetCurrentThreadId();
            if (foreThread != 0 && foreThread != currentThread)
            {
                AttachThreadInput(currentThread, foreThread, true);
                try
                {
                    BringWindowToTop(window.Handle);
                    SetForegroundWindow(window.Handle);
                }
                finally
                {
                    AttachThreadInput(currentThread, foreThread, false);
                }
            }

            // Report what actually happened rather than assuming the call worked.
            return GetForegroundWindow() == window.Handle;
        }
        catch
        {
            return false;
        }
    }

    private const int SW_RESTORE = 9;

    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, IntPtr pid);
    [DllImport("user32.dll")] private static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
}
