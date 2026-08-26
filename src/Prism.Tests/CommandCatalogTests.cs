using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K0 C6-C9：CommandCatalog 过滤逻辑。
/// 真管道双向通信由 PipeClientCommandFeatureTests 握手测试覆盖（不在此重复——
/// PipeChannel 内部 BoundedLineReader 与测试侧 StreamReader 共用同一
/// NamedPipeServerStream，第二轮读取必死锁）。此处只测不需要管道的部分：
/// - CommandsAvailable=false → RefreshAsync 直接清空（不发请求）
/// - Clear 重置
/// - CommandHandlers.IsKnown 内置注册表
/// 过滤逻辑（IsUsable + UI-owned 须已知 handler）由 CommandDescriptorParseTests
/// 的 IsUsable 判定 + 本文件的 CommandHandlers.IsKnown 联合覆盖。
/// </summary>
public sealed class CommandCatalogTests
{
    /// <summary>C9b：CommandsAvailable=false → RefreshAsync 直接清空，不发 command_list。</summary>
    [Fact]
    public async Task Not_Available_Clears_Without_Request()
    {
        // 不连接——PipeClient.CommandsAvailable 恒 false。
        var client = new PipeClient("prism-test-catalog-nonexistent-" + Guid.NewGuid().ToString("N"));
        var catalog = new CommandCatalog(client);

        await catalog.RefreshAsync();

        Assert.False(catalog.IsAvailable);
        Assert.Empty(catalog.Snapshot);
        Assert.Equal(0ul, catalog.Generation);
    }

    /// <summary>C9c：Clear 重置全部状态。</summary>
    [Fact]
    public void Clear_Resets_State()
    {
        var client = new PipeClient("prism-test-catalog-clear-" + Guid.NewGuid().ToString("N"));
        var catalog = new CommandCatalog(client);

        // 初始状态就是空，Clear 不应抛。
        catalog.Clear();

        Assert.False(catalog.IsAvailable);
        Assert.Empty(catalog.Snapshot);
        Assert.Equal(0ul, catalog.Generation);
    }

    /// <summary>C6/C8：CommandHandlers.IsKnown 内置注册表。</summary>
    [Fact]
    public void Known_Handler_Registry_Contains_Settings()
    {
        Assert.True(CommandHandlers.IsKnown("prism.settings.open"));
        Assert.False(CommandHandlers.IsKnown("prism.unknown.command"));
        Assert.False(CommandHandlers.IsKnown(""));
    }
}
