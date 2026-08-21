namespace Prism.Models;

/// <summary>呼出方式：双击 Ctrl 或自定义组合键。</summary>
public enum HotkeyMode
{
    /// <summary>默认：400ms 内双击 Ctrl 呼出。</summary>
    DoubleCtrl,

    /// <summary>自定义组合键（如 Alt+Space），走 RegisterHotKey。</summary>
    Combo,
}

/// <summary>一个网页快捷搜索引擎，如 g 关键词 = Google。</summary>
/// <param name="Keyword">触发关键词（如 "g"）。</param>
/// <param name="Name">显示名（如 "Google"）。</param>
/// <param name="UrlTemplate">URL 模板，用 {q} 占位查询词。</param>
public sealed record WebEngine(string Keyword, string Name, string UrlTemplate);

/// <summary>
/// favicon 联网许可（G8）。用户对某个 origin 授权后，favicon 缓存可下载该 origin 的图标。
/// </summary>
/// <param name="Origin">规范化 origin（scheme://host[:port]）。</param>
/// <param name="GrantedAt">授权时间（UTC ISO 8601），用于审计而非过期。</param>
public sealed record FaviconGrant(string Origin, string GrantedAt);

/// <summary>
/// 应用设置，前后端共享，序列化为 JSON。
/// 用 record + init 便于不可变复制；<see cref="Default"/> 提供全新用户的初始值。
/// </summary>
public sealed record Settings
{
    public const int CurrentSchemaVersion = 1;

    /// <summary>0 denotes a legacy file that predates explicit versioning.</summary>
    public int SchemaVersion { get; init; }

    /// <summary>呼出方式，默认双击 Ctrl。</summary>
    public HotkeyMode HotkeyMode { get; init; } = HotkeyMode.DoubleCtrl;

    /// <summary>Combo 模式下的组合键字符串，如 "Alt+Space"。DoubleCtrl 模式下忽略。</summary>
    public string ComboHotkey { get; init; } = "Alt+Space";

    /// <summary>是否开机自启。</summary>
    public bool AutoStart { get; init; } = false;

    /// <summary>网页快捷搜索引擎列表。</summary>
    public List<WebEngine> WebEngines { get; init; } = [];

    /// <summary>User-owned absolute directory exclusions sent as bounded search filters.</summary>
    public List<string> ExcludedPaths { get; init; } = [];

    public bool HistoryEnabled { get; init; } = true;

    public bool PinyinEnabled { get; init; } = true;

    /// <summary>当前目录搜索总开关（G4）：默认开启，关闭后呼出一律全局搜索。</summary>
    public bool CurrentDirectorySearchEnabled { get; init; } = true;

    /// <summary>
    /// Explorer Shell COM 宿主联动（G4）。默认关闭：兼容矩阵未全部通过前不发布该宿主支持。
    /// 旧 settings.json 缺字段时反序列化为 false。
    /// </summary>
    public bool ExplorerHostIntegrationEnabled { get; init; } = false;

    /// <summary>
    /// Directory Opus 13.23 官方外部命令联动（G4）。默认关闭，理由同 Explorer。
    /// </summary>
    public bool DirectoryOpusHostIntegrationEnabled { get; init; } = false;

    /// <summary>
    /// 自定义压缩程序可执行文件完整路径（G6 ZIP adapter）。设置且文件存在时优先于自动探测。
    /// 留空则自动探测本机 7-Zip，再回退到 Windows 内置 Shell 压缩。
    /// </summary>
    public string? ZipProgram { get; init; }

    /// <summary>
    /// 在线联想总开关（G8）。默认关闭：用户必须主动开启才发送联想网络请求。
    /// 只对内置 Bing/百度/Google 生效；自定义引擎不支持联想。
    /// </summary>
    public bool SuggestionsEnabled { get; init; } = false;

    /// <summary>
    /// 已授权获取 favicon 的 origin 集合（G8）。每个 origin 需用户明确同意才联网获取图标。
    /// 与 <see cref="SuggestionsEnabled"/> 相互独立。Key 为规范化 origin（scheme://host[:port]）。
    /// </summary>
    public Dictionary<string, FaviconGrant> FaviconGrants { get; init; } = [];

    /// <summary>
    /// 动作快捷键（2026-08-21 设想）：动作 id（broker 面板枚举）→ 窗口级组合键串
    /// （如 "Ctrl+Shift+C"）。空值/缺失 = 未绑定。旧设置文件缺字段时为空表。
    /// </summary>
    public Dictionary<string, string> ActionHotkeys { get; init; } = [];

    /// <summary>全新用户的默认设置：双击 Ctrl + 预设 bi/b/g 三个引擎（必应优先）。</summary>
    public static Settings Default => new()
    {
        SchemaVersion = CurrentSchemaVersion,
        HotkeyMode = HotkeyMode.DoubleCtrl,
        ComboHotkey = "Alt+Space",
        AutoStart = false,
        WebEngines = DefaultEngines(),
        ExcludedPaths = [],
        HistoryEnabled = true,
        PinyinEnabled = true,
        CurrentDirectorySearchEnabled = true,
        ExplorerHostIntegrationEnabled = false,
        DirectoryOpusHostIntegrationEnabled = false,
        ZipProgram = null,
        SuggestionsEnabled = false,
        FaviconGrants = [],
        ActionHotkeys = [],
    };

    /// <summary>预设引擎：必应优先，其次百度、Google（关键词 bi / b / g）。</summary>
    public static List<WebEngine> DefaultEngines() =>
    [
        new("bi", "Bing", "https://www.bing.com/search?q={q}"),
        new("b", "百度", "https://www.baidu.com/s?wd={q}"),
        new("g", "Google", "https://www.google.com/search?q={q}"),
    ];
}
