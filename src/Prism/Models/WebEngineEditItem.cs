using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace Prism.Models;

/// <summary>
/// 设置页可编辑的网页引擎行。与 <see cref="WebEngine"/> 记录类型分离，
/// 以便双向绑定 TextBox 时触发 PropertyChanged。
/// </summary>
public sealed class WebEngineEditItem : INotifyPropertyChanged
{
    private string _keyword = "";
    private string _name = "";
    private string _urlTemplate = "";

    public event PropertyChangedEventHandler? PropertyChanged;

    public WebEngineEditItem() { }

    public WebEngineEditItem(WebEngine engine)
    {
        _keyword = engine.Keyword;
        _name = engine.Name;
        _urlTemplate = engine.UrlTemplate;
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

    public string Name
    {
        get => _name;
        set
        {
            if (_name == value) return;
            _name = value;
            OnPropertyChanged();
        }
    }

    public string UrlTemplate
    {
        get => _urlTemplate;
        set
        {
            if (_urlTemplate == value) return;
            _urlTemplate = value;
            OnPropertyChanged();
        }
    }

    public WebEngine ToEngine() => new(
        Keyword.Trim(),
        Name.Trim(),
        UrlTemplate.Trim());

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
