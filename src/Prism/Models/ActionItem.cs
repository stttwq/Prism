namespace Prism.Models;

/// <summary>动作面板单项（frontend-spec.md §2）。</summary>
/// <param name="Id">回传后端：open_folder / copy / cut / copy_path / shell:n</param>
/// <param name="Label">中文动作名</param>
/// <param name="IconGlyph">Segoe MDL2 / Fluent Icons 字形；无图标传空</param>
/// <param name="HasSubmenu">右端 ">" 箭头</param>
/// <param name="IsSectionHeader">true 时渲染为灰色小标题（如"快捷菜单"）</param>
public sealed record ActionItem(
    string Id,
    string Label,
    string IconGlyph,
    bool HasSubmenu,
    bool IsSectionHeader);
