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
        area.Restore([new StagingItem("C:\\a", null)], [], null);
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
        area.Restore([new StagingItem("C:\\a", null), new StagingItem("C:\\b", "W")], [], null);
        var changes = 0;
        area.Changed += () => changes++;

        area.Remove(new StagingItem("C:\\missing", null));
        Assert.Equal(0, changes); // 不存在的条目：静默

        area.Remove(new StagingItem("C:\\a", null));
        Assert.Equal(1, changes);

        Assert.Equal(0, area.ClearUnmarked()); // 只剩标记项
        Assert.Equal(1, changes);
    }

    // ── 工作集（阶段三）──────────────────────────────────────────────

    [Fact]
    public void SaveAsWorksetMarksAllAndUpsertsRecord()
    {
        var items = Items(("C:\\a", null), ("C:\\b", null));
        var worksets = new List<WorksetEntry> { new("旧名", null, ["C:\\z"]) };

        StagingPolicy.SaveAsWorkset(items, worksets, "8月报告", "还差一张图");
        // 全部条目改标，顺序保留。
        Assert.All(items, i => Assert.Equal("8月报告", i.Workset));
        Assert.Equal(["C:\\a", "C:\\b"], items.Select(i => i.Path).ToArray());
        // 记录 upsert（新名追加，旧记录不动）。
        var ws = worksets.Single(w => w.Name == "8月报告");
        Assert.Equal(["C:\\a", "C:\\b"], ws.Paths);
        Assert.Equal("还差一张图", ws.Note);
        Assert.Equal(2, worksets.Count);

        // 同名再存 = 覆盖（成员与备注替换）。
        StagingPolicy.SaveAsWorkset(items, worksets, "8月报告", null);
        Assert.Equal(["C:\\a", "C:\\b"], worksets.Single(w => w.Name == "8月报告").Paths);
        Assert.Null(worksets.Single(w => w.Name == "8月报告").Note);
    }

    [Fact]
    public void LoadWorksetRemovesOldMarksKeepsUnmarkedAndMarksInPlace()
    {
        var items = Items(
            ("C:\\old1", "8月报告"),
            ("C:\\keep", null),
            ("C:\\old2", "9月报告"),
            ("C:\\shared", null)); // 与新工作集同路径的未标记条目：就地改标
        var worksets = new List<WorksetEntry>
        {
            new("8月报告", null, ["C:\\old1"]),
            new("9月报告", null, ["C:\\shared", "C:\\new"]),
        };

        StagingPolicy.LoadWorkset(items, worksets, "9月报告");

        // 未标记保留 + 同路径改标不重复加 + 新路径接末尾。
        Assert.Equal(["C:\\keep", "C:\\shared", "C:\\new"], items.Select(i => i.Path).ToArray());
        Assert.Null(items[0].Workset);
        Assert.Equal("9月报告", items[1].Workset);
        Assert.Equal("9月报告", items[2].Workset);
        // 档案不动：8月报告记录原样。
        Assert.Equal(["C:\\old1"], worksets.Single(w => w.Name == "8月报告").Paths);
    }

    [Fact]
    public void DeleteWorksetKeepsItemsButUnmarks()
    {
        var items = Items(("C:\\a", "W"), ("C:\\b", null));
        var worksets = new List<WorksetEntry> { new("W", "note", ["C:\\a"]) };

        Assert.True(StagingPolicy.DeleteWorkset(items, worksets, "W"));
        Assert.Empty(worksets);
        Assert.Null(items[0].Workset); // 条目保留，改未标记
        Assert.Equal("C:\\b", items[1].Path);
        Assert.False(StagingPolicy.DeleteWorkset(items, worksets, "不存在"));
    }

    [Fact]
    public void ActiveWorksetAutoMembershipOnAddAndRemove()
    {
        var area = new StagingArea { Capacity = 5 };
        area.Restore([], [new WorksetEntry("W", null, ["C:\\init"])], "W");

        area.Add("C:\\new.txt");
        // 零摩擦加入：自动带标 + 落盘成员。
        Assert.Equal("W", Assert.Single(area.Items).Workset);
        Assert.Equal(["C:\\init", "C:\\new.txt"], area.Find("W")!.Paths);

        // 移除带标条目同步移除成员。
        area.Remove(area.Items[0]);
        Assert.Equal(["C:\\init"], area.Find("W")!.Paths);

        // 工作集被删后 active 失效：新加入不带标。
        area.Add("C:\\x");
        area.DeleteWorkset("W");
        area.Add("C:\\y");
        Assert.Null(area.Items.Last(i => i.Path == "C:\\y").Workset);
    }

    [Fact]
    public void RestoreDropsActiveWorksetReferenceWhenRecordMissing()
    {
        var area = new StagingArea();
        area.Restore([], [], "幽灵");
        Assert.Null(area.ActiveWorkset);
    }

    [Fact]
    public void WorksetPathsMayOverlapAcrossWorksets()
    {
        var items = Items();
        var worksets = new List<WorksetEntry>
        {
            new("A", null, ["C:\\same"]),
            new("B", null, ["C:\\same"]),
        };
        StagingPolicy.SaveAsWorkset(items, worksets, "C", null);
        // A/B 各自仍持 C:\\same——重叠合法。
        Assert.Contains("C:\\same", worksets.Single(w => w.Name == "A").Paths);
        Assert.Contains("C:\\same", worksets.Single(w => w.Name == "B").Paths);
    }
}
