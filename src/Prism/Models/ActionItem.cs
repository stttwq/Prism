namespace Prism.Models;

/// <summary>动作面板单项（frontend-spec.md §2）。</summary>
/// <param name="Id">回传后端：open_folder / copy / cut / copy_path / shell:n</param>
/// <param name="Label">中文动作名</param>
/// <param name="IconGlyph">Segoe MDL2 / Fluent Icons 字形；无图标传空</param>
/// <param name="HasSubmenu">右端 ">" 箭头</param>
/// <param name="IsSectionHeader">true 时渲染为灰色小标题（如"快捷菜单"）</param>
/// <remarks>
/// K2 §4.2：新字段作为带默认值的 init 属性追加，不动主构造器签名——
/// 现有构造点（PipeClient 解析、ActionPanel、右键菜单、测试夹具）一行不改
/// 继续编译，默认值天然等于「这是内置动作」。
/// </remarks>
public sealed record ActionItem(
    string Id,
    string Label,
    string IconGlyph,
    bool HasSubmenu,
    bool IsSectionHeader)
{
    /// <summary>K2: "builtin_action" | "command"。默认内置动作。</summary>
    public string InvocationKind { get; init; } = "builtin_action";
    /// <summary>命令动作的 command id；内置动作为 null。</summary>
    public string? CommandId { get; init; }
    /// <summary>是否可用。内置动作恒 true。</summary>
    public bool IsEnabled { get; init; } = true;
    /// <summary>禁用原因（IsEnabled=false 时展示）。</summary>
    public string? DisabledReason { get; init; }
}

/// <summary>
/// 动作参数：copy_to/move_to 携带 Destination，rename 携带 NewName。
/// 所有字段可选，由具体动作决定哪些是必填。
/// </summary>
public sealed record ActionArgs
{
    public string? Destination { get; init; }
    public string? NewName { get; init; }

    public static ActionArgs Empty { get; } = new();
}
