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
    };

    /// <summary>预设引擎：必应优先，其次百度、Google（关键词 bi / b / g）。</summary>
    public static List<WebEngine> DefaultEngines() =>
    [
        new("bi", "Bing", "https://www.bing.com/search?q={q}"),
        new("b", "百度", "https://www.baidu.com/s?wd={q}"),
        new("g", "Google", "https://www.google.com/search?q={q}"),
    ];
}
