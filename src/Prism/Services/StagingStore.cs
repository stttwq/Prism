using System.IO;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 暂存区持久化：独立 staging.json，**不进 settings.json**——设置页是
/// "载入快照、保存全量写盘"，搜索窗侧写入的暂存条目会被设置页保存整体覆盖。
/// 原子写（.tmp + File.Move overwrite）与损坏回落空态沿用 SettingsStore 纪律。
/// </summary>
public sealed class StagingStore
{
    private const string FileName = "staging.json";
    // 防御上限：容量设置界是 32，但工作集标记项允许临时超容量，载入给更宽的界。
    private const int MaxItems = 64;
    private const int MaxWorksets = 32;
    private const int MaxPathsPerWorkset = 128;
    private const int MaxPathLength = 1024;
    private const int MaxNameLength = 64;
    private const int MaxNoteLength = 256;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        Converters = { new JsonStringEnumConverter() },
    };

    /// <summary>ActiveWorkset 已废弃（暂存区/工作集分离）：字段仅为旧文件反序列化保留，
    /// 载入时忽略，保存恒写 null。</summary>
    public sealed record StagingFile(
        List<StagingItem> Items,
        List<WorksetEntry> Worksets,
        string? ActiveWorkset = null);

    private readonly string _path;

    public StagingStore(string dataDir)
    {
        Directory.CreateDirectory(dataDir);
        _path = Path.Combine(dataDir, FileName);
    }

    /// <summary>损坏/缺失返回空态（不抛出）。</summary>
    public StagingFile Load()
    {
        try
        {
            if (!File.Exists(_path))
                return new StagingFile([], [], null);
            var json = File.ReadAllText(_path, Encoding.UTF8);
            var file = JsonSerializer.Deserialize<StagingFile>(json, JsonOptions);
            if (file is null)
                return new StagingFile([], [], null);
            return new StagingFile(
                SanitizeItems(file.Items),
                SanitizeWorksets(file.Worksets));
        }
        catch
        {
            return new StagingFile([], [], null);
        }
    }

    public void Save(IReadOnlyList<StagingItem> items, IReadOnlyList<WorksetEntry> worksets)
    {
        var tmp = _path + ".tmp";
        var file = new StagingFile([.. items], [.. worksets], null);
        var json = JsonSerializer.Serialize(file, JsonOptions);
        File.WriteAllText(tmp, json, Encoding.UTF8);
        File.Move(tmp, _path, overwrite: true);
    }

    private static List<StagingItem> SanitizeItems(List<StagingItem>? items)
    {
        if (items is null) return [];
        var clean = new List<StagingItem>(Math.Min(items.Count, MaxItems));
        foreach (var item in items)
        {
            if (clean.Count >= MaxItems) break;
            if (string.IsNullOrWhiteSpace(item.Path)
                || item.Path.Length > MaxPathLength
                || item.Path.Any(char.IsControl))
                continue;
            clean.Add(new StagingItem(item.Path.Trim(), SanitizeName(item.Workset, MaxNameLength)));
        }
        return clean;
    }

    private static List<WorksetEntry> SanitizeWorksets(List<WorksetEntry>? worksets)
    {
        if (worksets is null) return [];
        var clean = new List<WorksetEntry>(Math.Min(worksets.Count, MaxWorksets));
        foreach (var ws in worksets)
        {
            if (clean.Count >= MaxWorksets) break;
            var name = SanitizeName(ws.Name, MaxNameLength);
            if (name is null) continue;
            var paths = ws.Paths?
                .Where(p => !string.IsNullOrWhiteSpace(p) && p.Length <= MaxPathLength && !p.Any(char.IsControl))
                .Select(p => p.Trim())
                .Take(MaxPathsPerWorkset)
                .ToList() ?? [];
            clean.Add(new WorksetEntry(name, SanitizeName(ws.Note, MaxNoteLength), paths));
        }
        return clean;
    }

    /// <summary>名称裁剪：空/含控制符 → null（null 对 note/active 合法，对 workset 名表示丢弃条目）。</summary>
    private static string? SanitizeName(string? raw, int max)
    {
        if (string.IsNullOrWhiteSpace(raw)) return null;
        var trimmed = raw.Trim();
        if (trimmed.Any(char.IsControl)) return null;
        return trimmed.Length > max ? trimmed[..max] : trimmed;
    }
}
