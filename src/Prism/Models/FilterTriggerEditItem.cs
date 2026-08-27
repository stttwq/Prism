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
