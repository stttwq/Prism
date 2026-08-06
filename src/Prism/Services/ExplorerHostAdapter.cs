using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 一个 Explorer shell 窗口的只读快照。路径已尽量解析为文件系统绝对路径；
/// 无法区分标签时由上层返回 <see cref="HostFailureReason.FolderUnavailable"/>，禁止猜错标签。
/// </summary>
/// <param name="IsActiveTab">
/// Windows 11 标签页 Explorer 里这一条是否属于活动标签。
/// <see langword="null"/> = 无法判定（旧系统、接口不可用或调用失败），
/// 调用方必须按「无法区分」保守处理，不能当成「不是活动标签」。
/// 单标签窗口只有一条记录，此值无关紧要。
/// </param>
public sealed record ExplorerShellWindow(
    IntPtr Hwnd,
    string? FolderPath,
    bool IsFileSystemFolder,
    bool? IsActiveTab = null);

/// <summary>
/// Explorer Shell COM 访问面。生产实现走 <c>Shell.Application</c> /
/// <c>IShellWindows</c>；单测注入 fake，避免真实 COM 与交互。
/// </summary>
public interface IExplorerShellAccess
{
    /// <summary>枚举当前 shell 窗口；必须可按 HWND 精确匹配，禁止「任意一个 Explorer」。</summary>
    IReadOnlyList<ExplorerShellWindow> EnumerateFolderWindows();

    /// <summary>
    /// 在<strong>同一</strong> shell 窗口内导航到文件夹，或导航到父目录并选中文件。
    /// 只能新开窗口时返回 false（上层映射为 ActionFailed）。
    /// </summary>
    bool TryNavigateOrReveal(IntPtr hwnd, string path, bool isDirectory, out HostFailureReason reason);
}

/// <summary>
/// Windows Explorer adapter：公开 Shell COM，无 DLL 注入。
/// 能力位：<see cref="HostCapability.ReadFolder"/> | NavigateFolder | RevealInHost。
/// </summary>
public sealed class ExplorerHostAdapter : IHostAdapter
{
    public const HostCapability SupportedCapabilities =
        HostCapability.ReadFolder | HostCapability.NavigateFolder | HostCapability.RevealInHost;

    private static readonly HashSet<string> ExplorerClasses = new(StringComparer.OrdinalIgnoreCase)
    {
        "CabinetWClass",
        "ExploreWClass",
    };

    private static readonly HashSet<string> DesktopClasses = new(StringComparer.OrdinalIgnoreCase)
    {
        "Progman",
        "WorkerW",
    };

    private readonly Func<bool> _isEnabled;
    private readonly INativeWindowQuery _windows;
    private readonly IHostProcessGuard _guard;
    private readonly IExplorerShellAccess _shell;

    public ExplorerHostAdapter(
        Func<bool>? isEnabled = null,
        INativeWindowQuery? windows = null,
        IHostProcessGuard? guard = null,
        IExplorerShellAccess? shell = null)
    {
        _isEnabled = isEnabled ?? (() => true);
        _windows = windows ?? new Win32NativeWindowQuery();
        _guard = guard ?? new Win32HostProcessGuard();
        _shell = shell ?? new ComExplorerShellAccess(_windows);
    }

    public HostKind Kind => HostKind.Explorer;

    public bool IsEnabled => _isEnabled();

    public HostDetection Detect(IntPtr foregroundWindow)
    {
        if (!IsEnabled)
            return HostDetection.NotHost(Kind, HostFailureReason.AdapterDisabled);
        if (foregroundWindow == IntPtr.Zero || !_windows.IsAlive(foregroundWindow))
            return HostDetection.NotHost(Kind, HostFailureReason.HostGone);

        try
        {
            if (_guard.IsElevatedAboveCurrent(foregroundWindow))
                return HostDetection.NotHost(Kind, HostFailureReason.HostElevated);

            var className = _windows.GetClassName(foregroundWindow);
            if (string.IsNullOrEmpty(className))
                return HostDetection.NotHost(Kind, HostFailureReason.NotThisHost);
            if (DesktopClasses.Contains(className))
                return HostDetection.NotHost(Kind, HostFailureReason.NotThisHost);
            // 必须是 Explorer 顶层窗口类。子控件 / 其他 explorer.exe 窗口（如托盘）一律不认，
            // 避免把「任意 explorer 进程」当成当前文件夹窗口。
            if (!ExplorerClasses.Contains(className))
                return HostDetection.NotHost(Kind, HostFailureReason.NotThisHost);

            return HostDetection.Host(Kind, SupportedCapabilities);
        }
        catch
        {
            return HostDetection.NotHost(Kind, HostFailureReason.DetectFailed);
        }
    }

