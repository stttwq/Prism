using System.Text.Json;

namespace Prism.Models;

/// <summary>
/// K0 T9.1：命令目录条目（broker→前端，Serialize-only 对应类型）。
/// 容忍解析：未知 owner/danger/binding input 取值一律不抛异常，
/// 保留原始字符串并置 IsUsable=false。新 broker + 旧 WPF 是常态组合，
/// 严格解析会让一个新增 binding 类型把整份目录解析打死。
/// </summary>
public sealed record CommandDescriptor(
    string Id,
    string Title,
    string Subtitle,
    string IconGlyph,
    string Owner,
    string Trust,
    IReadOnlyList<string> Keywords,
    CommandInputDto Input,
    CommandBindingsDto Bindings,
    string Danger,
    bool Enabled,
    bool IsUsable)
{
    /// <summary>
    /// 从 JSON 解析命令描述。未知字段不抛；无法识别的 owner/danger/input.kind
    /// 置 IsUsable=false 并保留原始字符串。
    /// </summary>
    internal static CommandDescriptor? Parse(JsonElement el)
    {
        if (el.ValueKind != JsonValueKind.Object) return null;

        var id = el.TryGetProperty("id", out var idEl) ? idEl.GetString() ?? "" : "";
        var title = el.TryGetProperty("title", out var tEl) ? tEl.GetString() ?? "" : "";
        var subtitle = el.TryGetProperty("subtitle", out var sEl) ? sEl.GetString() ?? "" : "";
        var iconGlyph = el.TryGetProperty("icon_glyph", out var igEl) ? igEl.GetString() ?? "" : "";
        var owner = el.TryGetProperty("owner", out var oEl) ? oEl.GetString() ?? "" : "";
        var trust = el.TryGetProperty("trust", out var trEl) ? trEl.GetString() ?? "" : "";
        var danger = el.TryGetProperty("danger", out var dEl) ? dEl.GetString() ?? "" : "";
        var enabled = el.TryGetProperty("enabled", out var enEl) && enEl.ValueKind == JsonValueKind.True;

        var keywords = new List<string>();
        if (el.TryGetProperty("keywords", out var kwEl) && kwEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var kw in kwEl.EnumerateArray())
            {
                if (kw.ValueKind == JsonValueKind.String)
                    keywords.Add(kw.GetString() ?? "");
            }
        }

        var input = CommandInputDto.Parse(
            el.TryGetProperty("input", out var inEl) ? inEl : default);

        var bindings = CommandBindingsDto.Parse(
            el.TryGetProperty("bindings", out var bindEl) ? bindEl : default);

        // 可用性判定：owner/danger/input.kind 必须是已知值。
        var isUsable = IsKnownOwner(owner) && IsKnownDanger(danger) && input.IsUsable;

        return new CommandDescriptor(
            id, title, subtitle, iconGlyph, owner, trust,
            keywords, input, bindings, danger, enabled, isUsable);
    }

    private static bool IsKnownOwner(string owner) =>
        owner is "broker" or "ui";

    private static bool IsKnownDanger(string danger) =>
        danger is "normal" or "destructive" or "elevated";
}

/// <summary>命令输入类型（K0 设计 §5.1）。</summary>
public sealed record CommandInputDto(string Kind, bool Required, string Prompt)
{
    internal bool IsUsable { get; } = Kind is "none" or "text" or "destination" or "output_path";

    internal static CommandInputDto Parse(JsonElement el)
    {
        if (el.ValueKind != JsonValueKind.Object)
            return new CommandInputDto("none", false, "");

        var kind = el.TryGetProperty("kind", out var kEl) ? kEl.GetString() ?? "none" : "none";
        var required = el.TryGetProperty("required", out var rEl) && rEl.ValueKind == JsonValueKind.True;
        var prompt = el.TryGetProperty("prompt", out var pEl) ? pEl.GetString() ?? "" : "";
        return new CommandInputDto(kind, required, prompt);
    }
}

/// <summary>命令在各 UI 面的绑定（K0 全 null = 不可达）。</summary>
public sealed record CommandBindingsDto(
    CommandBindingDto? RootSearch,
    CommandBindingDto? Keyword,
    CommandBindingDto? ActionPanel,
    CommandBindingDto? Staging,
    CommandBindingDto? Shortcut)
{
    internal static CommandBindingsDto Parse(JsonElement el)
    {
        if (el.ValueKind != JsonValueKind.Object)
            return new CommandBindingsDto(null, null, null, null, null);

        return new CommandBindingsDto(
            ParseBinding(el, "root_search"),
            ParseBinding(el, "keyword"),
            ParseBinding(el, "action_panel"),
            ParseBinding(el, "staging"),
            ParseBinding(el, "shortcut"));
    }

    private static CommandBindingDto? ParseBinding(JsonElement parent, string name)
    {
        if (!parent.TryGetProperty(name, out var el) || el.ValueKind != JsonValueKind.Object)
            return null;
        var priority = el.TryGetProperty("priority", out var pEl) && pEl.TryGetInt32(out var pri)
            ? pri
            : 0;
        var input = el.TryGetProperty("input", out var iEl) ? iEl.GetString() ?? "" : "";
        var cardinality = el.TryGetProperty("cardinality", out var cEl) ? cEl.GetString() ?? null : null;
        var targetKinds = new List<string>();
        if (el.TryGetProperty("target_kinds", out var tkEl) && tkEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var tk in tkEl.EnumerateArray())
            {
                if (tk.ValueKind == JsonValueKind.String)
                    targetKinds.Add(tk.GetString() ?? "");
            }
        }
        var requiresHostRoot = el.TryGetProperty("requires_host_root", out var rhrEl)
            && rhrEl.ValueKind == JsonValueKind.True;
        return new CommandBindingDto(priority, input, cardinality, targetKinds, requiresHostRoot);
    }
}

/// <summary>
/// K2 §4.3：按 surface 扩展。Cardinality/TargetKinds/RequiresHostRoot 容忍缺字段
/// （旧 broker 的 binding 只有 priority + input）。
/// </summary>
public sealed record CommandBindingDto(
    int Priority,
    string Input,
    string? Cardinality,
    IReadOnlyList<string> TargetKinds,
    bool RequiresHostRoot)
{
    // 兼容旧调用点（K1 只传 priority + input）。
    public CommandBindingDto(int Priority, string Input)
        : this(Priority, Input, null, Array.Empty<string>(), false) { }
}
