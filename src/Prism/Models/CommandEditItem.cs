using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace Prism.Models;

/// <summary>
/// K3 §4.7：设置页可编辑的命令行。与 broker 持久化形态 <see cref="UserCommandDefinition"/>
/// 分离——双向绑定 TextBox 时触发 PropertyChanged。
/// 只承载用户命令（builtin 命令只读展示，不可编辑）。
/// </summary>
public sealed class CommandEditItem : INotifyPropertyChanged
{
    private string _id = "";
    private string _title = "";
    private string _subtitle = "";
    private string _iconGlyph = "";
    private string _keywordsText = "";
    private string _handler = "open_url";
    private string _urlTemplate = "";
    private string _programPath = "";
    private string _argsTemplate = "";
    private string _workingDir = "";
    private string _trigger = "";
    private string _shortcutCombo = "";
    private bool _showInRootSearch = true;
    private bool _enabled = true;
    private string _danger = "normal";

    public event PropertyChangedEventHandler? PropertyChanged;

    public CommandEditItem() { }

    /// <summary>从 broker 下发描述符构造（只读展示 builtin / 已有用户命令）。</summary>
    public CommandEditItem(CommandDescriptor desc)
    {
        _id = desc.Id;
        _title = desc.Title;
        _subtitle = desc.Subtitle;
        _iconGlyph = desc.IconGlyph;
        _keywordsText = string.Join(", ", desc.Keywords);
        _enabled = desc.Enabled;
        _danger = desc.Danger;
        _shortcutCombo = desc.Bindings.Shortcut?.ShortcutCombo ?? "";
        _trigger = desc.Bindings.Keyword?.Trigger ?? "";
        _showInRootSearch = desc.Bindings.RootSearch?.ShowInRootSearch ?? true;

        // handler 从 subtitle/handler 不可直接获取——CommandDescriptor 不含 handler 字段。
        // 用户命令从 Subtitle 反推不够可靠；对 builtin 命令 handler 字段无意义（不可编辑）。
        // 新建命令默认 open_url；编辑时由 ViewModel 从 broker preview 请求补充。
        _handler = "open_url";
    }

    /// <summary>唯一标识。空串=新建（保存时 broker 分配 id）。</summary>
    public string Id
    {
        get => _id;
        set { if (_id != value) { _id = value; OnPropertyChanged(); } }
    }

    public string Title
    {
        get => _title;
        set { if (_title != value) { _title = value; OnPropertyChanged(); } }
    }

    public string Subtitle
    {
        get => _subtitle;
        set { if (_subtitle != value) { _subtitle = value; OnPropertyChanged(); } }
    }

    public string IconGlyph
    {
        get => _iconGlyph;
        set { if (_iconGlyph != value) { _iconGlyph = value; OnPropertyChanged(); } }
    }

    /// <summary>逗号分隔的关键字文本（编辑用），保存时拆分。</summary>
    public string KeywordsText
    {
        get => _keywordsText;
        set { if (_keywordsText != value) { _keywordsText = value; OnPropertyChanged(); } }
    }

    /// <summary>open_url | launch_program。</summary>
    public string Handler
    {
        get => _handler;
        set { if (_handler != value) { _handler = value; OnPropertyChanged(); } }
    }

    public bool IsOpenUrl => Handler == "open_url";
    public bool IsLaunchProgram => Handler == "launch_program";

    public string UrlTemplate
    {
        get => _urlTemplate;
        set { if (_urlTemplate != value) { _urlTemplate = value; OnPropertyChanged(); } }
    }

    public string ProgramPath
    {
        get => _programPath;
        set { if (_programPath != value) { _programPath = value; OnPropertyChanged(); } }
    }

    public string ArgsTemplate
    {
        get => _argsTemplate;
        set { if (_argsTemplate != value) { _argsTemplate = value; OnPropertyChanged(); } }
    }

    public string WorkingDir
    {
        get => _workingDir;
        set { if (_workingDir != value) { _workingDir = value; OnPropertyChanged(); } }
    }

    public string Trigger
    {
        get => _trigger;
        set { if (_trigger != value) { _trigger = value; OnPropertyChanged(); } }
    }

    public string ShortcutCombo
    {
        get => _shortcutCombo;
        set { if (_shortcutCombo != value) { _shortcutCombo = value; OnPropertyChanged(); } }
    }

    public bool ShowInRootSearch
    {
        get => _showInRootSearch;
        set { if (_showInRootSearch != value) { _showInRootSearch = value; OnPropertyChanged(); } }
    }

    public bool Enabled
    {
        get => _enabled;
        set { if (_enabled != value) { _enabled = value; OnPropertyChanged(); } }
    }

    public string Danger
    {
        get => _danger;
        set { if (_danger != value) { _danger = value; OnPropertyChanged(); } }
    }

    /// <summary>是否为内置命令（只读展示，不可编辑/删除）。</summary>
    public bool IsBuiltin => !string.IsNullOrEmpty(Id) && Id.StartsWith("builtin.");

    /// <summary>从编辑态构造持久化形态，用于发送给 broker。</summary>
    public UserCommandDefinition ToDefinition()
    {
        var keywords = new List<string>();
        foreach (var kw in KeywordsText.Split(','))
        {
            var t = kw.Trim();
            if (t.Length > 0) keywords.Add(t);
        }

        var handlerParams = new Dictionary<string, string>();
        if (Handler == "open_url")
        {
            handlerParams["url_template"] = UrlTemplate;
        }
        else if (Handler == "launch_program")
        {
            handlerParams["path"] = ProgramPath;
            if (!string.IsNullOrEmpty(ArgsTemplate))
                handlerParams["args_template"] = ArgsTemplate;
            if (!string.IsNullOrEmpty(WorkingDir))
                handlerParams["working_dir"] = WorkingDir;
        }

        var bindings = new CommandBindingsSpecDto
        {
            RootSearch = new CommandBindingSpecDto { ShowInRootSearch = ShowInRootSearch },
        };
        if (!string.IsNullOrWhiteSpace(Trigger))
            bindings.Keyword = new CommandBindingSpecDto { Trigger = Trigger.Trim() };
        if (!string.IsNullOrWhiteSpace(ShortcutCombo))
            bindings.Shortcut = new CommandBindingSpecDto { ShortcutCombo = ShortcutCombo.Trim() };

        return new UserCommandDefinition
        {
            Id = Id,
            Title = Title.Trim(),
            Subtitle = Subtitle.Trim(),
            IconGlyph = IconGlyph,
            Keywords = keywords,
            Input = new CommandInputSpecDto { Kind = "text", Required = true, Prompt = "输入查询词" },
            Bindings = bindings,
            Danger = Danger,
            Enabled = Enabled,
            Handler = Handler,
            HandlerParams = handlerParams,
        };
    }

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

/// <summary>命令模板预设（§4.7：预填表单，不绕过校验）。</summary>
public sealed record CommandTemplate(
    string DisplayName,
    string Handler,
    string Title,
    string UrlTemplate,
    string ProgramPath,
    string ArgsTemplate,
    string WorkingDir,
    string KeywordsText,
    string IconGlyph);