    public HostFolder GetFolder(IntPtr hostWindow)
    {
        if (!IsEnabled)
            return HostFolder.Failure(HostFailureReason.AdapterDisabled);
        if (hostWindow == IntPtr.Zero || !_windows.IsAlive(hostWindow))
            return HostFolder.Failure(HostFailureReason.HostGone);

        try
        {
            if (_guard.IsElevatedAboveCurrent(hostWindow))
                return HostFolder.Failure(HostFailureReason.HostElevated);

            var detection = Detect(hostWindow);
            if (!detection.IsHost)
                return HostFolder.Failure(detection.Reason == HostFailureReason.None
                    ? HostFailureReason.NotThisHost
                    : detection.Reason);

            var matches = _shell.EnumerateFolderWindows()
                .Where(w => w.Hwnd == hostWindow)
                .ToArray();

            if (matches.Length == 0)
                return HostFolder.Failure(HostFailureReason.FolderUnavailable);

            // Windows 11 标签页 Explorer：一个顶层 HWND 下每个标签各一条记录。
            // 只有活动标签的视图窗口可见，据此消歧；判不出来才拒绝猜路径。
            if (matches.Length > 1)
            {
                var activeTabs = matches.Where(m => m.IsActiveTab == true).ToArray();
                if (activeTabs.Length != 1)
                    return HostFolder.Failure(HostFailureReason.FolderUnavailable);
                matches = activeTabs;
            }

            var match = matches[0];
            if (!match.IsFileSystemFolder || string.IsNullOrWhiteSpace(match.FolderPath))
                return HostFolder.Failure(HostFailureReason.FolderUnavailable);

            var path = NormalizeFolderPath(match.FolderPath);
            return path is null
                ? HostFolder.Failure(HostFailureReason.FolderUnavailable)
                : HostFolder.Success(path);
        }
        catch (UnauthorizedAccessException)
        {
            return HostFolder.Failure(HostFailureReason.AccessDenied);
        }
        catch
        {
            return HostFolder.Failure(HostFailureReason.FolderUnavailable);
        }
    }

    public HostNavigation NavigateOrFill(IntPtr hostWindow, HostNavigationRequest request)
    {
        if (!IsEnabled)
            return HostNavigation.Failure(HostFailureReason.AdapterDisabled);
        if (request.Intent == HostNavigationIntent.FillFileName)
            return HostNavigation.Failure(HostFailureReason.Unsupported);
        if (hostWindow == IntPtr.Zero || !_windows.IsAlive(hostWindow))
            return HostNavigation.Failure(HostFailureReason.HostGone);
        if (string.IsNullOrWhiteSpace(request.Path))
            return HostNavigation.Failure(HostFailureReason.ActionFailed);

        try
        {
            if (_guard.IsElevatedAboveCurrent(hostWindow))
                return HostNavigation.Failure(HostFailureReason.HostElevated);

            if (request.Intent is not (HostNavigationIntent.NavigateFolder or HostNavigationIntent.RevealInHost))
                return HostNavigation.Failure(HostFailureReason.Unsupported);

            var ok = _shell.TryNavigateOrReveal(
                hostWindow,
                request.Path.Trim(),
                request.IsDirectory || request.Intent == HostNavigationIntent.NavigateFolder,
                out var reason);
            return ok ? HostNavigation.Success : HostNavigation.Failure(reason);
        }
        catch
        {
            return HostNavigation.Failure(HostFailureReason.ActionFailed);
        }
    }

    internal static string? NormalizeFolderPath(string raw)
    {
        var value = raw.Trim();
        if (value.Length == 0) return null;

        // Shell 有时给 file:///C:/Users/...
        if (value.StartsWith("file:", StringComparison.OrdinalIgnoreCase))
        {
            if (Uri.TryCreate(value, UriKind.Absolute, out var uri) && uri.IsFile)
                value = uri.LocalPath;
            else
                return null;
        }

        value = value.Replace('/', '\\').TrimEnd();
        if (value.Length == 2 && value[1] == ':')
            value += "\\";

        if (value.Length < 2 || !char.IsAsciiLetter(value[0]) || value[1] != ':')
            return null;
        return value;
    }
}

