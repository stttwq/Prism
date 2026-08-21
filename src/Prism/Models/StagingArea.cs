namespace Prism.Models;

/// <summary>暂存区条目（2026-08-22 暂存区计划）：路径引用 + 工作集归属标记。
/// Workset 非空 = 属于该工作集（角标为真，LRU 挤不掉）；null = 临时文件。</summary>
public sealed record StagingItem(string Path, string? Workset);

/// <summary>
/// 工作集记录（2026-08-22 计划阶段三）：名字 → 路径列表 + 备注。
/// 备注写文件清单说不出来的东西（状态/进度/上下文）；路径可与其他工作集重叠
/// （工作集只是"这些文件是一组"的记录，文件不因归组而排他）。
/// </summary>
public sealed record WorksetEntry(string Name, string? Note, IReadOnlyList<string> Paths);

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
/// 工作集操作（阶段三）：载入保留规则 = 移除全部标记引用（档案不动）+ 保留
/// 未标记 + 工作集路径加入并标记（同路径未标记条目就地改标，不重复加）。
/// </summary>
public static class StagingPolicy
{
    public static StagingAddResult Add(
        List<StagingItem> items, string path, int capacity, string? mark = null)
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
        items.Add(new StagingItem(path, mark));
        return StagingAddResult.Added;
    }

    /// <summary>清空未标记项，返回清除数。工作集标记项不清（已归档）。</summary>
    public static int ClearUnmarked(List<StagingItem> items) =>
        items.RemoveAll(i => i.Workset is null);

    // ── 工作集（阶段三）────────────────────────────────────────────

    private static int FindIndexByName(IReadOnlyList<WorksetEntry> worksets, string name)
    {
        for (var i = 0; i < worksets.Count; i++)
            if (string.Equals(worksets[i].Name, name, StringComparison.OrdinalIgnoreCase))
                return i;
        return -1;
    }

    /// <summary>存为工作集：当前全部条目（保序）upsert 进表，全部条目改标该名。
    /// 同名覆盖（确认由 UI 层做）；工作集间路径可重叠，互不影响。</summary>
    public static void SaveAsWorkset(
        List<StagingItem> items, List<WorksetEntry> worksets, string name, string? note)
    {
        var paths = items.Select(i => i.Path).ToList();
        var idx = FindIndexByName(worksets, name);
        if (idx >= 0)
            worksets[idx] = worksets[idx] with { Note = note, Paths = paths };
        else
            worksets.Add(new WorksetEntry(name, note, paths));
        for (var i = 0; i < items.Count; i++)
            items[i] = items[i] with { Workset = name };
    }

    /// <summary>载入工作集：移除全部标记引用（档案不动）、未标记保留、
    /// 工作集路径逐个标记加入；同名路径就地改标不重复加。容量不参与
    /// （归档整组载入 + 未标记"不能丢"都优先于容量上限）。</summary>
    public static void LoadWorkset(
        List<StagingItem> items, IReadOnlyList<WorksetEntry> worksets, string name)
    {
        var idx = FindIndexByName(worksets, name);
        if (idx < 0) return;
        var paths = worksets[idx].Paths;

        items.RemoveAll(i => i.Workset is not null);
        foreach (var path in paths)
        {
            var existing = -1;
            for (var i = 0; i < items.Count; i++)
                if (string.Equals(items[i].Path, path, StringComparison.OrdinalIgnoreCase))
                {
                    existing = i;
                    break;
                }
            if (existing >= 0)
                items[existing] = items[existing] with { Workset = name };
            else
                items.Add(new StagingItem(path, name));
        }
    }

    /// <summary>删除工作集：删表项；对应标记条目改为未标记（条目不删——
    /// 删不删由用户「清」决定，最小惊扰）。</summary>
    public static bool DeleteWorkset(List<StagingItem> items, List<WorksetEntry> worksets, string name)
    {
        var idx = FindIndexByName(worksets, name);
        if (idx < 0) return false;
        worksets.RemoveAt(idx);
        for (var i = 0; i < items.Count; i++)
            if (string.Equals(items[i].Workset, name, StringComparison.OrdinalIgnoreCase))
                items[i] = items[i] with { Workset = null };
        return true;
    }

    /// <summary>活跃工作集自动落盘成员（零摩擦加入的延伸）：追加路径。</summary>
    public static void AppendMember(List<WorksetEntry> worksets, string name, string path)
    {
        var idx = FindIndexByName(worksets, name);
        if (idx < 0) return;
        var ws = worksets[idx];
        if (ws.Paths.Any(p => string.Equals(p, path, StringComparison.OrdinalIgnoreCase)))
            return;
        worksets[idx] = ws with { Paths = [.. ws.Paths, path] };
    }

    /// <summary>移除带标条目时同步移除工作集成员（工作集不锁死，加减更新落盘）。</summary>
    public static void RemoveMember(List<WorksetEntry> worksets, string name, string path)
    {
        var idx = FindIndexByName(worksets, name);
        if (idx < 0) return;
        var ws = worksets[idx];
        var remaining = ws.Paths
            .Where(p => !string.Equals(p, path, StringComparison.OrdinalIgnoreCase))
            .ToList();
        if (remaining.Count == ws.Paths.Count) return;
        worksets[idx] = ws with { Paths = remaining };
    }

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
    private readonly List<WorksetEntry> _worksets = [];
    private string? _activeWorkset;

    /// <summary>容量上限，App 从设置注入并在保存后热更新。仅约束未标记加入。</summary>
    public int Capacity { get; set; } = 5;

    public IReadOnlyList<StagingItem> Items => _items;
    public int Count => _items.Count;
    public IReadOnlyList<WorksetEntry> Worksets => _worksets;

    /// <summary>当前活跃工作集名（最近载入/存为的）；之后加入的文件自动带标
    /// 并落盘为该工作集成员（零摩擦加入的延伸）。无活跃工作集时为 null。</summary>
    public string? ActiveWorkset => _activeWorkset;

    /// <summary>任何变更（加入/移除/清空/载入/存为/删除）后触发一次；订阅方负责落盘。</summary>
    public event Action? Changed;

    /// <summary>启动恢复（StagingStore.Load 的结果）；不合法条目由 store 侧过滤。
    /// active 引用已删除的工作集时置空。</summary>
    public void Restore(IEnumerable<StagingItem> items, IReadOnlyList<WorksetEntry> worksets, string? active)
    {
        _items.Clear();
        _items.AddRange(items);
        _worksets.Clear();
        _worksets.AddRange(worksets);
        _activeWorkset = Find(active) is not null ? active : null;
        Changed?.Invoke();
    }

    public WorksetEntry? Find(string? name)
    {
        if (name is null) return null;
        foreach (var ws in _worksets)
            if (string.Equals(ws.Name, name, StringComparison.OrdinalIgnoreCase))
                return ws;
        return null;
    }

    public StagingAddResult Add(string path)
    {
        // 活跃工作集存在才自动带标（且该工作集记录仍存在——被删后不复活）。
        var mark = Find(_activeWorkset) is not null ? _activeWorkset : null;
        var result = StagingPolicy.Add(_items, path, Math.Max(1, Capacity), mark);
        if (result == StagingAddResult.Added)
        {
            if (mark is not null)
                StagingPolicy.AppendMember(_worksets, mark, path);
            Changed?.Invoke();
        }
        return result;
    }

    /// <summary>移除指定条目（角标 ×）；带标条目同步从工作集记录移除成员。
    /// 越界/不存在静默。</summary>
    public void Remove(StagingItem item)
    {
        if (!_items.Remove(item)) return;
        if (item.Workset is not null)
            StagingPolicy.RemoveMember(_worksets, item.Workset, item.Path);
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

    /// <summary>存为工作集：当前全部条目转正（改标 + upsert 落盘记录），成为活跃。
    /// 同名覆盖由 UI 层先行确认。</summary>
    public void SaveAsWorkset(string name, string? note)
    {
        StagingPolicy.SaveAsWorkset(_items, _worksets, name, note);
        _activeWorkset = name;
        Changed?.Invoke();
    }

    /// <summary>载入工作集（档案不动）。名字不存在返回 false。</summary>
    public bool LoadWorkset(string name)
    {
        if (Find(name) is null) return false;
        StagingPolicy.LoadWorkset(_items, _worksets, name);
        _activeWorkset = name;
        Changed?.Invoke();
        return true;
    }

    /// <summary>删除工作集记录；对应标记条目改为未标记（不删条目）。</summary>
    public bool DeleteWorkset(string name)
    {
        if (!StagingPolicy.DeleteWorkset(_items, _worksets, name)) return false;
        if (string.Equals(_activeWorkset, name, StringComparison.OrdinalIgnoreCase))
            _activeWorkset = null;
        Changed?.Invoke();
        return true;
    }
}
