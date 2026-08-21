namespace Prism.Models;

/// <summary>暂存区条目（2026-08-22 暂存区计划）：路径引用 + 工作集归属标记。
/// Workset 非空 = 属于该工作集（角标为真，LRU 挤不掉）；null = 临时文件。</summary>
public sealed record StagingItem(string Path, string? Workset);

/// <summary>加入暂存区的结果。</summary>
public enum StagingAddResult
{
    Added,
    /// <summary>同路径（忽略大小写）已在暂存区。</summary>
    Duplicate,
    /// <summary>容量满且全部是工作集标记项，不可挤。</summary>
    Full,
}

/// <summary>
/// 暂存区纯策略（单测锚）：只操作内存列表，不碰 I/O 与 UI。
/// 不变量：条目顺序 = 扔进去的先后（新文件永远在末尾，不分组不重排）；
/// 容量满时挤掉最早的未标记项；标记项（已归档进工作集）永不挤。
/// </summary>
public static class StagingPolicy
{
    public static StagingAddResult Add(List<StagingItem> items, string path, int capacity)
    {
        if (string.IsNullOrWhiteSpace(path)) return StagingAddResult.Duplicate;
        if (items.Any(i => string.Equals(i.Path, path, StringComparison.OrdinalIgnoreCase)))
            return StagingAddResult.Duplicate;

        while (items.Count >= capacity)
        {
            var victim = IndexOfEarliestUnmarked(items);
            if (victim < 0) return StagingAddResult.Full;
            items.RemoveAt(victim);
        }
        items.Add(new StagingItem(path, null));
        return StagingAddResult.Added;
    }

    /// <summary>清空未标记项，返回清除数。工作集标记项不清（已归档）。</summary>
    public static int ClearUnmarked(List<StagingItem> items) =>
        items.RemoveAll(i => i.Workset is null);

    private static int IndexOfEarliestUnmarked(List<StagingItem> items)
    {
        for (var i = 0; i < items.Count; i++)
            if (items[i].Workset is null)
                return i;
        return -1;
    }
}

/// <summary>
/// 暂存区状态（唯一实例，App 持有）。独立于 <see cref="AppState"/>——后者在每次
/// 窗口隐藏（ReleaseIdleMemory / ResetForShow）时清空 Results，暂存区必须活过
/// 呼出周期。全部成员只在 UI 线程调用。
/// </summary>
public sealed class StagingArea
{
    private readonly List<StagingItem> _items = [];

    /// <summary>容量上限，App 从设置注入并在保存后热更新。仅约束未标记加入。</summary>
    public int Capacity { get; set; } = 5;

    public IReadOnlyList<StagingItem> Items => _items;
    public int Count => _items.Count;

    /// <summary>任何变更（加入/移除/清空/载入）后触发一次；订阅方负责落盘。</summary>
    public event Action? Changed;

    /// <summary>启动恢复（StagingStore.Load 的结果）；不合法条目由 store 侧过滤。</summary>
    public void Restore(IEnumerable<StagingItem> items)
    {
        _items.Clear();
        _items.AddRange(items);
        Changed?.Invoke();
    }

    public StagingAddResult Add(string path)
    {
        var result = StagingPolicy.Add(_items, path, Math.Max(1, Capacity));
        if (result == StagingAddResult.Added)
            Changed?.Invoke();
        return result;
    }

    /// <summary>移除指定条目（角标 ×）；越界静默。</summary>
    public void Remove(StagingItem item)
    {
        if (_items.Remove(item))
            Changed?.Invoke();
    }

    /// <summary>清空未标记项，返回清除数。</summary>
    public int ClearUnmarked()
    {
        var removed = StagingPolicy.ClearUnmarked(_items);
        if (removed > 0)
            Changed?.Invoke();
        return removed;
    }
}
