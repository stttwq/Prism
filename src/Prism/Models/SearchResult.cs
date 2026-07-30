namespace Prism.Models;

public enum SearchResultKind
{
    Unknown,
    App,
    File,
    Folder,
    Web,
    More,
}

public sealed record SearchMatchMetadata(int Class, int Position, int Score);

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

    public SearchResultKind ResultKind => Kind switch
    {
        "app" => SearchResultKind.App,
        "file" => SearchResultKind.File,
        "folder" => SearchResultKind.Folder,
        "web" => SearchResultKind.Web,
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
