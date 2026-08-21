using System.Text;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>staging.json 持久化锚（2026-08-22 暂存区计划阶段二）。</summary>
public sealed class StagingStoreTests : IDisposable
{
    private readonly string _dir =
        Path.Combine(Path.GetTempPath(), "prism-staging-tests-" + Guid.NewGuid().ToString("N"));

    private StagingStore CreateStore() => new(_dir);

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { /* ignore */ }
    }

    [Fact]
    public void MissingFileLoadsEmpty()
    {
        var file = CreateStore().Load();
        Assert.Empty(file.Items);
        Assert.Empty(file.Worksets);
        Assert.Null(file.ActiveWorkset);
    }

    [Fact]
    public void RoundTripPreservesOrderMarksAndWorksets()
    {
        var store = CreateStore();
        var items = new List<StagingItem>
        {
            new("C:\\a.txt", null),
            new("C:\\子目录\\报告.docx", "8月报告"),
        };
        var worksets = new List<WorksetEntry> { new("8月报告", "还差一张图", ["C:\\子目录\\报告.docx"]) };
        store.Save(items, worksets, "8月报告");

        var loaded = store.Load();
        Assert.Equal(2, loaded.Items.Count);
        Assert.Equal("C:\\a.txt", loaded.Items[0].Path);
        Assert.Null(loaded.Items[0].Workset);
        Assert.Equal("8月报告", loaded.Items[1].Workset);
        // 中文不转义为 \uXXXX，文件可读。
        Assert.Contains("报告.docx", File.ReadAllText(
            Path.Combine(_dir, "staging.json"), Encoding.UTF8));
        var ws = Assert.Single(loaded.Worksets);
        Assert.Equal("8月报告", ws.Name);
        Assert.Equal("还差一张图", ws.Note);
        Assert.Equal("C:\\子目录\\报告.docx", Assert.Single(ws.Paths));
        Assert.Equal("8月报告", loaded.ActiveWorkset);
    }

    [Fact]
    public void CorruptFileFallsBackToEmpty()
    {
        Directory.CreateDirectory(_dir);
        File.WriteAllText(Path.Combine(_dir, "staging.json"), "{ not json !!!");
        Assert.Empty(CreateStore().Load().Items);
    }

    [Fact]
    public void LoadDropsInvalidEntriesAndBoundsLists()
    {
        Directory.CreateDirectory(_dir);
        var json = """
        {
          "Items": [
            { "Path": "", "Workset": null },
            { "Path": "  C:\\ok.txt  ", "Workset": "  W  " }
          ],
          "Worksets": [
            { "Name": "", "Note": null, "Paths": [] },
            { "Name": "W", "Note": null, "Paths": ["C:\\ok.txt", ""] }
          ],
          "ActiveWorkset": " W "
        }
        """;
        File.WriteAllText(Path.Combine(_dir, "staging.json"), json, Encoding.UTF8);

        var file = CreateStore().Load();
        var item = Assert.Single(file.Items);
        Assert.Equal("C:\\ok.txt", item.Path); // 空路径丢弃、合法路径去空白
        Assert.Equal("W", item.Workset);       // 标记名去空白
        var ws = Assert.Single(file.Worksets); // 空名工作集丢弃
        Assert.Equal("W", ws.Name);
        Assert.Equal(["C:\\ok.txt"], ws.Paths); // 空路径成员丢弃
        Assert.Equal("W", file.ActiveWorkset);
    }

    [Fact]
    public void SaveIsAtomicViaTempFileOverwrite()
    {
        var store = CreateStore();
        store.Save([new StagingItem("C:\\a", null)], [], null);
        var firstWrite = File.GetLastWriteTimeUtc(Path.Combine(_dir, "staging.json"));
        Thread.Sleep(20);
        store.Save([new StagingItem("C:\\b", null)], [], null);
        // 直接覆盖第二次写入成功，无 .tmp 残留。
        Assert.False(File.Exists(Path.Combine(_dir, "staging.json.tmp")));
        var loaded = store.Load();
        Assert.Equal("C:\\b", Assert.Single(loaded.Items).Path);
        Assert.True(File.GetLastWriteTimeUtc(Path.Combine(_dir, "staging.json")) >= firstWrite);
    }
}
