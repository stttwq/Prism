using System.Runtime.InteropServices;

namespace Prism.Services;

/// <summary>
/// 提权宿主拒绝：目标进程完整性级别高于当前进程时，任何 adapter 都不得尝试控制
/// （prd：无提权宿主控制）。可被两个真实 adapter 与单测共用。
/// </summary>
public interface IHostProcessGuard
{
    /// <summary>
    /// 目标窗口所属进程的完整性级别是否严格高于当前进程。
    /// 打不开目标进程令牌（常见于更高完整性）时按「更高」处理，宁可拒联也不盲控。
    /// </summary>
    bool IsElevatedAboveCurrent(IntPtr window);
}

/// <summary>基于 Win32 令牌完整性级别的实现；只读查询，不调整权限。</summary>
public sealed class Win32HostProcessGuard : IHostProcessGuard
{
    private const uint ProcessQueryLimitedInformation = 0x1000;
    private const uint TokenQuery = 0x0008;
    private const int TokenIntegrityLevel = 25;
    private static readonly IntPtr CurrentProcess = GetCurrentProcess();

    public bool IsElevatedAboveCurrent(IntPtr window)
    {
        if (window == IntPtr.Zero) return false;

        GetWindowThreadProcessId(window, out var targetPid);
        if (targetPid == 0) return false;

        var currentPid = GetCurrentProcessId();
        if (targetPid == currentPid) return false;

        if (!TryReadIntegrityLevel(CurrentProcess, alreadyHandle: true, out var currentLevel))
            return false;

        var process = OpenProcessHandle(targetPid);
        if (process == IntPtr.Zero)
        {
            // 中等完整性打不开高完整性进程是常态 → 视为提权宿主。
            var error = Marshal.GetLastWin32Error();
            return error is 5 or 87 or 299; // ACCESS_DENIED / INVALID_PARAMETER / partial copy
        }

        try
        {
            if (!TryReadIntegrityLevel(process, alreadyHandle: true, out var targetLevel))
                return true; // 读不到就拒联

            return targetLevel > currentLevel;
        }
        finally
        {
            CloseHandle(process);
        }
    }

    private static bool TryReadIntegrityLevel(IntPtr processOrCurrent, bool alreadyHandle, out int level)
    {
        level = 0;
        var process = processOrCurrent;
        var needClose = false;
        try
        {
            if (!alreadyHandle)
            {
                process = OpenProcess(ProcessQueryLimitedInformation, false, (uint)processOrCurrent.ToInt64());
                if (process == IntPtr.Zero) return false;
                needClose = true;
            }

            if (!OpenProcessToken(process, TokenQuery, out var token) || token == IntPtr.Zero)
                return false;

            try
            {
                GetTokenInformation(token, TokenIntegrityLevel, IntPtr.Zero, 0, out var needed);
                if (needed == 0 || needed > 1024) return false;

                var buffer = Marshal.AllocHGlobal(needed);
                try
                {
                    if (!GetTokenInformation(token, TokenIntegrityLevel, buffer, needed, out _))
                        return false;

                    var label = Marshal.PtrToStructure<TokenMandatoryLabel>(buffer);
                    if (label.Label.Sid == IntPtr.Zero) return false;

                    var subAuthCount = GetSidSubAuthorityCount(label.Label.Sid);
                    if (subAuthCount == IntPtr.Zero) return false;
                    var count = Marshal.ReadByte(subAuthCount);
                    if (count == 0) return false;

                    var subAuth = GetSidSubAuthority(label.Label.Sid, count - 1);
                    if (subAuth == IntPtr.Zero) return false;
                    level = Marshal.ReadInt32(subAuth);
                    return true;
                }
                finally
                {
                    Marshal.FreeHGlobal(buffer);
                }
            }
            finally
            {
                CloseHandle(token);
            }
        }
        finally
        {
            if (needClose && process != IntPtr.Zero)
                CloseHandle(process);
        }
    }

    private static IntPtr OpenProcessHandle(uint pid)
    {
        var handle = OpenProcess(ProcessQueryLimitedInformation, false, pid);
        return handle;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct SidAndAttributes
    {
        public IntPtr Sid;
        public uint Attributes;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct TokenMandatoryLabel
    {
        public SidAndAttributes Label;
    }

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentProcessId();

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool GetTokenInformation(
        IntPtr token,
        int tokenInfoClass,
        IntPtr tokenInfo,
        int tokenInfoLength,
        out int returnLength);

    [DllImport("advapi32.dll")]
    private static extern IntPtr GetSidSubAuthority(IntPtr sid, int subAuthority);

    [DllImport("advapi32.dll")]
    private static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);
}

/// <summary>测试用：按 HWND 集合判定「提权」。</summary>
public sealed class FakeHostProcessGuard : IHostProcessGuard
{
    public HashSet<IntPtr> ElevatedWindows { get; } = [];

    public bool IsElevatedAboveCurrent(IntPtr window) => ElevatedWindows.Contains(window);
}
