using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K4b：声明式参数。descriptor 容忍解析 + 编辑行往返；结构校验（数量/重名/
/// 必填顺序）在 broker 侧，不在此重复。
/// </summary>
public sealed class CommandArgumentsTests
{
    [Fact]
    public void DescriptorArgumentsMissingMeansEmpty()
    {
        var json = System.Text.Json.JsonDocument.Parse("""
            {"id":"user.legacy","title":"L","owner":"broker","trust":"user",
                "keywords":[],"input":{"kind":"text","required":true},"bindings":{},
                "danger":"normal","enabled":true}
            """).RootElement;
        var desc = CommandDescriptor.Parse(json);

        Assert.NotNull(desc);
        Assert.Empty(desc!.Arguments);
    }

    [Fact]
    public void DescriptorArgumentsParseTolerantly()
    {
        var json = System.Text.Json.JsonDocument.Parse("""
            {"id":"user.args","title":"A","owner":"broker","trust":"user",
                "keywords":[],"input":{"kind":"text","required":true},"bindings":{},
                "danger":"normal","enabled":true,
                "arguments":[
                    {"name":"width","required":true},
                    {"name":"height","default":"10"},
                    {"name":"bad","required":"yes"}
                ]}
            """).RootElement;
        var desc = CommandDescriptor.Parse(json);

        Assert.NotNull(desc);
        Assert.Equal(3, desc!.Arguments.Count);
        Assert.Equal("width", desc.Arguments[0].Name);
        Assert.True(desc.Arguments[0].Required);
        Assert.Equal("height", desc.Arguments[1].Name);
        Assert.False(desc.Arguments[1].Required);
        Assert.Equal("10", desc.Arguments[1].Default);
        // required 字段非布尔 → 容忍为 false，不抛
        Assert.False(desc.Arguments[2].Required);
    }

    [Fact]
    public void CommandEditItemArgumentsRoundTripToDefinition()
    {
        var json = System.Text.Json.JsonDocument.Parse("""
            {"id":"user.args","title":"A","owner":"broker","trust":"user",
                "keywords":[],"input":{"kind":"text","required":true},"bindings":{},
                "danger":"normal","enabled":true,
                "arguments":[{"name":"width","required":true},{"name":"height","default":"10"}]}
            """).RootElement;
        var item = new CommandEditItem(CommandDescriptor.Parse(json)!);

        Assert.Equal(2, item.Arguments.Count);
        Assert.True(item.Arguments[0].Required);
        Assert.Equal("10", item.Arguments[1].Default);

        item.Handler = "open_url";
        item.UrlTemplate = "https://example.test/?w={arg.width}&h={arg.height}";
        item.Title = "A";
        var def = item.ToDefinition();

        Assert.Equal(2, def.Arguments.Count);
        Assert.Equal("width", def.Arguments[0].Name);
        Assert.True(def.Arguments[0].Required);
        Assert.Equal("height", def.Arguments[1].Name);
        Assert.Equal("10", def.Arguments[1].Default);
    }

    [Fact]
    public void ToDefinitionDropsEmptyNamedRows()
    {
        var item = new CommandEditItem { Id = "user.x", Title = "x", Handler = "open_url" };
        item.UrlTemplate = "https://example.test/?q={arg.q}";
        item.Arguments.Add(new CommandArgumentEditItem { Name = "q", Required = true });
        item.Arguments.Add(new CommandArgumentEditItem { Name = "", Required = true });

        var def = item.ToDefinition();
        var arg = Assert.Single(def.Arguments);
        Assert.Equal("q", arg.Name);
    }
}
