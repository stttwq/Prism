using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// G4 步骤 3：当前目录 / 全局 范围状态机。覆盖 prd 验收项 2、3：
/// 识别失败不沿用旧目录、宿主失效回到全局、`Ctrl+G` 在 root 失效时不可切回、
/// 范围标签文案、总开关关闭后的行为。
/// </summary>
public sealed class HostScopeControllerTests
{
    private static readonly IntPtr Explorer = new(0x1234);
    private static readonly IntPtr Other = new(0x5678);

    [Fact]
    public void DetectedHostScopesToCurrentDirectoryAndLabelsTheRoot()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out var validator, out _);

        var host = controller.Capture(Explorer);

        Assert.Equal(HostDetectionStatus.Detected, host.Status);
        Assert.Equal(HostKind.Explorer, host.Kind);
        Assert.Equal(@"C:\Users\me\Docs", controller.Root);
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);
        Assert.True(controller.IsScopeLabelVisible);
        Assert.Equal("当前目录：Docs", controller.ScopeLabel);
        Assert.Contains(@"C:\Users\me\Docs", controller.ScopeTooltip);
        Assert.Equal("", controller.Notice);
        Assert.Equal(@"C:\Users\me\Docs", controller.Apply(SearchContext.Default).Root);
        Assert.Equal(1, validator.Calls);
    }

    [Fact]
    public void UnsupportedForegroundHostSearchesGloballyWithoutANotice()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);

        var host = controller.Capture(Other);

        Assert.Equal(HostDetectionStatus.NoSupportedHost, host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Null(controller.Root);
        Assert.False(controller.IsScopeLabelVisible);
        Assert.Equal("", controller.Notice);
    }

    [Fact]
    public void FailedDetectionClearsTheRootInsteadOfReusingTheLastOne()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);

        adapter.FolderReason = HostFailureReason.FolderUnavailable;
        var host = controller.Capture(Explorer);

        Assert.Equal(HostDetectionStatus.FolderUnavailable, host.Status);
        Assert.Equal(HostKind.None, host.Kind);
        Assert.Null(host.Root);
        Assert.Null(controller.Root);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Contains("已回到全局搜索", controller.Notice);
        Assert.Null(controller.Apply(SearchContext.Default).Root);
    }

    [Theory]
    [InlineData(RootRejection.NotFound, HostDetectionStatus.RootInvalid)]
    [InlineData(RootRejection.Unsupported, HostDetectionStatus.RootInvalid)]
    [InlineData(RootRejection.AccessDenied, HostDetectionStatus.AccessDenied)]
    [InlineData(RootRejection.VolumeNotIndexed, HostDetectionStatus.RootNotIndexed)]
    public void InvalidOrUnindexedRootsFallBackToGlobalWithAnExplanation(
        RootRejection rejection,
        HostDetectionStatus expected)
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\ghost");
        var controller = Controller(adapter, out var validator, out _);
        validator.Rejection = rejection;

        var host = controller.Capture(Explorer);

        Assert.Equal(expected, host.Status);
        Assert.Null(controller.Root);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Contains("已回到全局搜索", controller.Notice);
    }

    [Fact]
    public void ClosedHostWindowDuringCaptureFallsBackToGlobal()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out var probe);
        probe.Dead.Add(Explorer);

        var host = controller.Capture(Explorer);

        Assert.Equal(HostDetectionStatus.HostGone, host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Contains("已回到全局搜索", controller.Notice);
    }

    [Fact]
    public void ToggleReturnsToCurrentDirectoryOnlyWhileTheRootStaysValid()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out var validator, out var probe);
        controller.Capture(Explorer);

        Assert.True(controller.ToggleScope());
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.True(controller.IsScopeLabelVisible);
        Assert.Equal("全局", controller.ScopeLabel);
        Assert.Contains(@"C:\Users\me\Docs", controller.ScopeTooltip);

        Assert.True(controller.ToggleScope());
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);
        Assert.Equal(@"C:\Users\me\Docs", controller.Root);

        // 回到全局后目录被删除：Ctrl+G 不允许再切回，并且 root 被清空。
        Assert.True(controller.ToggleScope());
        validator.Rejection = RootRejection.NotFound;
        Assert.False(controller.ToggleScope());
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Null(controller.Root);
        Assert.False(controller.IsScopeLabelVisible);
        Assert.Contains("已回到全局搜索", controller.Notice);

        // root 已清空，再按 Ctrl+G 只提示没有可用目录。
        validator.Rejection = null;
        Assert.False(controller.ToggleScope());
        Assert.Equal("没有可用的当前目录，保持全局搜索", controller.Notice);
    }

    [Fact]
    public void ToggleRefusesWhenTheCapturedHostWindowIsGone()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out var probe);
        controller.Capture(Explorer);
        Assert.True(controller.ToggleScope());

        probe.Dead.Add(Explorer);
        Assert.False(controller.ToggleScope());

        Assert.Equal(HostDetectionStatus.HostGone, controller.Host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Null(controller.Root);
        Assert.Contains("原窗口已关闭", controller.Notice);
    }

    [Fact]
    public void MasterSwitchOffSkipsDetectionAndBlocksTheToggle()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);
        var detectsBeforeDisable = adapter.DetectCalls;
        var changes = 0;
        controller.Changed += () => changes++;

        controller.SetCurrentDirectoryEnabled(false);

        Assert.Equal(1, changes);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Null(controller.Root);
        Assert.Equal("", controller.Notice);
        Assert.False(controller.ToggleScope());
        Assert.Equal("当前目录搜索已在设置中关闭", controller.Notice);

        var host = controller.Capture(Explorer);
        Assert.Equal(HostDetectionStatus.FeatureDisabled, host.Status);
        Assert.Equal(detectsBeforeDisable, adapter.DetectCalls);
        Assert.False(controller.IsScopeLabelVisible);

        // 重新打开后下一次呼出才恢复当前目录范围。
        controller.SetCurrentDirectoryEnabled(true);
        Assert.Equal(SearchScope.Global, controller.Scope);
        controller.Capture(Explorer);
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);
    }

    [Fact]
    public void DisabledAdapterNeitherDetectsNorBreaksGlobalSearch()
    {
        var controller = new HostScopeController(
            DisabledHostAdapter.SupportMatrix,
            new FakeValidator(),
            new FakeProbe());

        var host = controller.Capture(Explorer);

        Assert.Equal(HostDetectionStatus.NoSupportedHost, host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Equal("", controller.Notice);
        Assert.Null(controller.Apply(SearchContext.Default).Root);
    }

    [Fact]
    public void AdapterExceptionsDegradeInsteadOfPropagating()
    {
        var adapter = new FakeAdapter(HostKind.DirectoryOpus, Explorer, @"C:\Users\me\Docs")
        {
            ThrowOnDetect = true,
        };
        var controller = Controller(adapter, out _, out _);

        var host = controller.Capture(Explorer);

        Assert.Equal(HostDetectionStatus.DetectFailed, host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Contains("已回到全局搜索", controller.Notice);
    }

    [Fact]
    public void BackendReportedRootRejectionInvalidatesTheScope()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);

        controller.Invalidate(RootRejection.VolumeNotIndexed);

        Assert.Equal(HostDetectionStatus.RootNotIndexed, controller.Host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Null(controller.Root);
        Assert.Contains("不在索引中", controller.Notice);
    }

    [Fact]
    public void TryRevealInHostNavigatesFoldersAndRevealsFilesWithCorrectIntent()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);

        // 用户切到全局后，Ctrl+Enter 仍应交回原宿主。
        Assert.True(controller.ToggleScope());
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.True(controller.Host.HasUsableRoot);

        var folder = controller.TryRevealInHost(@"C:\Users\me\Docs\Project", isDirectory: true);
        Assert.True(folder.Attempted);
        Assert.True(folder.Succeeded);
        Assert.Equal(1, adapter.NavigateCalls);
        Assert.Equal(Explorer, adapter.LastNavigateWindow);
        Assert.NotNull(adapter.LastRequest);
        Assert.Equal(@"C:\Users\me\Docs\Project", adapter.LastRequest!.Path);
        Assert.True(adapter.LastRequest.IsDirectory);
        Assert.Equal(HostNavigationIntent.NavigateFolder, adapter.LastRequest.Intent);

        var file = controller.TryRevealInHost(@"C:\Users\me\Docs\readme.txt", isDirectory: false);
        Assert.True(file.Succeeded);
        Assert.Equal(2, adapter.NavigateCalls);
        Assert.Equal(HostNavigationIntent.RevealInHost, adapter.LastRequest!.Intent);
        Assert.False(adapter.LastRequest.IsDirectory);
        // 定位成功/进行中都不清空 host。
        Assert.True(controller.Host.HasUsableRoot);
        Assert.Equal(SearchScope.Global, controller.Scope);
    }

    [Fact]
    public void TryRevealActionFailedKeepsRootAndDoesNotFallbackAsUnavailable()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs")
        {
            NavigateResult = HostNavigation.Failure(HostFailureReason.ActionFailed),
        };
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);

        var result = controller.TryRevealInHost(@"C:\Users\me\Docs\a.txt", isDirectory: false);

        Assert.True(result.Attempted);
        Assert.False(result.Succeeded);
        Assert.Equal(HostFailureReason.ActionFailed, result.Reason);
        Assert.Equal(1, adapter.NavigateCalls);
        Assert.True(controller.Host.HasUsableRoot);
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);
        Assert.Equal(@"C:\Users\me\Docs", controller.Root);
    }

    [Fact]
    public void TryRevealHostGoneInvalidatesTheScope()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out var probe);
        controller.Capture(Explorer);
        probe.Dead.Add(Explorer);

        var result = controller.TryRevealInHost(@"C:\Users\me\Docs\a.txt", isDirectory: false);

        Assert.True(result.Attempted);
        Assert.False(result.Succeeded);
        Assert.Equal(HostFailureReason.HostGone, result.Reason);
        Assert.Equal(0, adapter.NavigateCalls);
        Assert.Equal(HostDetectionStatus.HostGone, controller.Host.Status);
        Assert.Null(controller.Root);
        Assert.Equal(SearchScope.Global, controller.Scope);
    }

    [Fact]
    public void TryRevealHostElevatedFromAdapterInvalidatesTheScope()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs")
        {
            NavigateResult = HostNavigation.Failure(HostFailureReason.HostElevated),
        };
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);

        var result = controller.TryRevealInHost(@"C:\Users\me\Docs\a.txt", isDirectory: false);

        Assert.True(result.Attempted);
        Assert.False(result.Succeeded);
        Assert.Equal(HostFailureReason.HostElevated, result.Reason);
        Assert.Equal(HostDetectionStatus.NoSupportedHost, controller.Host.Status);
        Assert.Null(controller.Root);
        Assert.Equal(SearchScope.Global, controller.Scope);
    }

    [Fact]
    public void TryRevealWithoutHostOrDisabledAdapterIsUnavailable()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);

        Assert.False(controller.TryRevealInHost(@"C:\a.txt", false).Attempted);
        Assert.Equal(0, adapter.NavigateCalls);

        controller.Capture(Explorer);
        adapter.IsEnabled = false;
        Assert.False(controller.TryRevealInHost(@"C:\a.txt", false).Attempted);
        Assert.Equal(0, adapter.NavigateCalls);

        adapter.IsEnabled = true;
        adapter.Capabilities = HostCapability.ReadFolder; // 无 RevealInHost
        Assert.False(controller.TryRevealInHost(@"C:\a.txt", false).Attempted);
        Assert.Equal(0, adapter.NavigateCalls);
    }

    [Fact]
    public void TryRevealUnsupportedKeepsRootAndMarksAttemptedFailure()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs")
        {
            NavigateResult = HostNavigation.Failure(HostFailureReason.Unsupported),
        };
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);

        var result = controller.TryRevealInHost(@"C:\Users\me\Docs\a.txt", isDirectory: false);

        Assert.True(result.Attempted);
        Assert.False(result.Succeeded);
        Assert.Equal(HostFailureReason.Unsupported, result.Reason);
        Assert.True(controller.Host.HasUsableRoot);
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);
        Assert.Equal(@"C:\Users\me\Docs", controller.Root);
    }

    [Fact]
    public void TryRevealDetectExceptionIsActionFailedWithoutClearingRoot()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        controller.Capture(Explorer);
        adapter.ThrowOnDetect = true;

        var result = controller.TryRevealInHost(@"C:\Users\me\Docs\a.txt", isDirectory: false);

        Assert.True(result.Attempted);
        Assert.False(result.Succeeded);
        Assert.Equal(HostFailureReason.ActionFailed, result.Reason);
        Assert.Equal(0, adapter.NavigateCalls);
        Assert.True(controller.Host.HasUsableRoot);
        Assert.Equal(@"C:\Users\me\Docs", controller.Root);
    }

    [Theory]
    [InlineData("relative", RootRejection.NotAbsolute)]
    [InlineData(@"C:docs", RootRejection.NotAbsolute)]
    [InlineData(@"\\server\share", RootRejection.Unsupported)]
    [InlineData(@"C:\a\..\b", RootRejection.Unsupported)]
    public void LocalValidatorRejectsPathsPrismCannotScope(string path, RootRejection expected)
    {
        var validator = new FileSystemRootValidator();
        Assert.Equal(expected, validator.Validate(path, out _));
    }

    [Fact]
    public void LocalValidatorNormalizesAnExistingDirectory()
    {
        var directory = Path.Combine(Path.GetTempPath(), "prism-host-scope", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var validator = new FileSystemRootValidator();
            var rejection = validator.Validate(directory.Replace('\\', '/') + "/", out var normalized);
            Assert.Null(rejection);
            Assert.Equal(directory.TrimEnd('\\'), normalized, ignoreCase: true);

            var missing = Path.Combine(directory, "nope");
            Assert.Equal(RootRejection.NotFound, validator.Validate(missing, out _));

            var file = Path.Combine(directory, "file.txt");
            File.WriteAllText(file, "x");
            Assert.Equal(RootRejection.NotADirectory, validator.Validate(file, out _));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void ComputeCaptureIsPureUntilApplied()
    {
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        var changes = 0;
        controller.Changed += () => changes++;

        var result = controller.ComputeCapture(Explorer);

        // 不修改状态、不触发事件：后台线程可安全调用。
        Assert.Equal(0, changes);
        Assert.Equal(HostDetectionStatus.Detected, result.Status);
        Assert.Equal(HostKind.None, controller.Host.Kind);
        Assert.Equal(SearchScope.Global, controller.Scope);
        Assert.Null(controller.Root);
        Assert.Equal("", controller.Notice);

        // 重复计算幂等。
        Assert.Equal(result, controller.ComputeCapture(Explorer));
        Assert.Equal(0, changes);
    }

    [Fact]
    public void ResetComputeApplyMatchesDirectCapture()
    {
        // 成功路径：三段式与直接 Capture 状态一致。
        var adapter = new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs");
        var controller = Controller(adapter, out _, out _);
        var changes = 0;
        controller.Changed += () => changes++;

        controller.ResetForCapture();
        Assert.Equal(1, changes);
        controller.ApplyCapture(controller.ComputeCapture(Explorer));

        Assert.Equal(2, changes);
        Assert.Equal(HostDetectionStatus.Detected, controller.Host.Status);
        Assert.Equal(SearchScope.CurrentDirectory, controller.Scope);
        Assert.Equal(@"C:\Users\me\Docs", controller.Root);
        Assert.Equal("", controller.Notice);
        Assert.True(controller.IsScopeLabelVisible);

        // 降级路径：字段逐一与直接 Capture 等价。
        controller.ResetForCapture();
        adapter.FolderReason = HostFailureReason.FolderUnavailable;
        controller.ApplyCapture(controller.ComputeCapture(Explorer));

        var other = Controller(
            new FakeAdapter(HostKind.Explorer, Explorer, @"C:\Users\me\Docs")
            {
                FolderReason = HostFailureReason.FolderUnavailable,
            },
            out _, out _);
        var direct = other.Capture(Explorer);

        Assert.Equal(direct, controller.Host);
        Assert.Equal(other.Scope, controller.Scope);
        Assert.Equal(other.Notice, controller.Notice);
    }

    private static HostScopeController Controller(
        FakeAdapter adapter,
        out FakeValidator validator,
        out FakeProbe probe)
    {
        validator = new FakeValidator();
        probe = new FakeProbe();
        return new HostScopeController([adapter], validator, probe);
    }

    private sealed class FakeAdapter(HostKind kind, IntPtr window, string folder) : IHostAdapter
    {
        public HostKind Kind { get; } = kind;
        public bool IsEnabled { get; set; } = true;
        public HostCapability Capabilities { get; set; } =
            HostCapability.ReadFolder | HostCapability.NavigateFolder | HostCapability.RevealInHost;
        public HostFailureReason FolderReason { get; set; } = HostFailureReason.None;
        public HostNavigation NavigateResult { get; set; } = HostNavigation.Success;
        public bool ThrowOnDetect { get; set; }
        public int DetectCalls { get; private set; }
        public int NavigateCalls { get; private set; }
        public IntPtr LastNavigateWindow { get; private set; }
        public HostNavigationRequest? LastRequest { get; private set; }

        public HostDetection Detect(IntPtr foregroundWindow)
        {
            DetectCalls++;
            if (ThrowOnDetect) throw new InvalidOperationException("adapter blew up");
            return foregroundWindow == window
                ? HostDetection.Host(Kind, Capabilities)
                : HostDetection.NotHost(Kind, HostFailureReason.NotThisHost);
        }

        public HostFolder GetFolder(IntPtr hostWindow) =>
            FolderReason == HostFailureReason.None
                ? HostFolder.Success(folder)
                : HostFolder.Failure(FolderReason);

        public HostNavigation NavigateOrFill(IntPtr hostWindow, HostNavigationRequest request)
        {
            NavigateCalls++;
            LastNavigateWindow = hostWindow;
            LastRequest = request;
            return NavigateResult;
        }
    }

    private sealed class FakeValidator : IRootValidator
    {
        public RootRejection? Rejection { get; set; }
        public int Calls { get; private set; }

        public RootRejection? Validate(string? path, out string normalized)
        {
            Calls++;
            normalized = Rejection is null ? path?.Trim() ?? "" : "";
            return Rejection;
        }
    }

    private sealed class FakeProbe : IHostWindowProbe
    {
        public HashSet<IntPtr> Dead { get; } = [];
        public bool IsAlive(IntPtr window) => window != IntPtr.Zero && !Dead.Contains(window);
    }
}
