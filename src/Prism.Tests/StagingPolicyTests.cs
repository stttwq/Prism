using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>暂存区纯策略锚（2026-08-22 暂存区计划阶段二）。</summary>
public sealed class StagingPolicyTests
{
    private static List<StagingItem> Items(params (string path, string? workset)[] entries) =>
        entries.Select(e => new StagingItem(e.path, e.workset)).ToList();

    [Fact]
    public void AddAppendsWithInsertionOrder_NoRegrouping()
    {
        var items = Items();
        Assert.Equal(StagingAddResult.Added, StagingPolicy.Add(items, "C:\\a.txt", 5));
        Assert.Equal(StagingAddResult.Added, StagingPolicy.Add(items, "C:\\b.txt", 5));
        Assert.Equal("C:\\a.txt", items[0].Path);
        Assert.Equal("C:\\b.txt", items[1].Path);
        Assert.Null(items[0].Workset);
    }

    [Fact]
    public void AddBeyondCapacityEvictsEarliestUnmarked()
    {
        var items = Items(("C:\\a", null), ("C:\\b", null), ("C:\\c", null));
        Assert.Equal(StagingAddResult.Added, StagingPolicy.Add(items, "C:\\d", 3));
        // a（最早未标记）被挤掉，新文件接末尾——顺序即扔进顺序。
        Assert.Equal(["C:\\b", "C:\\c", "C:\\d"], items.Select(i => i.Path).ToArray());
    }

    [Fact]
    public void MarkedItemsAreNeverEvicted()
    {
        var items = Items(("C:\\a", "8月报告"), ("C:\\b", null), ("C:\\c", null));
        Assert.Equal(StagingAddResult.Added, StagingPolicy.Add(items, "C:\\d", 3));
        // 最早未标记是 b，标记项 a 保留。
        Assert.Equal(["C:\\a", "C:\\c", "C:\\d"], items.Select(i => i.Path).ToArray());
        Assert.Equal("8月报告", items[0].Workset);
    }

    [Fact]
    public void AllMarkedAndFull_RefusesAndKeepsListUnchanged()
    {
        var items = Items(("C:\\a", "W"), ("C:\\b", "W"), ("C:\\c", "W"));
        Assert.Equal(StagingAddResult.Full, StagingPolicy.Add(items, "C:\\d", 3));
        Assert.Equal(3, items.Count);
        Assert.Equal(["C:\\a", "C:\\b", "C:\\c"], items.Select(i => i.Path).ToArray());
    }

    [Theory]
    [InlineData("C:\\same.txt", "c:\\SAME.txt")] // 大小写不敏感
    [InlineData("C:\\x", "C:\\x")]               // 完全相同
    public void AddIgnoresDuplicatePaths(string first, string second)
    {
        var items = Items();
        StagingPolicy.Add(items, first, 5);
        Assert.Equal(StagingAddResult.Duplicate, StagingPolicy.Add(items, second, 5));
        Assert.Single(items);
    }

    [Fact]
    public void AddRejectsEmptyPath()
    {
        var items = Items();
        Assert.Equal(StagingAddResult.Duplicate, StagingPolicy.Add(items, "", 5));
        Assert.Equal(StagingAddResult.Duplicate, StagingPolicy.Add(items, "  ", 5));
        Assert.Empty(items);
    }

    [Fact]
    public void ClearUnmarkedKeepsMarkedEntries()
    {
        var items = Items(("C:\\a", null), ("C:\\b", "W"), ("C:\\c", null));
        Assert.Equal(2, StagingPolicy.ClearUnmarked(items));
        Assert.Equal(["C:\\b"], items.Select(i => i.Path).ToArray());
    }

    // ── StagingArea 薄壳：容量钳制 + Changed 只在真实变更时触发 ──────────

    [Fact]
    public void AreaClampsCapacityToAtLeastOne()
    {
        var area = new StagingArea { Capacity = 0 };
        var changes = 0;
        area.Changed += () => changes++;
        area.Restore([new StagingItem("C:\\a", null)]);
        Assert.Equal(StagingAddResult.Added, area.Add("C:\\b"));
        // Capacity=0 被钳为 1：加入 b 前先挤掉 a。
        Assert.Equal(["C:\\b"], area.Items.Select(i => i.Path).ToArray());
        // Duplicate 不触发 Changed（无落盘必要）。
        var before = changes;
        Assert.Equal(StagingAddResult.Duplicate, area.Add("C:\\b"));
        Assert.Equal(before, changes);
    }

    [Fact]
    public void RemoveAndClearUnmarkedFireChangedOnlyWhenSomethingChanged()
    {
        var area = new StagingArea { Capacity = 5 };
        area.Restore([new StagingItem("C:\\a", null), new StagingItem("C:\\b", "W")]);
        var changes = 0;
        area.Changed += () => changes++;

        area.Remove(new StagingItem("C:\\missing", null));
        Assert.Equal(0, changes); // 不存在的条目：静默

        area.Remove(new StagingItem("C:\\a", null));
        Assert.Equal(1, changes);

        Assert.Equal(0, area.ClearUnmarked()); // 只剩标记项
        Assert.Equal(1, changes);
    }
}