/// <summary>
/// 生产环境 Shell COM 访问：通过 <c>Shell.Application</c> ProgID 晚绑定枚举窗口。
/// 不引入 SHDocVw 互操作程序集；失败全部收敛为结构化 reason。
/// </summary>
public sealed class ComExplorerShellAccess : IExplorerShellAccess
{
    private readonly INativeWindowQuery _windows;

    public ComExplorerShellAccess(INativeWindowQuery? windows = null)
    {
        _windows = windows ?? new Win32NativeWindowQuery();
    }

    public IReadOnlyList<ExplorerShellWindow> EnumerateFolderWindows()
    {
        var results = new List<ExplorerShellWindow>();
        if (!TryCreateShellApplication(out var shell) || shell is null)
            return results;

        object? windows = null;
        try
        {
            windows = shell.GetType().InvokeMember(
                "Windows",
                System.Reflection.BindingFlags.InvokeMethod,
                null,
                shell,
                null);
            if (windows is null) return results;

            var countObj = windows.GetType().InvokeMember(
                "Count",
                System.Reflection.BindingFlags.GetProperty,
                null,
                windows,
                null);
            var count = Convert.ToInt32(countObj, CultureInfo.InvariantCulture);
            for (var i = 0; i < count; i++)
            {
                object? item = null;
                try
                {
                    item = windows.GetType().InvokeMember(
                        "Item",
                        System.Reflection.BindingFlags.InvokeMethod,
                        null,
                        windows,
                        [i]);
                    if (item is null) continue;
                    if (!TryReadShellWindow(item, out var snapshot))
                        continue;
                    results.Add(snapshot);
                }
                catch
                {
                    // 单个窗口失败不影响其余。
                }
                finally
                {
                    if (item is not null && Marshal.IsComObject(item))
                        Marshal.FinalReleaseComObject(item);
                }
            }
        }
        catch
        {
            return results;
        }
        finally
        {
            if (windows is not null && Marshal.IsComObject(windows))
                Marshal.FinalReleaseComObject(windows);
            if (Marshal.IsComObject(shell))
                Marshal.FinalReleaseComObject(shell);
        }

        return results;
    }

