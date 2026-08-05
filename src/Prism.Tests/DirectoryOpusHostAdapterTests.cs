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
    public void GetFolderUsesStructuredArgsAndHandlesMissingDopusRt()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\Program Files\GPSoftware\Directory Opus\dopus.exe", "项目");
        var runtime = new FakeRuntime { DopusRtPath = null };
        var adapter = new DirectoryOpusHostAdapter(windows: windows, runtime: runtime);

        Assert.Equal(HostFailureReason.FolderUnavailable, adapter.GetFolder(OpusHwnd).Reason);
        Assert.Empty(runtime.Calls);

        runtime.DopusRtPath = @"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe";
        runtime.NextResult = new DirectoryOpusCommandResult(false, 0, "", "");
        runtime.WritePathsFile = [@"C:\Users\me\项目 Docs"];

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
    public void GetFolderAmbiguousMultiListerWithoutTitleMatchFails()
    {
        var windows = new FakeWindows();
        windows.Set(OpusHwnd, "dopus.lister", "dopus", @"C:\x\dopus.exe", "Lister");
        var runtime = new FakeRuntime
        {
            DopusRtPath = @"C:\x\dopusrt.exe",
            NextResult = new DirectoryOpusCommandResult(false, 0, "", ""),
            WritePathsFile = [@"C:\One", @"C:\Two"],
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
    public void ReadInfoPathsParsesTabSeparatedAndDriveRoots()
    {
        var file = Path.Combine(Path.GetTempPath(), "prism-dopus-test-" + Guid.NewGuid().ToString("N") + ".txt");
        try
        {
            File.WriteAllText(file, "C:\\\tLister1\r\n\"D:\\Work Folder\"\r\n\r\nshell:bad\r\n");
            var paths = DirectoryOpusHostAdapter.ReadInfoPaths(file);
            Assert.Equal(2, paths.Count);
            Assert.Equal(@"C:\", paths[0]);
            Assert.Equal(@"D:\Work Folder", paths[1]);
        }
        finally
        {
            File.Delete(file);
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
        public List<string>? WritePathsFile { get; set; }
        public List<IReadOnlyList<string>> Calls { get; } = [];

        public string? ResolveDopusRtPath(string? dopusProcessPath) => DopusRtPath;

        public DirectoryOpusCommandResult Run(
            string dopusRtPath,
            IReadOnlyList<string> arguments,
            TimeSpan timeout)
        {
            Calls.Add(arguments.ToArray());
            if (WritePathsFile is not null
                && arguments.Count >= 2
                && arguments[0] == "/info")
            {
                var spec = arguments[1];
                var comma = spec.LastIndexOf(',');
                var file = comma > 0 ? spec[..comma] : spec;
                File.WriteAllLines(file, WritePathsFile);
            }
            return DopusRtPath is null ? DirectoryOpusCommandResult.Missing : NextResult;
        }
    }
}
