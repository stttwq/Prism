using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// G4 步骤 4-5：Explorer Shell COM adapter 单测。全部用 fake 窗口/COM，
/// 不依赖真实 Explorer 交互，可在 CI 无头环境运行。
/// </summary>
public sealed class ExplorerHostAdapterTests
{
    private static readonly IntPtr ExplorerHwnd = new(0x1001);
    private static readonly IntPtr OtherExplorer = new(0x1002);
    private static readonly IntPtr Desktop = new(0x1003);
    private static readonly IntPtr Notepad = new(0x1004);

    [Fact]
    public void DetectMatchesCabinetClassAndRejectsDesktop()
    {
        var windows = new FakeWindows();
        var workerW = new IntPtr(0x1005);
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        windows.Set(Desktop, "Progman", "explorer", @"C:\Windows\explorer.exe");
        windows.Set(workerW, "WorkerW", "explorer", @"C:\Windows\explorer.exe");
        windows.Set(Notepad, "Notepad", "notepad", @"C:\Windows\notepad.exe");
        var adapter = Adapter(windows, out _, out _);

        Assert.True(adapter.Detect(ExplorerHwnd).IsHost);
        Assert.Equal(ExplorerHostAdapter.SupportedCapabilities, adapter.Detect(ExplorerHwnd).Capabilities);
        Assert.Equal(HostFailureReason.NotThisHost, adapter.Detect(Desktop).Reason);
        Assert.Equal(HostFailureReason.NotThisHost, adapter.Detect(workerW).Reason);
        Assert.Equal(HostFailureReason.NotThisHost, adapter.Detect(Notepad).Reason);
    }

    [Fact]
    public void GetFolderAndNavigateRespectDisabledSwitchWithoutNoise()
    {
        // IsEnabled=false 时 Detect/GetFolder/Navigate 一律 AdapterDisabled，
        // 不得返回路径或假装导航成功（产品门禁：关开关静默全局）。
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows = [new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\Docs", true)],
            NavigateSucceeds = true,
        };
        var adapter = new ExplorerHostAdapter(
            isEnabled: () => false,
            windows: windows,
            guard: new FakeHostProcessGuard(),
            shell: shell);

        Assert.Equal(HostFailureReason.AdapterDisabled, adapter.Detect(ExplorerHwnd).Reason);
        Assert.Equal(HostFailureReason.AdapterDisabled, adapter.GetFolder(ExplorerHwnd).Reason);
        Assert.Null(adapter.GetFolder(ExplorerHwnd).Path);

        var nav = adapter.NavigateOrFill(
            ExplorerHwnd,
            new HostNavigationRequest(@"C:\Temp", true, HostNavigationIntent.NavigateFolder));
        Assert.Equal(HostFailureReason.AdapterDisabled, nav.Reason);
        Assert.Equal(IntPtr.Zero, shell.LastHwnd); // 未调用 shell
    }

    [Fact]
    public void DetectRespectsDisabledSwitchAndElevation()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var enabled = false;
        var guard = new FakeHostProcessGuard();
        var adapter = new ExplorerHostAdapter(
            isEnabled: () => enabled,
            windows: windows,
            guard: guard,
            shell: new FakeExplorerShell());

        Assert.Equal(HostFailureReason.AdapterDisabled, adapter.Detect(ExplorerHwnd).Reason);

