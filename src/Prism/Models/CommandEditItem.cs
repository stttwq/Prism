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
    private bool _fallback;
    private bool _enabled = true;
    private string _danger = "normal";
    // COMMAND-SIMPLIFY A3（2026-08-30）：命令类型（web/selection/open/folder/
    // advanced）。不参与 ToDefinition() 输出——类型不进持久化格式（A1.1），只
    // 驱动一级表单可见性与保存前的字段归一。既有命令按 A1.1 反推。
    private string _commandKind = "web";

    /// <summary>K4b：声明式参数行（表格编辑，broker 侧做结构校验）。</summary>
    public System.Collections.ObjectModel.ObservableCollection<CommandArgumentEditItem> Arguments { get; } =
        new();

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
        _fallback = desc.Fallback;
        foreach (var arg in desc.Arguments ?? [])
            Arguments.Add(new CommandArgumentEditItem
            {
                Name = arg.Name,
                Required = arg.Required,
                Default = arg.Default,
            });

        // handler 与参数：K4b 收尾起 catalog 随描述符下发，编辑既有命令可完整回显
        // （此前 URL/程序路径丢失，重存被 broker 校验拒绝）。旧 broker 无此字段时
        // 保持 open_url 默认。
        if (!string.IsNullOrEmpty(desc.Handler))
            _handler = desc.Handler;
        if (desc.HandlerParams is { } hp)
        {
            if (hp.TryGetValue("url_template", out var url)) _urlTemplate = url;
            if (hp.TryGetValue("path", out var path)) _programPath = path;
            if (hp.TryGetValue("args_template", out var args)) _argsTemplate = args;
            if (hp.TryGetValue("working_dir", out var wd)) _workingDir = wd;
        }

        // A1.1：类型不进持久化，重新编辑时反推。推错（落到 advanced）的代价
        // 只是表单默认展开「高级」区，不影响任何行为——够用，不加 kind 字段。
        _commandKind = InferKind(_handler, _urlTemplate, _argsTemplate);
    }

    /// <summary>A1.1 类型反推。顺序：① web → ② selection → ④ folder → 其余 advanced。</summary>
    private static string InferKind(string handler, string urlTemplate, string argsTemplate)
    {
        if (handler == "open_url" && urlTemplate.Contains("{query}"))
            return "web";
        if (handler == "launch_program")
        {
            if (argsTemplate.Contains("{selection.target}")) return "selection";
            if (argsTemplate.Contains("{current_folder}")) return "folder";
            return "open";
        }
        return "advanced";
    }

    /// <summary>唯一标识。新建时由 ViewModel 分配 user.<guid> 形式的 id。</summary>
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
        set
        {
            if (_keywordsText != value)
            {
                _keywordsText = value;
                OnPropertyChanged();
                OnPropertyChanged(nameof(EffectiveTriggerDisplay));
            }
        }
    }

    /// <summary>open_url | launch_program。</summary>
    public string Handler
    {
        get => _handler;
        set
        {
            if (_handler != value)
            {
                _handler = value;
                OnPropertyChanged();
                OnPropertyChanged(nameof(IsOpenUrl));
                OnPropertyChanged(nameof(IsLaunchProgram));
                OnPropertyChanged(nameof(ShowUrlTemplate));
                OnPropertyChanged(nameof(ShowProgramPath));
                OnPropertyChanged(nameof(ShowArgsTemplateTopLevel));
            }
        }
    }

    public bool IsOpenUrl => Handler == "open_url";
    public bool IsLaunchProgram => Handler == "launch_program";

    /// <summary>A3：命令类型。四选一 radio 绑定（web/selection/open/folder）；
    /// advanced = 反推不出的存量命令，一级表单只出标题/关键字，其余全在高级区。</summary>
    public string CommandKind
    {
        get => _commandKind;
        set
        {
            if (_commandKind != value)
            {
                _commandKind = value;
                OnPropertyChanged();
                RaiseKindDependentProperties();
            }
        }
    }

    private void RaiseKindDependentProperties()
    {
        OnPropertyChanged(nameof(IsKindWeb));
        OnPropertyChanged(nameof(IsKindSelection));
        OnPropertyChanged(nameof(IsKindOpen));
        OnPropertyChanged(nameof(IsKindFolder));
        OnPropertyChanged(nameof(IsAdvanced));
        OnPropertyChanged(nameof(ShowUrlTemplate));
        OnPropertyChanged(nameof(ShowProgramPath));
        OnPropertyChanged(nameof(ShowArgsTemplateTopLevel));
        OnPropertyChanged(nameof(ShowWebHint));
        OnPropertyChanged(nameof(ShowSelectionHint));
        OnPropertyChanged(nameof(ShowFolderHint));
        OnPropertyChanged(nameof(KeywordsEnabled));
    }

    /// <summary>radio 双向绑定：选中即收敛类型（get-only 会让组内互斥失效）。</summary>
    public bool IsKindWeb { get => CommandKind == "web"; set { if (value) SetKind("web"); } }
    public bool IsKindSelection { get => CommandKind == "selection"; set { if (value) SetKind("selection"); } }
    public bool IsKindOpen { get => CommandKind == "open"; set { if (value) SetKind("open"); } }
    public bool IsKindFolder { get => CommandKind == "folder"; set { if (value) SetKind("folder"); } }
    /// <summary>反推不出的存量命令：高级区默认展开，字段一个不丢。</summary>
    public bool IsAdvanced => CommandKind == "advanced";

    /// <summary>A3.2：radio 选中类型时收敛表单。② 自动清关键字/根搜索并预填
    /// {selection.target}（配合 broker A2.2 互斥裁决，保存必过）；④ 预填
    /// {current_folder}。①固定 open_url；③不动 handler（网址或程序都合法）。</summary>
    public void SetKind(string kind)
    {
        if (CommandKind == kind) return;
        CommandKind = kind;
        switch (kind)
        {
            case "web":
                Handler = "open_url";
                break;
            case "selection":
                Handler = "launch_program";
                KeywordsText = "";
                ShowInRootSearch = false;
                if (string.IsNullOrWhiteSpace(ArgsTemplate))
                    ArgsTemplate = "{selection.target}";
                break;
            case "folder":
                Handler = "launch_program";
                if (string.IsNullOrWhiteSpace(ArgsTemplate))
                    ArgsTemplate = "{current_folder}";
                break;
        }
    }

    /// <summary>一级表单 URL 模板可见性（web/open 类型；advanced 在高级区）。</summary>
    public bool ShowUrlTemplate => IsOpenUrl && (IsKindWeb || IsKindOpen);
    /// <summary>一级表单程序路径可见性（selection/open/folder 类型）。</summary>
    public bool ShowProgramPath => IsLaunchProgram && (IsKindSelection || IsKindOpen || IsKindFolder);
    /// <summary>④ 一级表单参数模板（预填 {current_folder}，可直接改）。</summary>
    public bool ShowArgsTemplateTopLevel => IsLaunchProgram && IsKindFolder;

    /// <summary>类型② 关键字输入框置灰（动作面板专用，关键字恒为空）。</summary>
    public bool KeywordsEnabled => !IsKindSelection;

    /// <summary>A3.5：一级占位符提示按类型收敛（完整速查在高级区）。</summary>
    public bool ShowWebHint => IsKindWeb;
    public bool ShowSelectionHint => IsKindSelection;
    public bool ShowFolderHint => IsKindFolder;

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

    /// <summary>
    /// A2.3：关键字路由触发词 = Trigger（存量回显）留空时恒取关键字第一项
    /// （ToDefinition 的既有兜底成为唯一路径）。只读展示，不再有独立输入框。
    /// </summary>
    public string EffectiveTriggerDisplay
    {
        get
        {
            var trigger = _trigger.Trim();
            if (trigger.Length == 0)
            {
                foreach (var kw in _keywordsText.Split(','))
                {
                    var t = kw.Trim();
                    if (t.Length > 0) { trigger = t; break; }
                }
            }
            return trigger.Length > 0
                ? $"关键字路由触发词：{trigger}（= 关键字第一项）"
                : "不经过关键字路由（填写关键字后自动启用）";
        }
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

    /// <summary>K4a：无结果回退——根搜索空结果时该命令作为回退行出现。</summary>
    public bool Fallback
    {
        get => _fallback;
        set { if (_fallback != value) { _fallback = value; OnPropertyChanged(); } }
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

    /// <summary>
    /// 是否为内置命令（只读展示，仅快捷键可改；不可编辑/删除）。
    /// broker 内置 id 一律 prism.* 前缀（prism.settings.open 等）；
    /// "builtin." 是历史误判前缀，保留只为兼容。
    /// </summary>
    public bool IsBuiltin =>
        !string.IsNullOrEmpty(Id) &&
        (Id.StartsWith("prism.") || Id.StartsWith("builtin."));

    /// <summary>用户命令 = 表单可编辑。XAML 绑定用。</summary>
    public bool IsUserCommand => !IsBuiltin;

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
            // 默认进动作面板（target_kinds 空 = 接受所有目标类型）：动作面板是
            // {selection.target} 唯一有值的入口——面板行就是选中文件。不默认绑的话
            // 用户命令永远到不了面板（设置页也没有单独的面板开关）。
            ActionPanel = new CommandBindingSpecDto { ShowInRootSearch = true },
        };
        // 关键字路由：Trigger 优先；留空时用 Keywords 首项自动回退。
        var trigger = Trigger.Trim();
        if (string.IsNullOrEmpty(trigger) && keywords.Count > 0)
            trigger = keywords[0];
        if (!string.IsNullOrEmpty(trigger))
            bindings.Keyword = new CommandBindingSpecDto { Trigger = trigger };
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
            Fallback = Fallback,
            Arguments = Arguments
                .Where(a => !string.IsNullOrWhiteSpace(a.Name))
                .Select(a => new CommandArgumentSpecDto
                {
                    Name = a.Name.Trim(),
                    Required = a.Required,
                    Default = a.Default.Trim(),
                })
                .ToList(),
        };
    }

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

