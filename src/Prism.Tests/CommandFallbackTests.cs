using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K4a：无结果回退命令。合成行形状 + 编辑项 fallback 标志往返。
/// VM 级合成路径（ApplySearchResponse）依赖 CommandCatalog 具体类无注入缝，
/// 不在此覆盖（扩产线代码换测试缝不划算）；核心形状由 broker 侧测试兜底。
/// </summary>
public sealed class CommandFallbackTests
{
    private static CommandDescriptor Descriptor(bool fallback) => new(
        Id: "user.test-cmd",
        Title: "测试命令",
        Subtitle: "",
        IconGlyph: "",
        Owner: "broker",
        Trust: "user",
        Keywords: [],
        Input: new CommandInputDto("text", true, ""),
        Bindings: new CommandBindingsDto(null, null, null, null, null),
        Danger: "normal",
        Enabled: true,
        IsUsable: true,
        Fallback: fallback);

    [Fact]
    public void FallbackCommandRowCarriesQueryAndStableRowKey()
    {
        var row = SearchResult.FallbackCommand(Descriptor(fallback: true), "hello");

        Assert.Equal("command", row.Kind);
        Assert.Equal("user.test-cmd", row.ExecuteId);
        Assert.Equal("hello", row.FallbackQuery);
        Assert.Equal("fbcmd:user.test-cmd:hello", row.RowKey);
        // Enter 路径走 command target（broker 复核），执行 id 不退化为路径。
        Assert.Equal("command", row.ExecutionTarget.Kind);
    }

    [Fact]
    public void DescriptorFallbackFlagMissingMeansFalse()
    {
        // 容忍解析：旧 broker 目录无 fallback 字段 → false，不抛异常。
        var json = System.Text.Json.JsonDocument.Parse("""
            {"id":"user.legacy","title":"L","owner":"broker","trust":"user",
                "keywords":[],"input":{"kind":"text","required":true},"bindings":{},
                "danger":"normal","enabled":true}
            """).RootElement;
        var desc = CommandDescriptor.Parse(json);

        Assert.NotNull(desc);
        Assert.False(desc!.Fallback);
        Assert.True(Descriptor(fallback: true).Fallback);
    }

    [Fact]
    public void CommandEditItemFallbackRoundTripsToDefinition()
    {
        var item = new CommandEditItem(Descriptor(fallback: true));
        Assert.True(item.Fallback);

        item.Handler = "open_url";
        item.UrlTemplate = "https://example.com/?q={query}";
        item.Title = "测试命令";
        var def = item.ToDefinition();
        Assert.True(def.Fallback);

        var off = new CommandEditItem(Descriptor(fallback: false));
        Assert.False(off.ToDefinition().Fallback);
    }
}
