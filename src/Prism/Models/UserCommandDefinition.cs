using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Prism.Models;

/// <summary>
/// K3 §4.1：用户命令持久化形态（前端→broker，对应 Rust <c>UserCommandDefinition</c>）。
/// 与下发型 <see cref="CommandDescriptor"/> 刻意分离——下发型不含 handler_params 全文。
/// 前端构造此对象经 CommandSet 发送给 broker。
/// </summary>
public sealed class UserCommandDefinition
{
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("title")]
    public string Title { get; set; } = "";

    [JsonPropertyName("subtitle")]
    public string Subtitle { get; set; } = "";

    [JsonPropertyName("icon_glyph")]
    public string IconGlyph { get; set; } = "";

    /// <summary>关键字：独占路由命名空间。≤4 个、各 ≤16 字符、无空白。</summary>
    [JsonPropertyName("keywords")]
    public List<string> Keywords { get; set; } = [];

    [JsonPropertyName("input")]
    public CommandInputSpecDto Input { get; set; } = new();

    [JsonPropertyName("bindings")]
    public CommandBindingsSpecDto Bindings { get; set; } = new();

    /// <summary>normal | elevated | destructive。用户命令禁止 elevated。</summary>
    [JsonPropertyName("danger")]
    public string Danger { get; set; } = "normal";

    [JsonPropertyName("enabled")]
    public bool Enabled { get; set; } = true;

    /// <summary>open_url | launch_program | unknown。未知 → broker 标记不可用。</summary>
    [JsonPropertyName("handler")]
    public string Handler { get; set; } = "open_url";

    /// <summary>handler 参数。open_url → url_template；launch_program → path + args_template + working_dir。</summary>
    [JsonPropertyName("handler_params")]
    public Dictionary<string, string> HandlerParams { get; set; } = [];
}

/// <summary>命令输入要求（对应 Rust CommandInputSpec）。</summary>
public sealed class CommandInputSpecDto
{
    [JsonPropertyName("kind")]
    public string Kind { get; set; } = "none";

    [JsonPropertyName("required")]
    public bool Required { get; set; }

    [JsonPropertyName("prompt")]
    public string Prompt { get; set; } = "";
}

/// <summary>各 surface 绑定集合（对应 Rust CommandBindings）。</summary>
public sealed class CommandBindingsSpecDto
{
    [JsonPropertyName("root_search")]
    public CommandBindingSpecDto? RootSearch { get; set; }

    [JsonPropertyName("keyword")]
    public CommandBindingSpecDto? Keyword { get; set; }

    [JsonPropertyName("action_panel")]
    public CommandBindingSpecDto? ActionPanel { get; set; }

    [JsonPropertyName("staging")]
    public CommandBindingSpecDto? Staging { get; set; }

    [JsonPropertyName("shortcut")]
    public CommandBindingSpecDto? Shortcut { get; set; }
}

/// <summary>单条 binding（对应 Rust CommandBinding）。</summary>
public sealed class CommandBindingSpecDto
{
    [JsonPropertyName("priority")]
    public int Priority { get; set; }

    [JsonPropertyName("shortcut_combo")]
    public string? ShortcutCombo { get; set; }

    [JsonPropertyName("trigger")]
    public string? Trigger { get; set; }

    [JsonPropertyName("show_in_root_search")]
    public bool ShowInRootSearch { get; set; } = true;
}

/// <summary>
/// K3 §4.6：命令预览结果（对应 Rust CommandPreviewResult）。
/// broker dry-run 返回执行时将用的最终字符串。
/// </summary>
public sealed record CommandPreviewResult(
    bool Ok,
    string Message,
    string? Url,
    string? ProgramPath,
    IReadOnlyList<string> ProgramArgs,
    string? ProgramWorkingDir)
{
    internal static CommandPreviewResult Parse(JsonElement el)
    {
        var ok = el.TryGetProperty("ok", out var okEl) && okEl.ValueKind == JsonValueKind.True;
        var msg = el.TryGetProperty("message", out var mEl) ? mEl.GetString() ?? "" : "";
        var url = el.TryGetProperty("url", out var uEl) && uEl.ValueKind == JsonValueKind.String
            ? uEl.GetString() : null;
        var path = el.TryGetProperty("program_path", out var pEl) && pEl.ValueKind == JsonValueKind.String
            ? pEl.GetString() : null;
        var workDir = el.TryGetProperty("program_working_dir", out var wEl) && wEl.ValueKind == JsonValueKind.String
            ? wEl.GetString() : null;
        var args = new List<string>();
        if (el.TryGetProperty("program_args", out var aEl) && aEl.ValueKind == JsonValueKind.Array)
            foreach (var a in aEl.EnumerateArray())
                if (a.ValueKind == JsonValueKind.String) args.Add(a.GetString() ?? "");
        return new CommandPreviewResult(ok, msg, url, path, args, workDir);
    }
}

/// <summary>
/// K3 §4.5：命名空间冲突校验结果（对应 Rust NamespaceValidation）。
/// ok=true 无冲突；ok=false 时 Conflict 携带来源信息。
/// </summary>
public sealed record NamespaceValidationResult(
    bool Ok,
    NamespaceConflictDto? Conflict)
{
    internal static NamespaceValidationResult Parse(JsonElement el)
    {
        var ok = el.TryGetProperty("ok", out var okEl) && okEl.ValueKind == JsonValueKind.True;
        NamespaceConflictDto? conflict = null;
        if (el.TryGetProperty("conflict", out var cEl) && cEl.ValueKind == JsonValueKind.Object)
        {
            var kind = cEl.TryGetProperty("kind", out var kEl) ? kEl.GetString() ?? "" : "";
            var label = cEl.TryGetProperty("owner_label", out var lEl) ? lEl.GetString() ?? "" : "";
            conflict = new NamespaceConflictDto(kind, label);
        }
        return new NamespaceValidationResult(ok, conflict);
    }
}

/// <summary>命名空间冲突来源信息。</summary>
public sealed record NamespaceConflictDto(string Kind, string OwnerLabel);
