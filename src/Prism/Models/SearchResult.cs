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
    More,
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
    /// 行身份键（可选）。ResultList 用它决定"同一行更新"还是"删旧插新"。
    /// 网页模式的行内容每次按键都变（URL 与标题都含查询词），若按内容比对身份，
    /// 行会被反复删除重建，容器重建瞬间图标为空——表现为每敲一个字母图标闪一下。
    /// 这类行显式给一个与查询无关的稳定键，让容器原地更新。
    /// </summary>
    public string? RowKey { get; init; }

    /// <summary>容器复用比对用的键。未显式指定 RowKey 时退回按内容比对。</summary>
    public string ContainerKey => RowKey ?? $"{Kind}\u001f{ExecuteId}\u001f{Title}";

    public ActionTarget ExecutionTarget => Target ?? ActionTarget.FromLegacy(Kind, ExecuteId);

    public SearchResultKind ResultKind => Kind switch
    {
        "app" => SearchResultKind.App,
        "file" => SearchResultKind.File,
        "folder" => SearchResultKind.Folder,
        "web" => SearchResultKind.Web,
        "window" => SearchResultKind.Window,
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
}
