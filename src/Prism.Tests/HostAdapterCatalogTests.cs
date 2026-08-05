using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Xunit;

namespace Prism.Tests;

/// <summary>G4 步骤 5：HostAdapterCatalog 与设置开关默认值 / 热更新。</summary>
public sealed class HostAdapterCatalogTests
{
    [Fact]
    public void CatalogWiresRealAdaptersDefaultingToDisabled()
    {
        var adapters = HostAdapterCatalog.Create(Settings.Default);
        Assert.Equal(3, adapters.Count);
        Assert.Equal(HostKind.Explorer, adapters[0].Kind);
        Assert.IsType<ExplorerHostAdapter>(adapters[0]);
        Assert.False(adapters[0].IsEnabled);

        Assert.Equal(HostKind.SystemFileDialog, adapters[1].Kind);
        Assert.IsType<DisabledHostAdapter>(adapters[1]);
        Assert.False(adapters[1].IsEnabled);

        Assert.Equal(HostKind.DirectoryOpus, adapters[2].Kind);
        Assert.IsType<DirectoryOpusHostAdapter>(adapters[2]);
        Assert.False(adapters[2].IsEnabled);
    }

    [Fact]
    public void CatalogIsEnabledTracksMutableSettingsDelegate()
    {
        var settings = Settings.Default;
        var adapters = HostAdapterCatalog.Create(() => settings);

        Assert.False(adapters[0].IsEnabled);
        Assert.False(adapters[2].IsEnabled);

        settings = settings with
        {
            ExplorerHostIntegrationEnabled = true,
            DirectoryOpusHostIntegrationEnabled = true,
        };

        Assert.True(adapters[0].IsEnabled);
        Assert.True(adapters[2].IsEnabled);
        // SystemFileDialog 本轮仍固定关闭。
        Assert.False(adapters[1].IsEnabled);
    }

    [Fact]
    public void ControllerCaptureUsesEnabledCatalogAdaptersOnly()
    {
        var settings = Settings.Default with { ExplorerHostIntegrationEnabled = false };
        var adapters = HostAdapterCatalog.Create(() => settings);
        var controller = new HostScopeController(
            adapters,
            new AlwaysOkValidator(),
            new AlwaysAliveProbe());

        var host = controller.Capture(new IntPtr(0x42));
        Assert.Equal(HostDetectionStatus.NoSupportedHost, host.Status);
        Assert.Equal(SearchScope.Global, controller.Scope);
    }

    [Fact]
    public void DisabledCatalogAdaptersReportAdapterDisabledWithoutPaths()
    {
        // 关开关时 Detect 返回 AdapterDisabled，不产生可用路径，也不抛错。
        var adapters = HostAdapterCatalog.Create(Settings.Default);
        var explorer = adapters[0];
        var opus = adapters[2];
        var hwnd = new IntPtr(0x99);

        Assert.Equal(HostFailureReason.AdapterDisabled, explorer.Detect(hwnd).Reason);
        Assert.Equal(HostFailureReason.AdapterDisabled, explorer.GetFolder(hwnd).Reason);
        Assert.Null(explorer.GetFolder(hwnd).Path);

        Assert.Equal(HostFailureReason.AdapterDisabled, opus.Detect(hwnd).Reason);
        Assert.Equal(HostFailureReason.AdapterDisabled, opus.GetFolder(hwnd).Reason);
        Assert.Null(opus.GetFolder(hwnd).Path);

        Assert.IsType<DisabledHostAdapter>(adapters[1]);
        Assert.Equal(HostFailureReason.AdapterDisabled, adapters[1].Detect(hwnd).Reason);
    }

    private sealed class AlwaysOkValidator : IRootValidator
    {
        public RootRejection? Validate(string? path, out string normalized)
        {
            normalized = path ?? "";
            return null;
        }
    }

    private sealed class AlwaysAliveProbe : IHostWindowProbe
    {
        public bool IsAlive(IntPtr window) => window != IntPtr.Zero;
    }
}