    public bool TryNavigateOrReveal(IntPtr hwnd, string path, bool isDirectory, out HostFailureReason reason)
    {
        reason = HostFailureReason.ActionFailed;
        if (!TryCreateShellApplication(out var shell) || shell is null)
        {
            reason = HostFailureReason.ActionFailed;
            return false;
        }

        object? windows = null;
        object? target = null;
        try
        {
            windows = shell.GetType().InvokeMember(
                "Windows",
                System.Reflection.BindingFlags.InvokeMethod,
                null,
                shell,
                null);
            if (windows is null) return false;

            var count = Convert.ToInt32(
                windows.GetType().InvokeMember(
                    "Count",
                    System.Reflection.BindingFlags.GetProperty,
                    null,
                    windows,
                    null),
                CultureInfo.InvariantCulture);

            for (var i = 0; i < count; i++)
            {
                object? item = null;
                try
                {
                    item = windows.GetType().InvokeMember(
                        "Item",
                        System.Reflection.BindingFlags.InvokeMethod,
                        null,
                        windows,
                        [i]);
                    if (item is null) continue;
                    if (!TryGetHwnd(item, out var itemHwnd) || itemHwnd != hwnd)
                        continue;

                    // Windows 11 标签页：多个同 HWND 条目，只有活动标签可见。
                    var tabVisibility = ShellBrowserInterop.GetTabVisibility(item);
                    if (tabVisibility.Known && !tabVisibility.IsVisible)
                        continue;

                    target = item;
                    item = null; // ownership transferred
                    break;
                }
                finally
                {
                    if (item is not null && Marshal.IsComObject(item))
                        Marshal.FinalReleaseComObject(item);
                }
            }

            if (target is null)
            {
                reason = HostFailureReason.HostGone;
                return false;
            }

            if (isDirectory)
            {
                target.GetType().InvokeMember(
                    "Navigate",
                    System.Reflection.BindingFlags.InvokeMethod,
                    null,
                    target,
                    [path]);
                reason = HostFailureReason.None;
                return true;
            }

            // 文件：先 Navigate 到父目录，再 SelectItem；任一步失败 → ActionFailed，
            // 绝不退回 explorer /select 新开窗口当作成功。
            var parent = Path.GetDirectoryName(path);
            var name = Path.GetFileName(path);
            if (string.IsNullOrEmpty(parent) || string.IsNullOrEmpty(name))
            {
                reason = HostFailureReason.ActionFailed;
                return false;
            }

            target.GetType().InvokeMember(
                "Navigate",
                System.Reflection.BindingFlags.InvokeMethod,
                null,
                target,
                [parent]);

            // Navigate 是异步的：Explorer 先返回，之后才换掉 Document.Folder。
            // 立刻 SelectItem 会打在旧文件夹上，表现为「跳转了但没选中」。
            // 轮询直到 Folder 指向目标父目录，最多等 2 秒。
            if (!WaitForFolder(target, parent))
            {
                reason = HostFailureReason.ActionFailed;
                return false;
            }

            var document = target.GetType().InvokeMember(
                "Document",
                System.Reflection.BindingFlags.GetProperty,
                null,
                target,
                null);
            if (document is null)
            {
                reason = HostFailureReason.ActionFailed;
                return false;
            }

            try
            {
                var folder = document.GetType().InvokeMember(
                    "Folder",
                    System.Reflection.BindingFlags.GetProperty,
                    null,
                    document,
                    null);
                if (folder is null)
                {
                    reason = HostFailureReason.ActionFailed;
                    return false;
                }

                try
                {
                    var folderItem = folder.GetType().InvokeMember(
                        "ParseName",
                        System.Reflection.BindingFlags.InvokeMethod,
                        null,
                        folder,
                        [name]);
                    if (folderItem is null)
                    {
                        reason = HostFailureReason.ActionFailed;
                        return false;
                    }

                    try
                    {
                        // SVSI_SELECT | SVSI_ENSUREVISIBLE | SVSI_FOCUSED | SVSI_DESELECTOTHERS
                        const int selectFlags = 1 | 8 | 16 | 4;
                        document.GetType().InvokeMember(
                            "SelectItem",
                            System.Reflection.BindingFlags.InvokeMethod,
                            null,
                            document,
                            [folderItem, selectFlags]);
                        reason = HostFailureReason.None;
                        return true;
                    }
                    finally
                    {
                        if (Marshal.IsComObject(folderItem))
                            Marshal.FinalReleaseComObject(folderItem);
                    }
                }
                finally
                {
                    if (Marshal.IsComObject(folder))
                        Marshal.FinalReleaseComObject(folder);
                }
            }
            finally
            {
                if (Marshal.IsComObject(document))
                    Marshal.FinalReleaseComObject(document);
            }
        }
        catch
        {
            reason = HostFailureReason.ActionFailed;
            return false;
        }
        finally
        {
            if (target is not null && Marshal.IsComObject(target))
                Marshal.FinalReleaseComObject(target);
            if (windows is not null && Marshal.IsComObject(windows))
                Marshal.FinalReleaseComObject(windows);
            if (Marshal.IsComObject(shell))
                Marshal.FinalReleaseComObject(shell);
        }
    }

    /// <summary>
    /// <c>Navigate</c> 是异步的：Explorer 先返回，之后才把 <c>Document.Folder</c> 换成新目录。
    /// 立刻 <c>SelectItem</c> 会打在旧文件夹上，表现为「跳过去了但文件没选中」。
    /// 轮询当前文件夹直到等于目标父目录，最多等 2 秒；超时返回 false 交给上层降级。
    /// </summary>
    private static bool WaitForFolder(object target, string expectedParent)
    {
        var expected = NormalizeForCompare(expectedParent);
        var deadline = DateTime.UtcNow.AddSeconds(2);
        while (true)
        {
            var current = TryReadCurrentFolderPath(target);
            if (current is not null && NormalizeForCompare(current) == expected)
                return true;
            if (DateTime.UtcNow >= deadline)
                return false;
            System.Threading.Thread.Sleep(50);
        }
    }

    /// <summary>读取窗口当前文件夹路径；导航中或非文件系统位置时返回 null。</summary>
    private static string? TryReadCurrentFolderPath(object target)
    {
        object? document = null;
        object? folder = null;
        object? self = null;
        try
        {
            document = GetProperty(target, "Document");
            if (document is null) return null;
            folder = GetProperty(document, "Folder");
            if (folder is null) return null;
            self = GetProperty(folder, "Self");
            if (self is null) return null;
            var value = Convert.ToString(GetProperty(self, "Path"), CultureInfo.InvariantCulture);
            return string.IsNullOrWhiteSpace(value) ? null : value;
        }
        catch
        {
            // 导航过程中 shell 可能短暂拒绝调用，视为「还没到位」继续轮询。
            return null;
        }
        finally
        {
            ReleaseComObject(self);
            ReleaseComObject(folder);
            ReleaseComObject(document);
        }
    }

