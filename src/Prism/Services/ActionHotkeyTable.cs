using System.Windows.Input;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 窗口级动作快捷键表（2026-08-21 设想）：把 Settings.ActionHotkeys 解析成
/// (主键, 修饰键集合) → 动作 id 的查找表，供搜索窗口按键管道先查表再走硬编码导航。
///
/// 保留键（导航/Ctrl+Enter/Ctrl+G/Ctrl+数字）永远优先于用户表——这是
/// 「其他功能不受影响」的硬不变量：用户无论绑什么，原有按键行为分毫不动。
/// </summary>
public sealed class ActionHotkeyTable
{
    /// <summary>空表：未装配时的默认，TryMatch 恒 false。</summary>
    public static ActionHotkeyTable Empty { get; } = new();

    private readonly Dictionary<(Key Key, ModifierKeys Mods), string> _map = new();

    /// <summary>
    /// 从设置构建：只收录目录内的动作 id；组合键解析失败（无修饰键/主键不认识）
    /// 或落在保留集内的条目静默丢弃（手改 settings.json 的防御）；
    /// 同一组合键撞车时按目录顺序先到先得。
    /// </summary>
    public static ActionHotkeyTable FromSettings(IReadOnlyDictionary<string, string> bindings)
    {
        var table = new ActionHotkeyTable();
        foreach (var entry in ActionHotkeyCatalog.Entries)
        {
            if (!bindings.TryGetValue(entry.Id, out var combo) || string.IsNullOrWhiteSpace(combo))
                continue;
            var parsed = Parse(combo);
            if (parsed is null || IsReserved(parsed.Value.Key, parsed.Value.Mods))
                continue;
            table._map.TryAdd((parsed.Value.Key, parsed.Value.Mods), entry.Id);
        }
        return table;
    }

    /// <summary>精确匹配：修饰键集合相等（绑 Ctrl+C 不误吃 Ctrl+Shift+C）。</summary>
    public bool TryMatch(Key key, ModifierKeys mods, out string actionId)
    {
        if (_map.TryGetValue((key, mods), out actionId!))
            return true;
        actionId = "";
        return false;
    }

    /// <summary>
    /// 解析 "Ctrl+Shift+C" 形式组合键。主键按 <see cref="Key"/> 枚举名解析
    /// （与 HotkeyRecorderBox 输出、HotkeyService.ParseCombo 同规则，含单数字别名）。
    /// 必须至少一个修饰键且有可解析主键，否则返回 null。
    /// </summary>
    public static (Key Key, ModifierKeys Mods)? Parse(string combo)
    {
        if (string.IsNullOrWhiteSpace(combo)) return null;
        var mods = ModifierKeys.None;
        var key = Key.None;
        foreach (var part in combo.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            switch (part.ToUpperInvariant())
            {
                case "CTRL":
                case "CONTROL":
                    mods |= ModifierKeys.Control;
                    continue;
                case "ALT":
                    mods |= ModifierKeys.Alt;
                    continue;
                case "SHIFT":
                    mods |= ModifierKeys.Shift;
                    continue;
                case "WIN":
                case "WINDOWS":
                case "LWIN":
                case "RWIN":
                    mods |= ModifierKeys.Windows;
                    continue;
            }
            if (key != Key.None)
                return null; // 出现第二个主键，不是合法组合
            if (part.Length == 1 && part[0] is >= '0' and <= '9')
            {
                if (!Enum.TryParse<Key>("D" + part, true, out var digitKey))
                    return null;
                key = digitKey;
            }
            else if (Enum.TryParse<Key>(part, true, out var parsed) && parsed != Key.None)
            {
                key = parsed;
            }
            else
            {
                return null;
            }
        }
        if (key == Key.None || mods == ModifierKeys.None)
            return null;
        return (key, mods);
    }

    /// <summary>
    /// 规范化组合键串（固定顺序 Ctrl+Alt+Shift+Win+主键枚举名）。
    /// 无法解析返回 null。写回设置与冲突比较都用规范化形式。
    /// </summary>
    public static string? Canonicalize(string combo)
    {
        var parsed = Parse(combo);
        if (parsed is null) return null;
        var (key, mods) = parsed.Value;
        var parts = new List<string>(5);
        if (mods.HasFlag(ModifierKeys.Control)) parts.Add("Ctrl");
        if (mods.HasFlag(ModifierKeys.Alt)) parts.Add("Alt");
        if (mods.HasFlag(ModifierKeys.Shift)) parts.Add("Shift");
        if (mods.HasFlag(ModifierKeys.Windows)) parts.Add("Win");
        parts.Add(key.ToString());
        return string.Join("+", parts);
    }

    /// <summary>
    /// 保留键集合：这些键/组合永远走原硬编码导航逻辑，用户表不得占用。
    /// 方向键/Enter/Esc 任意修饰组合都保留（现有 switch 不看修饰键）；
    /// Ctrl+G、Ctrl+数字同理（现有逻辑只看 HasFlag(Control)）。
    /// Ctrl+Enter 属 Enter 保留集——「原宿主定位」语义保持现状。
    /// </summary>
    public static bool IsReserved(Key key, ModifierKeys mods)
    {
        switch (key)
        {
            case Key.Up:
            case Key.Down:
            case Key.Enter:
            case Key.Escape:
            case Key.Left:
            case Key.Right:
                return true;
            case Key.G:
                return mods.HasFlag(ModifierKeys.Control);
            case Key.D0 or Key.D1 or Key.D2 or Key.D3 or Key.D4
                 or Key.D5 or Key.D6 or Key.D7 or Key.D8 or Key.D9
                 or Key.NumPad0 or Key.NumPad1 or Key.NumPad2 or Key.NumPad3 or Key.NumPad4
                 or Key.NumPad5 or Key.NumPad6 or Key.NumPad7 or Key.NumPad8 or Key.NumPad9:
                return mods.HasFlag(ModifierKeys.Control);
            default:
                return false;
        }
    }

    /// <summary>
    /// 设置保存前的整表校验（设置页与 SettingsStore 双侧调用）。
    /// 返回错误文案；合法返回 null。规则：id 必须在目录内、组合键可解析、
    /// 不落保留集、同一组合键不得绑两个动作。
    /// </summary>
    public static string? ValidateBindings(IReadOnlyDictionary<string, string> bindings)
    {
        var seen = new Dictionary<(Key Key, ModifierKeys Mods), string>();
        foreach (var (id, combo) in bindings)
        {
            if (string.IsNullOrWhiteSpace(combo)) continue;
            var entry = ActionHotkeyCatalog.Find(id);
            if (entry is null)
                return $"未知动作 id：{id}";
            var parsed = Parse(combo);
            if (parsed is null)
                return $"「{entry.Label}」的组合键「{combo}」无法解析（需至少一个修饰键 + 主键）";
            var (key, mods) = parsed.Value;
            if (IsReserved(key, mods))
                return $"「{entry.Label}」的组合键「{Canonicalize(combo)}」保留给导航，请换一个";
            if (seen.TryGetValue((key, mods), out var other))
                return $"「{entry.Label}」和「{other}」绑了同一个组合键「{Canonicalize(combo)}」";
            seen[(key, mods)] = entry.Label;
        }
        return null;
    }
}
