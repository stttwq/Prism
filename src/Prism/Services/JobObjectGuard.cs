using System.Runtime.InteropServices;

namespace Prism.Services;

/// <summary>
/// 把子进程纳入 Job Object，用于跨 Prism 会话的 broker 所有权跟踪（TryAdopt 收编孤儿）。
/// **不再设 KILL_ON_JOB_CLOSE**（Bug 3 修复）：broker 经 ShellExecuteExW 打开的用户应用会
/// 继承 Job 成员身份，KILL 标志会让 Prism 退出时连用户应用一起杀。现在 Prism 正常退出由
/// DisposeCore 显式 Kill broker 本身；Prism 崩溃后 broker 作为孤儿留存，下次启动由
/// AdoptExistingServer 收编复用或杀旧拉新。
/// </summary>
internal sealed class JobObjectGuard : IDisposable
{
    private IntPtr _jobHandle;
    private bool _disposed;

    /// <summary>
    /// 创建 Job Object（不设任何 limit）。Job 仅用于 Assign/TryAdopt 的进程归属跟踪。
    /// </summary>
    public JobObjectGuard()
    {
        _jobHandle = CreateJobObject(IntPtr.Zero, null);
        if (_jobHandle == IntPtr.Zero)
            return;
        // Bug 3: 不再设置 KILL_ON_JOB_CLOSE——它会让 broker 启动的用户应用随 Prism 退出被杀。
        // 无任何 limit 需要设置，Job 句柄仅用于 AssignProcessToJobObject 归属跟踪。
    }

    /// <summary>把进程纳入 Job Object。失败时静默忽略——Job 是保险，不是必要条件。</summary>
    public bool Assign(IntPtr processHandle)
    {
        if (_disposed || _jobHandle == IntPtr.Zero)
            return false;
        return AssignProcessToJobObject(_jobHandle, processHandle);
    }

    /// <summary>
    /// AUDIT-2026-08-18 R-A8: 把一个**外部已存在**的进程（上次 Prism 留下的孤儿 broker）
    /// 收编进本 Job。OpenProcess 只申请收编所需的最小权限
    /// （AssignProcessToJobObject 要求 PROCESS_SET_QUOTA | PROCESS_TERMINATE），
    /// 不碰句柄所有权，用完即还。失败返回 false，由调用方决定杀旧拉新还是维持复用。
    /// </summary>
    public bool TryAdopt(int pid)
    {
        if (_disposed || _jobHandle == IntPtr.Zero || pid <= 0)
            return false;
        var hProcess = OpenProcess(ProcessSetQuota | ProcessTerminate, false, (uint)pid);
        if (hProcess == IntPtr.Zero)
            return false;
        try
        {
            return AssignProcessToJobObject(_jobHandle, hProcess);
        }
        finally
        {
            CloseHandle(hProcess);
        }
    }

    public void Dispose()
    {
        if (_disposed)
            return;
        _disposed = true;
        // 关闭 Job 句柄。不再设 KILL_ON_JOB_CLOSE（Bug 3），故不杀任何关联进程。
        if (_jobHandle != IntPtr.Zero)
        {
            CloseHandle(_jobHandle);
            _jobHandle = IntPtr.Zero;
        }
    }

    // --- P/Invoke ---

    private const uint ProcessSetQuota = 0x0100;
    private const uint ProcessTerminate = 0x0001;

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateJobObject(IntPtr lpJobAttributes, string? lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(IntPtr hJob, IntPtr hProcess);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);
}
