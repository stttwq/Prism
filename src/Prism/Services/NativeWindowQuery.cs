using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace Prism.Services;

/// <summary>只读窗口信息，供宿主 adapter 识别类名/进程，不执行任何控制。</summary>
public interface INativeWindowQuery
{
    bool IsAlive(IntPtr window);
    string? GetClassName(IntPtr window);
    string? GetWindowTitle(IntPtr window);
    uint GetProcessId(IntPtr window);
    string? GetProcessName(IntPtr window);
    string? GetProcessPath(IntPtr window);
}

public sealed class Win32NativeWindowQuery : INativeWindowQuery
{
    private const uint ProcessQueryLimitedInformation = 0x1000;
    private const int MaxClassName = 256;
    private const int MaxTitle = 512;

    public bool IsAlive(IntPtr window) =>
        window != IntPtr.Zero && IsWindow(window);

    public string? GetClassName(IntPtr window)
    {
        if (window == IntPtr.Zero) return null;
        var buffer = new StringBuilder(MaxClassName);
        var written = GetClassName(window, buffer, buffer.Capacity);
        return written > 0 ? buffer.ToString() : null;
    }

    public string? GetWindowTitle(IntPtr window)
    {
        if (window == IntPtr.Zero) return null;
        var length = GetWindowTextLength(window);
        if (length <= 0) return "";
        var buffer = new StringBuilder(Math.Min(length + 1, MaxTitle));
        _ = GetWindowText(window, buffer, buffer.Capacity);
        return buffer.ToString();
    }

    public uint GetProcessId(IntPtr window)
    {
        if (window == IntPtr.Zero) return 0;
        _ = GetWindowThreadProcessId(window, out var pid);
        return pid;
    }

    public string? GetProcessName(IntPtr window)
    {
        var path = GetProcessPath(window);
        if (string.IsNullOrEmpty(path)) return null;
        return Path.GetFileNameWithoutExtension(path);
    }

    public string? GetProcessPath(IntPtr window)
    {
        var pid = GetProcessId(window);
        if (pid == 0) return null;

        var handle = OpenProcess(ProcessQueryLimitedInformation, false, pid);
        if (handle == IntPtr.Zero) return null;
        try
        {
            var buffer = new StringBuilder(1024);
            var size = (uint)buffer.Capacity;
            if (!QueryFullProcessImageName(handle, 0, buffer, ref size) || size == 0)
                return null;
            return buffer.ToString(0, (int)size);
        }
        finally
        {
            CloseHandle(handle);
        }
    }

    [DllImport("user32.dll")]
    private static extern bool IsWindow(IntPtr hWnd);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int GetClassName(IntPtr hWnd, StringBuilder lpClassName, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextLength(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool QueryFullProcessImageName(
        IntPtr process,
        uint flags,
        StringBuilder exeName,
        ref uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);
}