    private static object? GetProperty(object instance, string name) =>
        instance.GetType().InvokeMember(
            name,
            System.Reflection.BindingFlags.GetProperty,
            null,
            instance,
            null);

    private static void ReleaseComObject(object? value)
    {
        if (value is not null && Marshal.IsComObject(value))
            Marshal.FinalReleaseComObject(value);
    }

    private static string NormalizeForCompare(string path) =>
        path.Trim().Replace('/', '\\').TrimEnd('\\').ToLowerInvariant();

    private static bool TryCreateShellApplication(out object? shell)
    {
        shell = null;
        try
        {
            var type = Type.GetTypeFromProgID("Shell.Application");
            if (type is null) return false;
            shell = Activator.CreateInstance(type);
            return shell is not null;
        }
        catch
        {
            shell = null;
            return false;
        }
    }

    private static bool TryReadShellWindow(object item, out ExplorerShellWindow snapshot)
    {
        snapshot = new ExplorerShellWindow(IntPtr.Zero, null, false, null);
        if (!TryGetHwnd(item, out var hwnd) || hwnd == IntPtr.Zero)
            return false;

        var tabVisibility = ShellBrowserInterop.GetTabVisibility(item);

        string? path = null;
        var isFs = false;
        try
        {
            var document = item.GetType().InvokeMember(
                "Document",
                System.Reflection.BindingFlags.GetProperty,
                null,
                item,
                null);
            if (document is not null)
            {
                try
                {
                    // 浏览器窗口也有 Document，但没有 Folder.Self.Path。
                    var folder = document.GetType().InvokeMember(
                        "Folder",
                        System.Reflection.BindingFlags.GetProperty,
                        null,
                        document,
                        null);
                    if (folder is not null)
                    {
                        try
                        {
                            var self = folder.GetType().InvokeMember(
                                "Self",
                                System.Reflection.BindingFlags.GetProperty,
                                null,
                                folder,
                                null);
                            if (self is not null)
                            {
                                try
                                {
                                    path = Convert.ToString(
                                        self.GetType().InvokeMember(
                                            "Path",
                                            System.Reflection.BindingFlags.GetProperty,
                                            null,
                                            self,
                                            null),
                                        CultureInfo.InvariantCulture);
                                    isFs = !string.IsNullOrWhiteSpace(path)
                                        && !path.StartsWith("::", StringComparison.Ordinal)
                                        && !path.StartsWith("shell:", StringComparison.OrdinalIgnoreCase);
                                }
                                finally
                                {
                                    if (Marshal.IsComObject(self))
                                        Marshal.FinalReleaseComObject(self);
                                }
                            }
                        }
                        finally
                        {
                            if (Marshal.IsComObject(folder))
                                Marshal.FinalReleaseComObject(folder);
                        }
                    }
                }
                finally
                {
                    if (Marshal.IsComObject(document))
                        Marshal.FinalReleaseComObject(document);
                }
            }
        }
        catch
        {
            path = null;
            isFs = false;
        }

        if (string.IsNullOrWhiteSpace(path))
        {
            try
            {
                var location = Convert.ToString(
                    item.GetType().InvokeMember(
                        "LocationURL",
                        System.Reflection.BindingFlags.GetProperty,
                        null,
                        item,
                        null),
                    CultureInfo.InvariantCulture);
                path = ExplorerHostAdapter.NormalizeFolderPath(location ?? "");
                isFs = path is not null;
            }
            catch
            {
                // ignore
            }
        }
        else
        {
            path = ExplorerHostAdapter.NormalizeFolderPath(path);
            isFs = path is not null;
        }

        snapshot = new ExplorerShellWindow(
            hwnd,
            path,
            isFs,
            tabVisibility.Known ? tabVisibility.IsVisible : null);
        return true;
    }

    private static bool TryGetHwnd(object item, out IntPtr hwnd)
    {
        hwnd = IntPtr.Zero;
        try
        {
            var value = item.GetType().InvokeMember(
                "HWND",
                System.Reflection.BindingFlags.GetProperty,
                null,
                item,
                null);
            if (value is null) return false;
            hwnd = value switch
            {
                int i => new IntPtr(i),
                long l => new IntPtr(l),
                IntPtr p => p,
                _ => new IntPtr(Convert.ToInt64(value, CultureInfo.InvariantCulture)),
            };
            return hwnd != IntPtr.Zero;
        }
        catch
        {
            return false;
        }
    }
}
