namespace Prism.Models;

public enum SearchResultKind
{
    Unknown,
    App,
    File,
    Folder,
    Web,
    /// <summary>可切换的顶层窗口（G5）。</summary>
    Window,
    /// <summary>K0：命令行（统一命令系统）。K0 无生产者，K1 接入。</summary>
    Command,
    More,
    /// <summary>K3 §4.9：无结果兜底行（本地生成，不进 complete-cache）。</summary>
    Fallback,
}

public sealed record SearchMatchMetadata(int Class, int Position, int Score);

public sealed record ActionTarget(string Kind, string Value)
{
    public static ActionTarget FromLegacy(string resultKind, string executeId) => new(
        resultKind switch
        {
            "app" => "application",
            "folder" => "directory",
            "web" => "web",
            // Window results always arrive with a typed target; this keeps the legacy
            // fallback from mis-labelling an enumeration token as a file path.
            "window" => "window",
            // K0 T10.2：命令行映射成 command 而非 file——现状 _ => "file" 会让命令 id
            // 退化成以 id 为路径的 file target，走 broker Shell 链路只剩绝对路径检查兜底。
            // 映射成 command 后，broker 每条路径都显式拒绝（T2 守卫）。
            "command" => "command",
            _ => "file",
        },
        executeId);
}

/// <summary>单条搜索结果（frontend-spec.md §2）。</summary>
/// <param name="Kind">"app" | "file" | "folder" | "web" | "more"</param>
/// <param name="Title">主标题（文件名）。</param>
/// <param name="Subtitle">副标题（完整路径等）。</param>
/// <param name="ExecuteId">回传后端 execute/reveal 的标识（当前为完整路径）。</param>
/// <param name="MatchSpans">标题中要染蓝的 UTF-16 区间 [start,len,...]。</param>
public sealed record SearchResult(
    string Kind,
    string Title,
    string Subtitle,
    string ExecuteId,
    int[] MatchSpans)
{
    public SearchMatchMetadata? MatchMetadata { get; init; }
    public ActionTarget? Target { get; init; }

    /// <summary>
    /// 图标来源 URL（可选）。仅 fallback 行使用：ResultList 据此取默认引擎图标
    /// （内置引擎名推断 / favicon / 通用 W），ExecuteId 是查询词不是 URL。
    /// </summary>
    public string? IconUrl { get; init; }

    /// <summary>
    /// K4a：仅「无结果回退命令」合成行设置——Enter 执行时该值作为 arguments.text
    /// 传入（命令拿到 {query}）。wire 解析不填此字段，仅前端合成行使用。
    /// </summary>
    public string? FallbackQuery { get; init; }

    /// <summary>
    /// 行身份键（可选）。ResultList 用它决定"同一行更新"还是"删旧插新"。
    /// 网页模式的行内容每次按键都变（URL 与标题都含查询词），若按内容比对身份，
    /// 行会被反复删除重建，容器重建瞬间图标为空——表现为每敲一个字母图标闪一下。
    /// 这类行显式给一个与查询无关的稳定键，让容器原地更新。
    /// </summary>
    public string? RowKey { get; init; }

    /// <summary>容器复用比对用的键。未显式指定 RowKey 时退回按内容比对。
    /// AUDIT-4 A3（2026-08-21）：惰性缓存——record 不可变，同实例恒同键；
    /// 此前每次访问都重新拼接分配，展开 1000 行态每击键 SynchronizeDisplayItems
    /// 要比对 ~2000 次。</summary>
    public string ContainerKey => _containerKey ??= RowKey ?? $"{Kind}\u001f{ExecuteId}\u001f{Title}";

    private string? _containerKey;

    public ActionTarget ExecutionTarget => Target ?? ActionTarget.FromLegacy(Kind, ExecuteId);

    public SearchResultKind ResultKind => Kind switch
    {
        "app" => SearchResultKind.App,
        "file" => SearchResultKind.File,
        "folder" => SearchResultKind.Folder,
        "web" => SearchResultKind.Web,
        "window" => SearchResultKind.Window,
        "command" => SearchResultKind.Command,
        "more" => SearchResultKind.More,
        _ => SearchResultKind.Unknown,
    };

    /// <summary>前端追加的"展示更多"行。</summary>
    public static SearchResult More(string query) => new(
        Kind: "more",
        Title: $"展示更多 '{query}' 的文件搜索结果",
        Subtitle: "热键: 双击 Ctrl",
        ExecuteId: "",
        MatchSpans: []);

    /// <summary>K3 §4.9：无结果兜底行（本地生成）。engineUrl 供图标装饰取默认引擎图标。</summary>
    public static SearchResult Fallback(string query, string? engineUrl = null) => new(
        Kind: "fallback",
        Title: $"用默认引擎搜索「{query}」",
        Subtitle: "Enter 用浏览器搜索此词",
        ExecuteId: query,
        MatchSpans: [])
    {
        RowKey = "fallback:" + query,
        IconUrl = engineUrl,
    };

    /// <summary>
    /// K4a：无结果回退命令行（本地合成，broker 无感知）。FallbackQuery 使 Enter
    /// 执行时把查询词作为 arguments.text 传入命令（{query} 展开源）。
    /// </summary>
    public static SearchResult FallbackCommand(CommandDescriptor desc, string query) => new(
        Kind: "command",
        Title: desc.Title,
        Subtitle: string.IsNullOrEmpty(desc.Subtitle) ? "无结果时回退执行" : desc.Subtitle,
        ExecuteId: desc.Id,
        MatchSpans: [])
    {
        RowKey = "fbcmd:" + desc.Id + ":" + query,
        FallbackQuery = query,
    };

    /// <summary>
    /// 进结果列表前的双重去重（2026-08-25 用户报告修复）：
    /// 1) 按 ContainerKey 去重——ResultList 的同步算法（FindBy 只查 i 之后）假定
    ///    键唯一，同键两行会把上一轮已显示的同键实例再次插入，ListBox 出现同一
    ///    实例两份；WPF Selector 的选中簿记以 Object.Equals 键控 ItemInfo 字典，
    ///    重复实例让 Dictionary.Add 抛 "An item with the same key has already been
    ///    added. Key: …ItemInfo"，选中存储从此带毒，此后每次按键都报"搜索失败"
    ///    直至重启。VM 组装 list 时先去重（保证 State.Results 与显示集合同源，
    ///    选中/执行索引一致），ResultList 再兜一层防御。
    /// 2) 按值去重（record 相等性）——等值记录必然同键（相等性含 RowKey），与键
    ///    去重语义重叠，纯防御：防将来 ContainerKey 取键逻辑与相等性脱钩。
    /// 同键/等值行对用户是同一行，丢弃无信息损失。
    /// </summary>
    public static List<SearchResult> DeduplicateRows(IReadOnlyList<SearchResult> rows)
    {
        if (rows.Count <= 1) return new List<SearchResult>(rows);

        var keys = new HashSet<string>(StringComparer.Ordinal);
        var values = new HashSet<SearchResult>();
        var unique = new List<SearchResult>(rows.Count);
        foreach (var row in rows)
        {
            if (!keys.Add(row.ContainerKey) || !values.Add(row))
                continue;
            unique.Add(row);
        }
        return unique;
    }
}
