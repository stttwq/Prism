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
        new("dx", "size", ""),
        new("xg", "dm", ""),
        new("cj", "dc", ""),
        new("wj", "file", ""),
        new("wjia", "folder", ""),
    ];

    [Fact]
    public void Size_Condition_With_Name()
    {
        Assert.Equal("size:>10mb 报告", FilterTriggerDetector.TryRewrite("dx >10mb 报告", Triggers));
    }

    [Fact]
    public void Size_Multiple_Conditions_Each_Own_Token()
    {
        // 条件各自成 token → broker 多条 size 过滤 AND 语义。
        Assert.Equal(
            "size:>1mb size:<100mb 报告",
            FilterTriggerDetector.TryRewrite("dx >1mb <100mb 报告", Triggers));
    }

    [Fact]
    public void Size_Bucket_And_Name_Tokens_Separated()
    {
        // "报告" 不是 size 形状 → 名字；"large" 是桶名 → 条件。
        Assert.Equal("size:large 报告", FilterTriggerDetector.TryRewrite("dx large 报告", Triggers));
    }

    [Fact]
    public void Date_Trigger_Rewrites_Named_And_Range()
    {
        Assert.Equal("dm:thisweek 报告", FilterTriggerDetector.TryRewrite("xg thisweek 报告", Triggers));
        Assert.Equal("dm:2024 报告", FilterTriggerDetector.TryRewrite("xg 2024 报告", Triggers));
        Assert.Equal(
            "dc:20240101..20240131 报告",
            FilterTriggerDetector.TryRewrite("cj 20240101..20240131 报告", Triggers));
    }

    [Fact]
    public void Flag_Trigger_Takes_No_Value()
    {
        Assert.Equal("file: 报告", FilterTriggerDetector.TryRewrite("wj 报告", Triggers));
        Assert.Equal("file:", FilterTriggerDetector.TryRewrite("wj ", Triggers));
    }

    [Fact]
    public void Folder_Trigger_Longer_Keyword_First()
    {
        // "wjia 报告" 应命中 wjia（folder）而非 wj（file）。
        var hit = FilterTriggerDetector.TryDetect("wjia 报告", Triggers);
        Assert.NotNull(hit);
        Assert.Equal("wjia", hit!.Trigger.Keyword);
    }

    [Fact]
    public void Path_Condition_With_Name()
    {
        Assert.Equal(
            @"path:D:\资料 报告",
            FilterTriggerDetector.TryRewrite(@"pp D:\资料 报告", Triggers));
        // 带空格的引号路径：两 token 合并回一个带引号路径值。
        Assert.Equal(
            @"path:""D:\my docs"" 报告",
            FilterTriggerDetector.TryRewrite(@"pp ""D:\my docs"" 报告", Triggers));
        // 无路径形 token：旧行为，整段作路径子串。
        Assert.Equal("path:docs", FilterTriggerDetector.TryRewrite("pp docs", Triggers));
    }

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
    public void Ext_Then_Name_Tokens_Keep_Both()
    {
        Assert.Equal("ext:txt 报告", FilterTriggerDetector.TryRewrite("tz txt 报告", Triggers));
    }

    [Fact]
    public void Name_Only_Terms_Search_Name_Without_Filter()
    {
        // 非 ext 形状（中文）整段作文件名——旧行为 ext:「报告」永远查不到
        Assert.Equal("报告", FilterTriggerDetector.TryRewrite("tz 报告", Triggers));
    }

    [Fact]
    public void Name_First_Ext_Last_Is_Order_Free()
    {
        Assert.Equal("ext:txt 报告", FilterTriggerDetector.TryRewrite("tz 报告 txt", Triggers));
    }

    [Fact]
    public void Numeric_Token_Treated_As_Name_Not_Ext()
    {
        // 纯数字（年份类）不当扩展名——须含字母
        Assert.Equal("2024 报告", FilterTriggerDetector.TryRewrite("tz 2024 报告", Triggers));
    }

    [Fact]
    public void Comma_Separated_Ext_Values_Preserved()
    {
        Assert.Equal("ext:txt,doc 报告", FilterTriggerDetector.TryRewrite("tz txt,doc 报告", Triggers));
    }

    [Fact]
    public void Second_Ascii_Token_Stays_Name_Not_Ext()
    {
        // 2026-08-30 用户实测回归："tz exe speed" 曾被重写为 "ext:exe,speed"——
        // 普通文件名词 "speed" 被吞成第二个扩展名，搜出一堆不相干 .exe 软件。
        // 只有第一个 ext 形 token 作过滤，其余保留为文件名搜索词。
        Assert.Equal("ext:exe speed", FilterTriggerDetector.TryRewrite("tz exe speed", Triggers));
        Assert.Equal("ext:txt 报告 txt2", FilterTriggerDetector.TryRewrite("tz txt 报告 txt2", Triggers));
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
