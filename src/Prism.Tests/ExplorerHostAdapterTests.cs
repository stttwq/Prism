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
        // COM 无法区分标签时，同一 HWND 可能出现多条路径：禁止猜。
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
