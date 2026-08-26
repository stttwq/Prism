using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Windows.Data;

namespace Prism.Models;

/// <summary>
/// 设置页动作快捷键行（2026-08-21 设想；第一轮 bug 修复重做）：
/// 默认列表为空，用户点「添加动作」逐行加——行上先选动作（ComboBox，
/// 只列未被其他行占用的动作），再录组合键。Value 空串 = 该行未绑定，
/// 保存时跳过（不落盘）。Id 可变（换选动作），Label/ScopeText 随之联动。
/// </summary>
public sealed class ActionHotkeyEditItem : INotifyPropertyChanged
{
    private string _id;
    private string _value = "";
    private IReadOnlyList<ActionHotkeyCatalog.Entry> _availableActions = [];

    /// <summary>选中的动作 id（ComboBox SelectedValue）。</summary>
    public string Id
    {
        get => _id;
        set
        {
            if (_id == value) return;
            _id = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(Label));
            OnPropertyChanged(nameof(ScopeText));
        }
    }

    /// <summary>动作名（目录查表，未知 id 原样显示）。</summary>
    public string Label => ActionHotkeyCatalog.Find(_id)?.Label ?? _id;

    /// <summary>适用类型说明（如「文件 / 文件夹」）。</summary>
    public string ScopeText =>
        ActionHotkeyCatalog.ScopeText(ActionHotkeyCatalog.Find(_id)?.Kinds ?? ActionHotkeyKinds.None);

    /// <summary>本行 ComboBox 可选动作（设置页刷新：目录减去其他行已占用的）。</summary>
    /// <remarks>返回按 Category 分组的 ListCollectionView，供下拉 GroupStyle 出 section header。</remarks>
    public IReadOnlyList<ActionHotkeyCatalog.Entry> AvailableActions
    {
        get => _availableActions;
        set
        {
            if (ReferenceEquals(_availableActions, value)) return;
            _availableActions = value;
            OnPropertyChanged();
            // 同步刷新分组视图（ComboBox ItemsSource 绑它）。
            var view = CollectionViewSource.GetDefaultView(_availableActions);
            if (view is ListCollectionView lcv)
            {
                lcv.GroupDescriptions.Clear();
                lcv.GroupDescriptions.Add(new PropertyGroupDescription("Category"));
            }
            else if (value is not null)
            {
                // 非 ListCollectionView 情况（罕见）回退新建一个，保证分组生效。
                var grouped = new ListCollectionView((System.Collections.IList)value);
                grouped.GroupDescriptions.Add(new PropertyGroupDescription("Category"));
            }
            OnPropertyChanged(nameof(AvailableActionsView));
        }
    }

    /// <summary>ComboBox 实际绑定的分组视图（AvailableActions 的分组包装）。</summary>
public System.Collections.IEnumerable AvailableActionsView
        => CollectionViewSource.GetDefaultView(_availableActions);

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

    public ActionHotkeyEditItem(string id, string value)
    {
        _id = id;
        _value = value;
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
