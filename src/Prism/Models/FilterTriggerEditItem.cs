using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace Prism.Models;

/// <summary>
/// 设置页可编辑的过滤触发词行。与 <see cref="FilterTrigger"/> 记录类型分离，
/// 以便双向绑定 TextBox 时触发 PropertyChanged。
/// </summary>
public sealed class FilterTriggerEditItem : INotifyPropertyChanged
{
    private string _keyword = "";
    private string _filterType = "ext";
    private string _description = "";

    /// <summary>各类型的默认说明（与 Settings.DefaultFilterTriggers 文案同风格）。</summary>
    private static readonly Dictionary<string, string> TypeLabels = new()
    {
        ["ext"] = "扩展名过滤",
        ["path"] = "路径过滤",
        ["size"] = "大小过滤",
        ["dm"] = "修改日期过滤",
        ["dc"] = "创建日期过滤",
        ["file"] = "只搜文件",
        ["folder"] = "只搜文件夹",
    };

    private static readonly HashSet<string> AutoLabels =
        new(TypeLabels.Values, StringComparer.Ordinal);

    public static string DefaultDescriptionFor(string filterType) =>
        TypeLabels.TryGetValue(filterType, out var label) ? label : "";

    /// <summary>说明还是自动文案（空/「新触发词」/某类型默认说明）时才跟随类型改写，
    /// 用户自定义过的说明不动。</summary>
    private bool IsAutoDescription =>
        _description.Length == 0 || _description == "新触发词" || AutoLabels.Contains(_description);

    public event PropertyChangedEventHandler? PropertyChanged;

    public FilterTriggerEditItem() { }

    public FilterTriggerEditItem(FilterTrigger trigger)
    {
        _keyword = trigger.Keyword;
        _filterType = trigger.FilterType;
        _description = trigger.Description;
    }

    public string Keyword
    {
        get => _keyword;
        set
        {
            if (_keyword == value) return;
            _keyword = value;
            OnPropertyChanged();
        }
    }

    public string FilterType
    {
        get => _filterType;
        set
        {
            if (_filterType == value) return;
            _filterType = value;
            if (IsAutoDescription)
            {
                _description = DefaultDescriptionFor(value);
                OnPropertyChanged(nameof(Description));
            }
            OnPropertyChanged();
        }
    }

    public string Description
    {
        get => _description;
        set
        {
            if (_description == value) return;
            _description = value;
            OnPropertyChanged();
        }
    }

    public FilterTrigger ToTrigger() => new(
        Keyword.Trim(),
        FilterType.Trim(),
        Description.Trim());

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
