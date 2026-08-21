using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace Prism.Models;

/// <summary>
/// 设置页动作快捷键行（2026-08-21 设想）：动作名 + 适用类型 + 录键框值。
/// Value 为空串表示未绑定；清除按钮只清 Value，不改目录行集合。
/// </summary>
public sealed class ActionHotkeyEditItem : INotifyPropertyChanged
{
    public string Id { get; }
    public string Label { get; }

    /// <summary>适用类型说明（如「文件 / 文件夹」「应用」）。</summary>
    public string ScopeText { get; }

    private string _value = "";

    public string Value
    {
        get => _value;
        set
        {
            if (_value == value) return;
            _value = value;
            OnPropertyChanged();
        }
    }

    public ActionHotkeyEditItem(string id, string label, string scopeText, string value)
    {
        Id = id;
        Label = label;
        ScopeText = scopeText;
        _value = value;
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
