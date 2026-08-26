using System.Text.Json;
using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K0 C1-C5：CommandDescriptor 容忍解析。新 broker + 旧 WPF 是常态组合，
/// 严格解析会让一个未知 binding 类型把整份目录解析打死。
/// </summary>
public sealed class CommandDescriptorParseTests
{
    private static CommandDescriptor Parse(string json)
    {
        using var doc = JsonDocument.Parse(json);
        return CommandDescriptor.Parse(doc.RootElement)!;
    }

    /// <summary>C1：完整已知字段 → IsUsable=true。</summary>
    [Fact]
    public void Full_Known_Fields_Parses_Usable()
    {
        var d = Parse("""
            {
              "id": "prism.settings.open",
              "title": "设置",
              "subtitle": "打开 Prism 设置",
              "icon_glyph": "\uE713",
              "owner": "ui",
              "trust": "internal",
              "danger": "normal",
              "enabled": true,
              "keywords": ["设置", "settings"],
              "input": { "kind": "none", "required": false, "prompt": "" },
              "bindings": {
                "root_search": { "priority": 1, "input": "" },
                "keyword": null
              }
            }
            """);

        Assert.Equal("prism.settings.open", d.Id);
        Assert.Equal("设置", d.Title);
        Assert.Equal("ui", d.Owner);
        Assert.Equal("normal", d.Danger);
        Assert.True(d.Enabled);
        Assert.True(d.IsUsable);
        Assert.Equal(2, d.Keywords.Count);
        Assert.Equal("none", d.Input.Kind);
        Assert.NotNull(d.Bindings.RootSearch);
        Assert.Null(d.Bindings.Keyword);
    }

    /// <summary>C2：未知 owner → IsUsable=false，不抛。</summary>
    [Fact]
    public void Unknown_Owner_Not_Usable_No_Throw()
    {
        var d = Parse("""
            {
              "id": "x", "title": "t", "subtitle": "", "icon_glyph": "",
              "owner": "alien", "trust": "", "danger": "normal",
              "enabled": true, "input": { "kind": "none" }
            }
            """);

        Assert.Equal("alien", d.Owner);
        Assert.False(d.IsUsable);
    }

    /// <summary>C3：未知 danger → IsUsable=false。</summary>
    [Fact]
    public void Unknown_Danger_Not_Usable()
    {
        var d = Parse("""
            {
              "id": "x", "title": "t", "owner": "broker", "danger": "catastrophic",
              "input": { "kind": "none" }
            }
            """);

        Assert.False(d.IsUsable);
    }

    /// <summary>C4：未知 input.kind → IsUsable=false。</summary>
    [Fact]
    public void Unknown_Input_Kind_Not_Usable()
    {
        var d = Parse("""
            {
              "id": "x", "title": "t", "owner": "broker", "danger": "normal",
              "input": { "kind": "telepathy" }
            }
            """);

        Assert.False(d.IsUsable);
    }

    /// <summary>C5：字段缺失 → 默认值不抛（missing input → kind=none/usable）。</summary>
    [Fact]
    public void Missing_Fields_Default_No_Throw()
    {
        var d = Parse("""{ "id": "x", "owner": "broker", "danger": "normal" }""");

        Assert.Equal("x", d.Id);
        Assert.Equal("", d.Title);
        Assert.True(d.IsUsable); // owner + danger known, input missing → kind=none
        Assert.Empty(d.Keywords);
        Assert.Null(d.Bindings.RootSearch);
    }

    /// <summary>C5b：非对象根 → null。</summary>
    [Fact]
    public void Non_Object_Root_Returns_Null()
    {
        using var doc = JsonDocument.Parse("[]");
        Assert.Null(CommandDescriptor.Parse(doc.RootElement));
    }
}