/// <summary>命令模板预设（§4.7：预填表单，不绕过校验）。A4（2026-08-30）：
/// Kind 首位——套用模板同时收敛表单类型（selection 模板自动清关键字/根搜索）。</summary>
public sealed record CommandTemplate(
    string Kind,
    string DisplayName,
    string Handler,
    string Title,
    string UrlTemplate,
    string ProgramPath,
    string ArgsTemplate,
    string WorkingDir,
    string KeywordsText,
    string IconGlyph);

/// <summary>
/// K4b：参数表格的可编辑行。INotifyPropertyChanged 供 CheckBox/TextBox 双向绑定；
/// 结构校验（数量/重名/必填顺序）在 broker 侧做，UI 只过滤空名行。
/// </summary>
public sealed class CommandArgumentEditItem : INotifyPropertyChanged
{
    private string _name = "";
    private bool _required;
    private string _default = "";

    public event PropertyChangedEventHandler? PropertyChanged;

    public string Name
    {
        get => _name;
        set { if (_name != value) { _name = value; OnPropertyChanged(); } }
    }

    public bool Required
    {
        get => _required;
        set { if (_required != value) { _required = value; OnPropertyChanged(); } }
    }

    public string Default
    {
        get => _default;
        set { if (_default != value) { _default = value; OnPropertyChanged(); } }
    }

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
