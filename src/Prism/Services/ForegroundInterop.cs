using System.Runtime.InteropServices;

namespace Prism.Services;

/// <summary>
/// Shared user32/kernel32 P/Invoke declarations for foreground-window activation.
/// Previously duplicated between <see cref="Win32WindowActivator"/> and
/// <see cref="Prism.Windows.SearchWindow"/>.
/// </summary>
internal static class ForegroundInterop
{
    public const int SW_SHOW = 5;
    public const int SW_RESTORE = 9;

    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, IntPtr pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();

    // ── 多显示器定位（B4）──────────────────────────────────────────────
    private const uint MONITOR_DEFAULTTONEAREST = 2;

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint dwFlags);

    [StructLayout(LayoutKind.Sequential)]
    private struct MONITORINFO
    {
        public int cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO lpmi);

    [DllImport("shcore.dll")]
    private static extern int GetDpiForMonitor(IntPtr hMonitor, int dpiType, out uint dpiX, out uint dpiY);

    private const int MDT_EFFECTIVE_DPI = 0;

    /// <summary>
    /// 指定窗口所在显示器的工作区（DIP）。混合 DPI 多显示器下把物理像素
    /// 按该显示器有效 DPI 换算回来，避免跨屏定位偏移；
    /// 任一步失败回退主屏工作区（行为与旧版一致）。
    /// </summary>
    public static System.Windows.Rect GetWorkAreaForWindow(IntPtr hwnd)
    {
        try
        {
            var monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if (monitor == IntPtr.Zero)
                return SystemParametersWorkArea();

            var info = new MONITORINFO { cbSize = System.Runtime.InteropServices.Marshal.SizeOf<MONITORINFO>() };
            if (!GetMonitorInfo(monitor, ref info))
                return SystemParametersWorkArea();

            var scale = 1.0;
            if (GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, out var dpiX, out _) == 0 && dpiX > 0)
                scale = dpiX / 96.0;

            return new System.Windows.Rect(
                info.rcWork.Left / scale,
                info.rcWork.Top / scale,
                (info.rcWork.Right - info.rcWork.Left) / scale,
                (info.rcWork.Bottom - info.rcWork.Top) / scale);
        }
        catch
        {
            return SystemParametersWorkArea();
        }
    }

    private static System.Windows.Rect SystemParametersWorkArea() =>
        new(
            System.Windows.SystemParameters.WorkArea.Left,
            System.Windows.SystemParameters.WorkArea.Top,
            System.Windows.SystemParameters.WorkArea.Width,
            System.Windows.SystemParameters.WorkArea.Height);

    /// <summary>
    /// Attempts to bring <paramref name="hwnd"/> to the foreground, using the
    /// <c>AttachThreadInput</c> fallback when the current foreground belongs to a
    /// different thread. Returns <c>true</c> if <paramref name="hwnd"/> is the
    /// foreground window after the attempt.
    /// </summary>
    public static bool TryForceForeground(IntPtr hwnd)
    {
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);

        if (GetForegroundWindow() == hwnd)
            return true;

        // Windows refuses cross-thread foreground changes unless the calling thread is
        // attached to the current foreground thread.
        var foreground = GetForegroundWindow();
        var foreThread = GetWindowThreadProcessId(foreground, IntPtr.Zero);
        var currentThread = GetCurrentThreadId();
        if (foreThread != 0 && foreThread != currentThread)
        {
            AttachThreadInput(currentThread, foreThread, true);
            try
            {
                BringWindowToTop(hwnd);
                SetForegroundWindow(hwnd);
            }
            finally
            {
                AttachThreadInput(currentThread, foreThread, false);
            }
        }

        return GetForegroundWindow() == hwnd;
    }
}
