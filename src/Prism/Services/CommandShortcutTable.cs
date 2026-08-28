using System.Windows.Input;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// K2 §4.6：命令快捷键表。从 broker 目录快照中带有 <c>bindings.shortcut.shortcut_combo</c>
/// 的命令构建。与 <see cref="ActionHotkeyTable"/> 分开存储——语义不同（动作快捷键需要
/// 选中结果，命令快捷键可能不需要）、存储所有权不同（broker 独占 commands-v1.json
/// vs WPF 全量读-改-写 settings.json）。
///
/// 复用 <see cref="ActionHotkeyTable.Parse"/> 做组合键字符串解析（纯函数，无状态），
/// 但不复用 <see cref="ActionHotkeyCatalog"/> 的 15 条静态镜像。
/// </summary>
internal sealed class CommandShortcutTable
{
    /// <summary>空表：未装配时的默认，TryMatch 恒 false。</summary>
    public static CommandShortcutTable Empty { get; } = new();

    private readonly Dictionary<(Key Key, ModifierKeys Mods), CommandShortcutEntry> _map = new();

    /// <summary>
    /// 从目录快照构建：遍历所有命令，提取 bindings.shortcut.shortcut_combo，
    /// 用 <see cref="ActionHotkeyTable.Parse"/> 解析后入表。
    /// 保留键（导航/Ctrl+Enter 等）永远进不去——解析后 IsReserved 的条目静默丢弃。
    /// 同一组合键撞车时按目录顺序先到先得。
    /// </summary>
    public static CommandShortcutTable FromCatalog(IReadOnlyList<CommandDescriptor> catalog)
    {
        var table = new CommandShortcutTable();
        foreach (var cmd in catalog)
        {
            if (cmd.Bindings.Shortcut is not { } binding)
                continue;
            if (string.IsNullOrWhiteSpace(binding.ShortcutCombo))
                continue;
            if (!cmd.Enabled)
                continue;
            var parsed = ActionHotkeyTable.Parse(binding.ShortcutCombo);
            if (parsed is null)
                continue;
            var (key, mods) = parsed.Value;
            if (ActionHotkeyTable.IsReserved(key, mods))
                continue;
            var entry = new CommandShortcutEntry(
                cmd.Id, cmd.Title, cmd.Input.Kind);
            table._map.TryAdd((key, mods), entry);
        }
        return table;
    }

    /// <summary>精确匹配：修饰键集合相等。命中返回命令 id 与元数据。</summary>
    public bool TryMatch(
        Key key, ModifierKeys mods,
        out CommandShortcutEntry entry)
    {
        if (_map.TryGetValue((key, mods), out entry!))
            return true;
        entry = default;
        return false;
    }

    /// <summary>
    /// K2 §4.6：冲突检测——命令快捷键与 ActionHotkeys 撞键时给出提示文案。
    /// 在设置页保存命令快捷键前调用。返回错误文案；合法返回 null。
    /// 规则：组合键可解析、不落保留集、不与任何 ActionHotkeys 同组合。
    /// </summary>
    public static string? ValidateConflict(string combo, IReadOnlyDictionary<string, string> actionHotkeys)
    {
        if (string.IsNullOrWhiteSpace(combo)) return null;
        var parsed = ActionHotkeyTable.Parse(combo);
        if (parsed is null)
            return "无法解析（需至少一个修饰键 + 主键）";
        var (key, mods) = parsed.Value;
        if (ActionHotkeyTable.IsReserved(key, mods))
            return $"{ActionHotkeyTable.Canonicalize(combo)} 保留给导航，请换一个";
        foreach (var (_, actionCombo) in actionHotkeys)
        {
            if (string.IsNullOrWhiteSpace(actionCombo)) continue;
            var other = ActionHotkeyTable.Parse(actionCombo);
            if (other is not null && other.Value.Key == key && other.Value.Mods == mods)
                return $"{ActionHotkeyTable.Canonicalize(combo)} 与动作快捷键冲突，请换一个";
        }
        return null;
    }
}

/// <summary>命中的命令快捷键条目。</summary>
internal readonly record struct CommandShortcutEntry(
    string CommandId,
    string Title,
    string InputKind);