        enabled = true;
        guard.ElevatedWindows.Add(ExplorerHwnd);
        Assert.Equal(HostFailureReason.HostElevated, adapter.Detect(ExplorerHwnd).Reason);
        Assert.Equal(HostFailureReason.HostElevated, adapter.GetFolder(ExplorerHwnd).Reason);
    }

    [Fact]
    public void GetFolderMatchesCapturedHwndAmongMultipleExplorers()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        windows.Set(OtherExplorer, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows =
            [
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\Docs", true),
                new ExplorerShellWindow(OtherExplorer, @"D:\Work", true),
            ],
        };
        var adapter = Adapter(windows, shell, out _, out _);

        var folder = adapter.GetFolder(ExplorerHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"C:\Users\me\Docs", folder.Path);

        // 禁止拿「任意一个」Explorer 的路径。
        Assert.NotEqual(@"D:\Work", adapter.GetFolder(ExplorerHwnd).Path);
    }

    [Fact]
    public void GetFolderRefusesAmbiguousSameHwndEntries()
    {
        // COM 判不出活动标签时（IsActiveTab=null），同一 HWND 多条路径：禁止猜。
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "ExploreWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows =
            [
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\One", true),
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\Two", true),
            ],
        };
        var adapter = Adapter(windows, shell, out _, out _);

        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(ExplorerHwnd).Reason);
    }

    /// <summary>
    /// E3 回归：Windows 11 标签页 Explorer 下一个顶层 HWND 有多条记录，
    /// 必须用活动标签消歧，而不是整体拒绝识别（旧行为导致新建标签后两个标签都失效）。
    /// </summary>
    [Fact]
    public void GetFolderPicksActiveTabAmongTabsOfOneWindow()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows =
            [
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\Background", true, IsActiveTab: false),
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\Active 项目", true, IsActiveTab: true),
            ],
        };
        var adapter = Adapter(windows, shell, out _, out _);

        var folder = adapter.GetFolder(ExplorerHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"C:\Users\me\Active 项目", folder.Path);
    }

    /// <summary>
    /// E3：切换标签后另一个标签成为活动标签，识别必须跟着变，
    /// 不能因为「这个 HWND 曾经有多个标签」就永久失效。
    /// </summary>
    [Fact]
    public void GetFolderFollowsActiveTabAfterSwitch()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var first = new ExplorerShellWindow(ExplorerHwnd, @"C:\One", true, IsActiveTab: true);
        var second = new ExplorerShellWindow(ExplorerHwnd, @"C:\Two", true, IsActiveTab: false);
        var shell = new FakeExplorerShell { Windows = [first, second] };
        var adapter = Adapter(windows, shell, out _, out _);

        Assert.Equal(@"C:\One", adapter.GetFolder(ExplorerHwnd).Path);

        // 用户切到第二个标签。
        shell.Windows =
        [
            first with { IsActiveTab = false },
            second with { IsActiveTab = true },
        ];
        Assert.Equal(@"C:\Two", adapter.GetFolder(ExplorerHwnd).Path);
    }

    /// <summary>
    /// E3：单标签窗口只有一条记录，即使 IsActiveTab 判不出来也必须正常识别
    /// （回归用户描述的「单窗口正常」路径）。
    /// </summary>
    [Fact]
    public void GetFolderStillWorksForSingleTabWithUnknownTabState()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows = [new ExplorerShellWindow(ExplorerHwnd, @"C:\Users\me\Docs", true, IsActiveTab: null)],
        };
        var adapter = Adapter(windows, shell, out _, out _);

        var folder = adapter.GetFolder(ExplorerHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"C:\Users\me\Docs", folder.Path);
    }

    /// <summary>
    /// E3：多标签但没有任何一条被标为活动（接口整体不可用）时仍然拒绝猜测，
    /// 保持「宁可降级全局也不给错目录」的门禁。
    /// </summary>
    [Fact]
    public void GetFolderRefusesMultipleTabsWithNoActiveMarker()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows =
            [
                new ExplorerShellWindow(ExplorerHwnd, @"C:\One", true, IsActiveTab: false),
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Two", true, IsActiveTab: false),
            ],
        };
        var adapter = Adapter(windows, shell, out _, out _);

        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(ExplorerHwnd).Reason);

        // 两个都自称活动同样无法消歧。
        shell.Windows =
        [
            new ExplorerShellWindow(ExplorerHwnd, @"C:\One", true, IsActiveTab: true),
            new ExplorerShellWindow(ExplorerHwnd, @"C:\Two", true, IsActiveTab: true),
        ];
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(ExplorerHwnd).Reason);
    }

    /// <summary>
    /// E3：多标签窗口和另一个独立窗口共存时，仍然只认捕获的 HWND，
    /// 不会因为标签消歧而跨窗口取路径。
    /// </summary>
    [Fact]
    public void GetFolderKeepsHwndScopeWhenDisambiguatingTabs()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        windows.Set(OtherExplorer, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows =
            [
                new ExplorerShellWindow(OtherExplorer, @"D:\Other Active", true, IsActiveTab: true),
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Tab One", true, IsActiveTab: false),
                new ExplorerShellWindow(ExplorerHwnd, @"C:\Tab Two", true, IsActiveTab: true),
            ],
        };
        var adapter = Adapter(windows, shell, out _, out _);

        Assert.Equal(@"C:\Tab Two", adapter.GetFolder(ExplorerHwnd).Path);
        Assert.Equal(@"D:\Other Active", adapter.GetFolder(OtherExplorer).Path);
    }

    [Fact]
    public void GetFolderMapsMissingAndNonFsToFolderUnavailable()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            Windows = [new ExplorerShellWindow(ExplorerHwnd, "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", false)],
        };
        var adapter = Adapter(windows, shell, out _, out _);
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(ExplorerHwnd).Reason);

        shell.Windows = [];
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(ExplorerHwnd).Reason);
    }

    [Fact]
    public void NavigateWorksInSameWindowAndFillIsUnsupported()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell { NavigateSucceeds = true };
        var adapter = Adapter(windows, shell, out _, out _);

        var nav = adapter.NavigateOrFill(
            ExplorerHwnd,
            new HostNavigationRequest(@"C:\Users\me\Docs", true, HostNavigationIntent.NavigateFolder));
        Assert.True(nav.Succeeded);
        Assert.Equal(ExplorerHwnd, shell.LastHwnd);
        Assert.Equal(@"C:\Users\me\Docs", shell.LastPath);

        var reveal = adapter.NavigateOrFill(
            ExplorerHwnd,
            new HostNavigationRequest(@"C:\Users\me\Docs\a.txt", false, HostNavigationIntent.RevealInHost));
        Assert.True(reveal.Succeeded);
        Assert.False(shell.LastIsDirectory);

        var fill = adapter.NavigateOrFill(
            ExplorerHwnd,
            new HostNavigationRequest("a.txt", false, HostNavigationIntent.FillFileName));
        Assert.False(fill.Succeeded);
        Assert.Equal(HostFailureReason.Unsupported, fill.Reason);
    }

    [Fact]
    public void NavigateDoesNotTreatOpenNewWindowAsSuccess()
    {
        var windows = new FakeWindows();
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe");
        var shell = new FakeExplorerShell
        {
            NavigateSucceeds = false,
            NavigateReason = HostFailureReason.ActionFailed,
        };
        var adapter = Adapter(windows, shell, out _, out _);

        var nav = adapter.NavigateOrFill(
            ExplorerHwnd,
            new HostNavigationRequest(@"C:\Temp", true, HostNavigationIntent.NavigateFolder));
        Assert.False(nav.Succeeded);
        Assert.Equal(HostFailureReason.ActionFailed, nav.Reason);
    }

    [Theory]
    [InlineData(@"C:\Users\me", @"C:\Users\me")]
    [InlineData(@"file:///C:/Users/me", @"C:\Users\me")]
    [InlineData(@"C:", @"C:\")]
    [InlineData(@"::dead", null)]
    public void NormalizeFolderPathAcceptsFilesystemOnly(string raw, string? expected)
    {
        Assert.Equal(expected, ExplorerHostAdapter.NormalizeFolderPath(raw));
    }

    private static ExplorerHostAdapter Adapter(
        FakeWindows windows,
        out FakeExplorerShell shell,
        out FakeHostProcessGuard guard) =>
        Adapter(windows, new FakeExplorerShell(), out shell, out guard);

    private static ExplorerHostAdapter Adapter(
        FakeWindows windows,
        FakeExplorerShell shell,
        out FakeExplorerShell shellOut,
        out FakeHostProcessGuard guard)
    {
        shellOut = shell;
        guard = new FakeHostProcessGuard();
        return new ExplorerHostAdapter(
            isEnabled: () => true,
            windows: windows,
            guard: guard,
            shell: shell);
    }

    private sealed class FakeWindows : INativeWindowQuery
    {
        private readonly Dictionary<IntPtr, (string Class, string Process, string Path)> _map = [];

        public void Set(IntPtr hwnd, string className, string process, string path) =>
            _map[hwnd] = (className, process, path);

        public bool IsAlive(IntPtr window) => _map.ContainsKey(window);
        public string? GetClassName(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Class : null;
        public string? GetWindowTitle(IntPtr window) => "";
        public uint GetProcessId(IntPtr window) => window == IntPtr.Zero ? 0u : 42u;
        public string? GetProcessName(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Process : null;
        public string? GetProcessPath(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Path : null;
    }

    private sealed class FakeExplorerShell : IExplorerShellAccess
    {
        public List<ExplorerShellWindow> Windows { get; set; } = [];
        public bool NavigateSucceeds { get; set; } = true;
        public HostFailureReason NavigateReason { get; set; } = HostFailureReason.ActionFailed;
        public IntPtr LastHwnd { get; private set; }
        public string? LastPath { get; private set; }
        public bool LastIsDirectory { get; private set; }

        public IReadOnlyList<ExplorerShellWindow> EnumerateFolderWindows() => Windows;

        public bool TryNavigateOrReveal(IntPtr hwnd, string path, bool isDirectory, out HostFailureReason reason)
        {
            LastHwnd = hwnd;
            LastPath = path;
            LastIsDirectory = isDirectory;
            reason = NavigateSucceeds ? HostFailureReason.None : NavigateReason;
            return NavigateSucceeds;
        }
    }
}
