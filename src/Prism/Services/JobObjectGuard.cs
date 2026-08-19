using System.Runtime.InteropServices;

namespace Prism.Services;

/// <summary>
/// 把子进程纳入 Job Object，父进程退出时 OS 内核自动回收子进程，
/// 防止 Prism 崩溃后 prism-core.exe 变成孤儿继续占管道。
/// </summary>
internal sealed class JobObjectGuard : IDisposable
{
    private IntPtr _jobHandle;
    private bool _disposed;

    /// <summary>
    /// 创建 Job Object 并设置 KILL_ON_JOB_CLOSE 限制。
    /// 只要本对象不被 Dispose（即父进程不退出），子进程照常运行；
    /// 父进程退出 → Job 句柄关闭 → OS 杀掉所有关联子进程。
    /// </summary>
    public JobObjectGuard()
    {
        _jobHandle = CreateJobObject(IntPtr.Zero, null);
        if (_jobHandle == IntPtr.Zero)
            return;

        var info = new JOBOBJECT_BASIC_LIMIT_INFORMATION
        {
            LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        var extended = new JOBOBJECT_EXTENDED_LIMIT_INFORMATION
        {
            BasicLimitInformation = info,
        };

        var length = Marshal.SizeOf<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>();
        var ptr = Marshal.AllocHGlobal(length);
        try
        {
            Marshal.StructureToPtr(extended, ptr, false);
            // KILL_ON_JOB_CLOSE 没设置成功 = 孤儿进程保护形同虚设（Prism 崩溃后
            // broker 会继续占管道）。行为保持"尽力而为"，但失败必须留下痕迹。
            if (!SetInformationJobObject(
                    _jobHandle,
                    JobObjectExtendedLimitInformation,
                    ptr,
                    (uint)length))
            {
                var error = Marshal.GetLastWin32Error();
                System.Diagnostics.Debug.WriteLine(
                    $"[Prism] JobObject 限制设置失败（Win32 {error}），孤儿进程保护未生效");
            }
        }
        finally
        {
            Marshal.FreeHGlobal(ptr);
        }
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
        // 关闭 Job 句柄 → OS 内核杀掉所有关联子进程。
        if (_jobHandle != IntPtr.Zero)
        {
            CloseHandle(_jobHandle);
            _jobHandle = IntPtr.Zero;
        }
    }

    // --- P/Invoke ---

    private const int JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000;
    private const int JobObjectExtendedLimitInformation = 9;

    private const uint ProcessSetQuota = 0x0100;
    private const uint ProcessTerminate = 0x0001;

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_BASIC_LIMIT_INFORMATION
    {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize;
        public UIntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct IO_COUNTERS
    {
        public ulong ReadOperationCount;
        public ulong WriteOperationCount;
        public ulong OtherOperationCount;
        public ulong ReadTransferCount;
        public ulong WriteTransferCount;
        public ulong OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    {
        public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
        public IO_COUNTERS IoInfo;
        public UIntPtr ProcessMemoryLimit;
        public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed;
        public UIntPtr PeakJobMemoryUsed;
    }

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateJobObject(IntPtr lpJobAttributes, string? lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(
        IntPtr hJob, int infoClass, IntPtr lpInfo, uint cbInfoLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(IntPtr hJob, IntPtr hProcess);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);
}
