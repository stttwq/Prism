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

    /// <summary>各类型的默认说明：功能名 + 简短用法（引号内是关键词后跟的内容）。</summary>
    private static readonly Dictionary<string, string> TypeLabels = new()
    {
        ["ext"] = "扩展名过滤，如「txt 报告」",
        ["path"] = "路径过滤，如「d:\\docs 报告」",
        ["size"] = "大小过滤，如「>1mb 报告」",
        ["dm"] = "修改日期过滤，如「today、>2024」",
        ["dc"] = "创建日期过滤，如「thisweek、2024」",
        ["file"] = "只搜文件（旗标，后跟搜索词）",
        ["folder"] = "只搜文件夹（旗标，后跟搜索词）",
    };

    /// <summary>视为自动文案的说明集合：现行默认全文 + 旧版纯功能名（存量行）
    /// + 空/「新触发词」。命中才随类型改写，用户自定义过的说明不动。</summary>
    private static readonly HashSet<string> AutoLabels = new(TypeLabels.Values, StringComparer.Ordinal)
    {
        "",
        "新触发词",
        "扩展名过滤",
        "路径过滤",
        "大小过滤",
        "修改日期过滤",
        "创建日期过滤",
        "只搜文件",
        "只搜文件夹",
    };

    public static string DefaultDescriptionFor(string filterType) =>
        TypeLabels.TryGetValue(filterType, out var label) ? label : "";

    /// <summary>说明还是自动文案时才跟随类型改写，用户自定义过的说明不动。</summary>
    private bool IsAutoDescription => AutoLabels.Contains(_description);

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
