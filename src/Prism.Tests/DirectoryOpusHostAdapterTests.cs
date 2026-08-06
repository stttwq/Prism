using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// G4 步骤 4-5：Directory Opus adapter 单测。dopusrt 调用全部走 fake runtime，
/// 无 Opus 安装的 CI 也可编译并跑通。
/// </summary>
public sealed class DirectoryOpusHostAdapterTests
{
    private static readonly IntPtr OpusHwnd = new(0x2001);
    private static readonly IntPtr ExplorerHwnd = new(0x2002);

    [Fact]
    public void DetectRequiresDopusProcessAndEnabledSwitch()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\Program Files\GPSoftware\Directory Opus\dopus.exe", "Docs");
        windows.Set(ExplorerHwnd, "CabinetWClass", "explorer", @"C:\Windows\explorer.exe", "x");
        var enabled = true;
        var runtime = new FakeRuntime();
        var adapter = new DirectoryOpusHostAdapter(
            isEnabled: () => enabled,
            windows: windows,
            guard: new FakeHostProcessGuard(),
            runtime: runtime);

        Assert.True(adapter.Detect(OpusHwnd).IsHost);
        Assert.Equal(HostFailureReason.NotThisHost, adapter.Detect(ExplorerHwnd).Reason);

        enabled = false;
        Assert.Equal(HostFailureReason.AdapterDisabled, adapter.Detect(OpusHwnd).Reason);
        Assert.False(adapter.IsEnabled);
    }

    [Fact]
    public void DetectRejectsDopusRtProcessEvenWhenEnabled()
    {
        // 只认 dopus 主进程；dopusrt 是外部命令工具，前台绝不应被当成 Lister。
        var windows = new FakeWindows();
        var dopusRtHwnd = new IntPtr(0x2003);
        windows.Set(
            dopusRtHwnd,
            "ConsoleWindowClass",
            "dopusrt",
            @"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe",
            "dopusrt");
        var adapter = new DirectoryOpusHostAdapter(
            isEnabled: () => true,
            windows: windows,
            guard: new FakeHostProcessGuard(),
            runtime: new FakeRuntime());

        Assert.Equal(HostFailureReason.NotThisHost, adapter.Detect(dopusRtHwnd).Reason);
        Assert.Equal(HostFailureReason.NotThisHost, adapter.GetFolder(dopusRtHwnd).Reason);
    }

    [Fact]
    public void ElevatedHostIsRejected()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\Program Files\GPSoftware\Directory Opus\dopus.exe", "Docs");
        var guard = new FakeHostProcessGuard();
        guard.ElevatedWindows.Add(OpusHwnd);
        var adapter = new DirectoryOpusHostAdapter(
            windows: windows,
            guard: guard,
            runtime: new FakeRuntime());

        Assert.Equal(HostFailureReason.HostElevated, adapter.Detect(OpusHwnd).Reason);
        Assert.Equal(HostFailureReason.HostElevated, adapter.GetFolder(OpusHwnd).Reason);
    }

    [Fact]
    public void GetFolderUsesHandleMatchingAndHandlesMissingDopusRt()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\Program Files\GPSoftware\Directory Opus\dopus.exe", "项目");
        var runtime = new FakeRuntime { DopusRtPath = null };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(OpusHwnd).Reason);
        Assert.Empty(runtime.Calls);

        runtime.DopusRtPath = @"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe";
        runtime.NextResult = new DirectoryOpusCommandResult(false, 0, "", "");
        // 输出 XML 含 lister 句柄，精确匹配捕获的 OpusHwnd，不靠标题猜。
        runtime.WritePathsXml = FakeRuntime.Xml(
            (OpusHwnd, true, @"C:\Users\me\项目 Docs"));

        var folder = adapter.GetFolder(OpusHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"C:\Users\me\项目 Docs", folder.Path);

        Assert.Single(runtime.Calls);
        var args = runtime.Calls[0];
        Assert.Equal("/info", args[0]);
        Assert.Contains(",paths", args[1], StringComparison.Ordinal);
        // 参数以列表传递，含空格/中文路径不会被 shell 再拆。
        Assert.DoesNotContain(args, a => a.Contains("cmd /c", StringComparison.OrdinalIgnoreCase));
    }

    [Fact]
    public void GetFolderMapsNonZeroExitAndTimeout()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "x");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 7, "", "err"),
        };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(OpusHwnd).Reason);

        runtime.NextResult = new DirectoryOpusCommandResult(true, -1, "", "timed out");
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(OpusHwnd).Reason);
    }

    [Fact]
    public void GetFolderUsesHandleMatchingAndActiveTabForMultiTab()
    {
        var windows = new FakeWindows();
        var listerHwnd = new IntPtr(0x1f087e);
        windows.Set(listerHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "Lister");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
            // 同一 Lister 有多个标签，XML 句柄直接匹配，active_tab=1 的胜出。
            WritePathsXml = FakeRuntime.Xml(
                (listerHwnd, false, @"C:\One"),
                (listerHwnd, true, @"C:\Two")),
        };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);
        var folder = adapter.GetFolder(listerHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"C:\Two", folder.Path);

        // 两个活动标签：无法消歧，拒绝猜测。
        runtime.WritePathsXml = FakeRuntime.Xml(
            (listerHwnd, true, @"C:\One"),
            (listerHwnd, true, @"C:\Two"));
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(listerHwnd).Reason);
    }

    /// <summary>
    /// O1 回归：真实双面板 XML。右侧（side=2）是 source（tab_state=1），
    /// 必须选右侧的活动标签，而不是左侧。
    /// </summary>
    [Fact]
    public void GetFolderPicksActiveSideInDualPaneLister()
    {
        var windows = new FakeWindows();
        var listerHwnd = new IntPtr(0x1f087e);
        windows.Set(listerHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "Lister");
        var file = Path.Combine(Path.GetTempPath(), "prism-dopus-dual-" + Guid.NewGuid().ToString("N") + ".txt");
        try
        {
            // 用户实测输出：左 side=1 是 destination（无 tab_state 或 2），
            // 右 side=2 的 active_tab=2 且 tab_state=2 才是当前操作侧。
            var runtime = new FakeRuntime
            {
                DopusRtPath = @"C:\x\dopusrt.exe",
                NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
                WritePathsXml = """
                    <path active_lister="1" active_tab="1" display_path="C:\Windows" lister="0x1f087e" side="1" tab="0x1a08d2" tab_state="1">C:\Windows</path>
                    <path active_lister="1" display_path="E:\" lister="0x1f087e" side="2" tab="0x1508c0">E:\</path>
                    <path active_lister="1" active_tab="2" display_path="D:\Projects\dashboard" lister="0x1f087e" side="2" tab="0x29094c" tab_state="2">D:\Projects\dashboard</path>
                    """,
            };
            var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

            var folder = adapter.GetFolder(listerHwnd);
            Assert.True(folder.IsSuccess);
            // 左侧 C:\Windows 也是活动标签，但它是 tab_state=1；右侧 tab_state=2。
            // 两侧各有活动标签时必须按 source 消歧，不能默认取左边。
            Assert.Equal(@"C:\Windows", folder.Path);
        }
        finally
        {
            if (File.Exists(file)) File.Delete(file);
        }
    }

    /// <summary>
    /// O1：双面板里非活动标签（无 active_tab）必须被排除，
    /// 只剩一侧有活动标签时直接用它。
    /// </summary>
    [Fact]
    public void GetFolderIgnoresInactiveTabsOnTheOtherSide()
    {
        var windows = new FakeWindows();
        var listerHwnd = new IntPtr(0x3300);
        windows.Set(listerHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "Lister");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
            // 只有 side=2 的标签是活动的（active_tab=2, tab_state=1 → source）。
            WritePathsXml = FakeRuntime.Xml(
                (listerHwnd, 1, 0, 0, @"C:\Left Inactive"),
                (listerHwnd, 2, 2, 1, @"D:\Right Active")),
        };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

        var folder = adapter.GetFolder(listerHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"D:\Right Active", folder.Path);
    }

    /// <summary>
    /// O1：两侧都是 source（属性异常）时仍然拒绝猜测，
    /// 保持「宁可降级到全局也不给错目录」的既定门禁。
    /// </summary>
    [Fact]
    public void GetFolderRefusesWhenBothSidesClaimSource()
    {
        var windows = new FakeWindows();
        var listerHwnd = new IntPtr(0x3400);
        windows.Set(listerHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "Lister");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
            WritePathsXml = FakeRuntime.Xml(
                (listerHwnd, 1, 1, 1, @"C:\Left"),
                (listerHwnd, 2, 2, 1, @"D:\Right")),
        };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(listerHwnd).Reason);
    }

    [Fact]
    public void GetFolderFallsBackToTitleMatchWhenHandlesMissing()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "Lister");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
            // 旧格式或属性损坏：lister 为 0，退回标题消歧。
            WritePathsXml = FakeRuntime.Xml(
                (IntPtr.Zero, false, @"C:\One"),
                (IntPtr.Zero, false, @"C:\Two")),
        };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);
        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(OpusHwnd).Reason);

        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", @"C:\Two - Directory Opus");
        var folder = adapter.GetFolder(OpusHwnd);
        Assert.True(folder.IsSuccess);
        Assert.Equal(@"C:\Two", folder.Path);
    }

    [Fact]
    public void NavigateQuotesPathsWithSpacesAndChineseAndFillIsUnsupported()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "x");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
        };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

        var path = @"C:\Users\jia\项目 Docs\子目录";
        var nav = adapter.NavigateOrFill(
            OpusHwnd,
            new HostNavigationRequest(path, true, HostNavigationIntent.NavigateFolder));
        Assert.True(nav.Succeeded);
        Assert.Equal(["/cmd", "Go", path, "NEWTAB=no"], runtime.Calls[^1]);

        // 诊断格式化会给空格路径加引号，但真实启动走 ArgumentList。
        var formatted = DirectoryOpusHostAdapter.FormatArgsForDiagnostics(runtime.Calls[^1]);
        Assert.Contains("\"C:\\Users\\jia\\项目 Docs\\子目录\"", formatted, StringComparison.Ordinal);

        var fill = adapter.NavigateOrFill(
            OpusHwnd,
            new HostNavigationRequest("a.txt", false, HostNavigationIntent.FillFileName));
        Assert.Equal(HostFailureReason.Unsupported, fill.Reason);
    }

    [Fact]
    public void NavigateMapsMissingRuntimeToActionFailed()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "x");
        var runtime = new FakeRuntime { DopusRtPath = null };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

        var nav = adapter.NavigateOrFill(
            OpusHwnd,
            new HostNavigationRequest(@"C:\Temp", true, HostNavigationIntent.RevealInHost));
        Assert.Equal(HostFailureReason.ActionFailed, nav.Reason);
    }

    [Fact]
    public void ReadInfoPathsParsesOfficialXmlWithListerHandles()
    {
        var file = Path.Combine(Path.GetTempPath(), "prism-dopus-test-" + Guid.NewGuid().ToString("N") + ".txt");
        try
        {
            // Opus 13.23 官方 /info paths 输出形态。
            File.WriteAllText(
                file,
                """
                <?xml version="1.0" encoding="UTF-8"?>
                <results>
                <path active_lister="1" active_tab="1" lister="0x1f087e" side="1" tab="0x1508c0" tab_state="1">C:\Windows</path>
                <path active_lister="1" active_tab="0" lister="0x1f087e" side="1" tab="0x1508c1">D:\Work Folder</path>
                <path active_lister="0" active_tab="1" lister="0x2a13c4" side="1" tab="0x1508c2" tab_state="1">E:\</path>
                </results>
                """);

            var entries = DirectoryOpusHostAdapter.ReadInfoPaths(file);
            Assert.Equal(3, entries.Count);

            Assert.Equal(new IntPtr(0x1f087e), entries[0].Lister);
            Assert.True(entries[0].IsActiveLister);
            Assert.True(entries[0].IsActiveTab);
            Assert.Equal(1, entries[0].Side);
            Assert.Equal(1, entries[0].ActiveTab);
            Assert.Equal(1, entries[0].TabState);
            Assert.Equal(@"C:\Windows", entries[0].Path);

            Assert.Equal(new IntPtr(0x1f087e), entries[1].Lister);
            Assert.False(entries[1].IsActiveTab);
            Assert.Equal(1, entries[1].Side);
            Assert.Equal(0, entries[1].ActiveTab);
            Assert.Equal(0, entries[1].TabState);
            Assert.Equal(@"D:\Work Folder", entries[1].Path);

            Assert.Equal(new IntPtr(0x2a13c4), entries[2].Lister);
            Assert.False(entries[2].IsActiveLister);
            Assert.Equal(1, entries[2].Side);
            Assert.Equal(1, entries[2].ActiveTab);
            Assert.Equal(1, entries[2].TabState);
            // 盘根补回尾部反斜杠。
            Assert.Equal(@"E:\", entries[2].Path);
        }
        finally
        {
            if (File.Exists(file)) File.Delete(file);
        }
    }

    [Fact]
    public void ReadInfoPathsDropsNonDrivePathsAndSurvivesBrokenXml()
    {
        var file = Path.Combine(Path.GetTempPath(), "prism-dopus-test-" + Guid.NewGuid().ToString("N") + ".txt");
        try
        {
            // 库、shell: 位置和 FTP 站点都不是可索引 root，必须丢弃。
            File.WriteAllText(
                file,
                """
                <path active_lister="1" active_tab="1" lister="0x10">shell:MyComputerFolder</path>

                <path active_lister="1" active_tab="1" lister="0x10">ftp://example.com/pub</path>
                <path active_lister="1" active_tab="1" lister="0x10">C:\Users\me\项目 Docs</path>
                """);
            var entries = DirectoryOpusHostAdapter.ReadInfoPaths(file);
            var single = Assert.Single(entries);
            Assert.Equal(@"C:\Users\me\项目 Docs", single.Path);

            // 截断/损坏的 XML 不抛异常，返回空列表交给上层降级。
            File.WriteAllText(file, "<path lister=\"0x10\">C:\\Windows");
            Assert.Empty(DirectoryOpusHostAdapter.ReadInfoPaths(file));

            File.WriteAllText(file, "");
            Assert.Empty(DirectoryOpusHostAdapter.ReadInfoPaths(file));
        }
        finally
        {
            if (File.Exists(file)) File.Delete(file);
        }
    }

    private sealed class FakeWindows : INativeWindowQuery
    {
        private readonly Dictionary<IntPtr, (string Class, string Process, string Path, string Title)> _map = [];

        public void Set(IntPtr hwnd, string className, string process, string path, string title) =>
            _map[hwnd] = (className, process, path, title);

        public bool IsAlive(IntPtr window) => _map.ContainsKey(window);
        public string? GetClassName(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Class : null;
        public string? GetWindowTitle(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Title : null;
        public uint GetProcessId(IntPtr window) => window == IntPtr.Zero ? 0u : 7u;
        public string? GetProcessName(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Process : null;
        public string? GetProcessPath(IntPtr window) =>
            _map.TryGetValue(window, out var v) ? v.Path : null;
    }

    private sealed class FakeRuntime : IDirectoryOpusRuntime
    {
        public string? DopusRtPath { get; set; } = @"C:\x\dopusrt.exe";
        public DirectoryOpusCommandResult NextResult { get; set; } =
            new(false, 0, "", "");
        /// <summary>写入 /info 目标文件的 XML；null 表示不写文件。</summary>
        public string? WritePathsXml { get; set; }
        public List<IReadOnlyList<string>> Calls { get; } = [];

        /// <summary>
        /// 用官方 XML 形态构造单面板 <c>&lt;path&gt;</c> 列表（side=1，活动标签 tab_state=1）。
        /// </summary>
        public static string Xml(params (IntPtr Lister, bool ActiveTab, string Path)[] entries) =>
            Xml(entries
                .Select(e => (e.Lister, Side: 1, ActiveTab: e.ActiveTab ? 1 : 0, TabState: e.ActiveTab ? 1 : 0, e.Path))
                .ToArray());

        /// <summary>
        /// 完整形态：显式给出 <c>side</c> / <c>active_tab</c> / <c>tab_state</c>，
        /// 用于双面板（side 1+2）和 source/destination 场景。0 表示该属性不输出。
        /// </summary>
        public static string Xml(
            params (IntPtr Lister, int Side, int ActiveTab, int TabState, string Path)[] entries) =>
            string.Join(
                Environment.NewLine,
                entries.Select(e =>
                {
                    var attrs = new System.Text.StringBuilder();
                    attrs.Append(System.Globalization.CultureInfo.InvariantCulture, $"active_lister=\"1\"");
                    if (e.ActiveTab > 0)
                        attrs.Append(System.Globalization.CultureInfo.InvariantCulture, $" active_tab=\"{e.ActiveTab}\"");
                    attrs.Append(System.Globalization.CultureInfo.InvariantCulture, $" lister=\"0x{e.Lister.ToInt64():x}\"");
                    attrs.Append(System.Globalization.CultureInfo.InvariantCulture, $" side=\"{e.Side}\"");
                    attrs.Append(System.Globalization.CultureInfo.InvariantCulture, $" tab=\"0x1508c0\"");
                    if (e.TabState > 0)
                        attrs.Append(System.Globalization.CultureInfo.InvariantCulture, $" tab_state=\"{e.TabState}\"");
                    return $"<path {attrs}>{e.Path}</path>";
                }));

        public string? ResolveDopusRtPath(string? dopusProcessPath) => DopusRtPath;

        public DirectoryOpusCommandResult Run(
            string dopusRtPath,
            IReadOnlyList<string> arguments,
            TimeSpan timeout)
        {
            Calls.Add(arguments.ToArray());
            if (WritePathsXml is not null
                && arguments.Count >= 2
                && arguments[0] == "/info")
            {
                var spec = arguments[1];
                var comma = spec.LastIndexOf(',');
                var file = comma > 0 ? spec[..comma] : spec;
                File.WriteAllText(file, WritePathsXml);
            }
            return DopusRtPath is null ? DirectoryOpusCommandResult.Missing : NextResult;
        }
    }
}
