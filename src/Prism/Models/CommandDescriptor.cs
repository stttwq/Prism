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
    bool IsUsable,
    bool Fallback = false,
    IReadOnlyList<CommandArgumentSpecDto>? Arguments = null,
    string Handler = "",
    IReadOnlyDictionary<string, string>? HandlerParams = null)
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

        // K4a：无结果回退标记（缺省 false，旧 broker 无此字段）。
        var fallback = el.TryGetProperty("fallback", out var fbEl) && fbEl.ValueKind == JsonValueKind.True;

        // K4b：声明式参数（缺省空表，旧 broker 无此字段）。
        var arguments = new List<CommandArgumentSpecDto>();
        if (el.TryGetProperty("arguments", out var argsEl) && argsEl.ValueKind == JsonValueKind.Array)
        {
            foreach (var a in argsEl.EnumerateArray())
            {
                if (a.ValueKind != JsonValueKind.Object) continue;
                arguments.Add(new CommandArgumentSpecDto
                {
                    Name = a.TryGetProperty("name", out var nEl) ? nEl.GetString() ?? "" : "",
                    Required = a.TryGetProperty("required", out var rEl) && rEl.ValueKind == JsonValueKind.True,
                    Default = a.TryGetProperty("default", out var defEl) ? defEl.GetString() ?? "" : "",
                });
            }
        }

        // K4b 收尾：handler 与参数（缺省空，旧 broker 无此字段）。
        var handler = el.TryGetProperty("handler", out var hEl) ? hEl.GetString() ?? "" : "";
        var handlerParams = new Dictionary<string, string>();
        if (el.TryGetProperty("handler_params", out var hpEl) && hpEl.ValueKind == JsonValueKind.Object)
        {
            foreach (var kv in hpEl.EnumerateObject())
            {
                if (kv.Value.ValueKind == JsonValueKind.String)
                    handlerParams[kv.Name] = kv.Value.GetString() ?? "";
            }
        }

        return new CommandDescriptor(
            id, title, subtitle, iconGlyph, owner, trust,
            keywords, input, bindings, danger, enabled, isUsable, fallback, arguments,
            handler, handlerParams);
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
        // K2 §4.6：仅 shortcut binding 携带。组合键原始字符串（如 "Ctrl+Shift+S"）。
        var shortcutCombo = el.TryGetProperty("shortcut_combo", out var scEl)
            && scEl.ValueKind == JsonValueKind.String
            ? scEl.GetString()
            : null;
        // K3 §4.4：仅 keyword binding 携带。关键字路由的触发词。
        var trigger = el.TryGetProperty("trigger", out var trEl)
            && trEl.ValueKind == JsonValueKind.String
            ? trEl.GetString()
            : null;
        // K3 §4.4：仅 root_search binding 携带。默认 true（缺省时视为可见）。
        var showInRootSearch = !el.TryGetProperty("show_in_root_search", out var srsEl)
            || srsEl.ValueKind != JsonValueKind.False;
        return new CommandBindingDto(
            priority, input, cardinality, targetKinds, requiresHostRoot, shortcutCombo,
            trigger, showInRootSearch);
    }
}

/// <summary>
/// K2 §4.3：按 surface 扩展。Cardinality/TargetKinds/RequiresHostRoot 容忍缺字段
/// （旧 broker 的 binding 只有 priority + input）。
/// K2 §4.6：ShortcutCombo 仅 shortcut binding 携带（组合键原始字符串）。
/// K3 §4.4：Trigger 仅 keyword binding 携带（路由触发词）；
/// ShowInRootSearch 仅 root_search binding 携带（默认 true）。
/// </summary>
public sealed record CommandBindingDto(
    int Priority,
    string Input,
    string? Cardinality,
    IReadOnlyList<string> TargetKinds,
    bool RequiresHostRoot,
    string? ShortcutCombo,
    string? Trigger = null,
    bool ShowInRootSearch = true)
{
    // 兼容旧调用点（K1 只传 priority + input）。
    public CommandBindingDto(int Priority, string Input)
        : this(Priority, Input, null, Array.Empty<string>(), false, null) { }

    // 兼容旧调用点（K2 §4.3 前 5 参数）。
    public CommandBindingDto(
        int Priority, string Input, string? Cardinality,
        IReadOnlyList<string> TargetKinds, bool RequiresHostRoot)
        : this(Priority, Input, Cardinality, TargetKinds, RequiresHostRoot, null) { }
}
