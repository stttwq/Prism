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
