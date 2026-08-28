using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K2 commit 0：FilterTriggerDetector 在 K1 期间并入但零测试覆盖。
/// 它是易错点密集区（首词+空格、长词优先、大小写、空 QueryTerms、非首词）。
/// </summary>
public sealed class FilterTriggerDetectorTests
{
    private static readonly FilterTrigger[] Triggers =
    [
        new("t", "ext", ""),
        new("tz", "ext", ""),
        new("pp", "path", ""),
    ];

    [Fact]
    public void Bare_Keyword_Does_Not_Trigger()
    {
        // 裸关键词无空格 → null（仍是普通文件搜索）
        Assert.Null(FilterTriggerDetector.TryDetect("tz", Triggers));
        Assert.Null(FilterTriggerDetector.TryRewrite("tz", Triggers));
    }

    [Fact]
    public void Keyword_With_Space_Triggers_Ext_Rewrite()
    {
        Assert.Equal("ext:pdf", FilterTriggerDetector.TryRewrite("tz pdf", Triggers));
    }

    [Fact]
    public void Path_Type_Rewrites_To_Path_Colon()
    {
        Assert.Equal("path:docs", FilterTriggerDetector.TryRewrite("pp docs", Triggers));
    }

    [Fact]
    public void Case_Insensitive_Match()
    {
        Assert.Equal("ext:pdf", FilterTriggerDetector.TryRewrite("TZ pdf", Triggers));
    }

    [Fact]
    public void Longer_Keyword_Preferred_Over_Shorter()
    {
        // triggers 含 t/tz，输入 "tz pdf" → 命中 tz 而非 t
        var hit = FilterTriggerDetector.TryDetect("tz pdf", Triggers);
        Assert.NotNull(hit);
        Assert.Equal("tz", hit!.Trigger.Keyword);
    }

    [Fact]
    public void Empty_QueryTerms_Falls_Back_To_Bare_Prefix()
    {
        // "tz " → "ext:"（broker 按无值过滤词处理）
        Assert.Equal("ext:", FilterTriggerDetector.TryRewrite("tz ", Triggers));
    }

    [Fact]
    public void Leading_Whitespace_Still_Triggers()
    {
        Assert.Equal("ext:pdf", FilterTriggerDetector.TryRewrite("  tz pdf", Triggers));
    }

    [Fact]
    public void Non_First_Word_Does_Not_Trigger()
    {
        // "a tz pdf" → null（首词 a 不命中任何触发词）
        Assert.Null(FilterTriggerDetector.TryDetect("a tz pdf", Triggers));
        Assert.Null(FilterTriggerDetector.TryRewrite("a tz pdf", Triggers));
    }

    [Fact]
    public void Unknown_FilterType_Rewrite_Returns_Null()
    {
        // FilterType 非 ext/path → TryRewrite 返回 null（TryDetect 仍命中，但无 prefix）
        var unknown = new FilterTrigger[] { new("xx", "mime", "") };
        Assert.NotNull(FilterTriggerDetector.TryDetect("xx pdf", unknown));
        Assert.Null(FilterTriggerDetector.TryRewrite("xx pdf", unknown));
    }

    [Fact]
    public void Empty_Triggers_Returns_Null()
    {
        Assert.Null(FilterTriggerDetector.TryDetect("tz pdf", []));
        Assert.Null(FilterTriggerDetector.TryRewrite("tz pdf", []));
    }

    [Fact]
    public void Empty_Or_Whitespace_Query_Returns_Null()
    {
        Assert.Null(FilterTriggerDetector.TryDetect("", Triggers));
        Assert.Null(FilterTriggerDetector.TryDetect("   ", Triggers));
    }
}
