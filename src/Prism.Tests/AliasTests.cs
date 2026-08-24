using System.Text.Json;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 别名系统（2026-08-21 设想）：协议解析与设置页 VM 行为。
/// </summary>
public sealed class AliasTests
{
    [Fact]
    public void AliasListParsesTargetsWordsAndBoundAt()
    {
        using var doc = JsonDocument.Parse("""
            {"type":"alias_items","items":[
              {"target":{"kind":"file","value":"C:\\a.exe"},"words":["wx","微信"],"bound_at_utc":1234},
              {"target":{"kind":"directory","value":"C:\\资料"},"words":["zl"],"bound_at_utc":100},
              {"target":{"kind":"bad"},"words":[]}
            ]}
            """);
        var items = PipeClient.ParseAliasList(doc.RootElement);
        Assert.Equal(2, items.Count); // 形状不完整的第三条静默跳过
        Assert.Equal("file", items[0].Target.Kind);
        Assert.Equal(@"C:\a.exe", items[0].Target.Value);
        Assert.Equal(["wx", "微信"], items[0].Words);
        Assert.Equal(1234L, items[0].BoundAtUtc);
        Assert.Equal("wx、微信", items[0].WordsText);
        Assert.Equal("directory", items[1].Target.Kind);
    }

    [Fact]
    public void AliasListHandlesMissingItems()
    {
        using var doc = JsonDocument.Parse("""{"type":"alias_items"}""");
        Assert.Empty(PipeClient.ParseAliasList(doc.RootElement));
    }

    /// <summary>别名行经既有 SearchResult 解析管道：kind app/file/folder 与空 spans 容错。</summary>
    [Fact]
    public void AliasSearchRowParsesThroughResultPipeline()
    {
        using var doc = JsonDocument.Parse("""
            {"kind":"app","title":"weixin.exe","subtitle":"C:\\Tools\\weixin.exe",
             "execute_id":"C:\\Tools\\weixin.exe","match_spans":[]}
            """);
        var row = PipeClient.ParseResult(doc.RootElement);
        Assert.Equal("app", row.Kind);
        Assert.Empty(row.MatchSpans);
        Assert.Equal(SearchResultKind.App, row.ResultKind);
    }

    /// <summary>
    /// 2026-08-24 修复锚定：broker 别名行的 score 是「i32::MAX - 绑定秒」（升序
    /// 排序键取反，见 ipc.rs alias_search_hits），上界恰为 int.MaxValue——前端
    /// TryGetInt32 必须能整段解析 match_metadata（曾因 u32 取反越界被静默丢弃）。
    /// </summary>
    [Fact]
    public void AliasRowScoreStaysWithinInt32ParseDomain()
    {
        using var doc = JsonDocument.Parse("""
            {"kind":"app","title":"weixin.exe","subtitle":"C:\\Tools\\weixin.exe",
             "execute_id":"C:\\Tools\\weixin.exe","match_spans":[],
             "match_metadata":{"kind":"literal","class":0,"position":0,"score":2147483647}}
            """);
        var row = PipeClient.ParseResult(doc.RootElement);
        Assert.NotNull(row.MatchMetadata);
        Assert.Equal(0, row.MatchMetadata!.Class);
        Assert.Equal(int.MaxValue, row.MatchMetadata.Score);
    }
}
