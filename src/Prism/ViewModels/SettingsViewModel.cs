using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Windows.Data;
using System.Windows.Input;
using Prism.Models;
using Prism.Services;

namespace Prism.ViewModels;

/// <summary>
/// 设置页视图模型（步骤 8 完整实现）：
/// 常规（快捷键 / 自启 / 数据目录）、网页搜索（引擎列表增删）、关于。
/// 保存后通过回调让 App 热应用热键，并通知后端 reload_engines。
/// </summary>
public sealed class SettingsViewModel : INotifyPropertyChanged
{
    private readonly SettingsStore _store;
    private readonly AutoStartService _autoStart;
    private readonly Action<Settings>? _onApplied;
    private readonly Func<IReadOnlyList<WebEngine>, Task>? _onEnginesChanged;
    private readonly Func<bool, bool, Task>? _onPreferencesChanged;
    private readonly Func<Task>? _onClearHistory;
    /// <summary>G8：保存设置后通知 App 更新联想开关和引擎列表。</summary>
    private readonly Action<IReadOnlyList<WebEngine>, bool>? _onWebSettingsChanged;
    /// <summary>保存设置后通知 App 更新过滤触发词列表。</summary>
    private readonly Action<IReadOnlyList<FilterTrigger>>? _onFilterTriggersChanged;
    /// <summary>G8：自定义引擎 origin 变化时弹出 favicon 授权对话框。返回 true=授权。</summary>
    private readonly Func<string, bool>? _onRequestFaviconGrant;
    /// <summary>G8：授权成功后触发 favicon 下载（App 持有 FaviconCache，完成后刷新图标缓存）。</summary>
    private readonly Action<string>? _onFaviconGranted;
    /// <summary>别名系统（2026-08-21 设想）：设置页打开时拉取别名列表。</summary>
    private readonly Func<Task<IReadOnlyList<AliasEntry>>>? _onAliasList;
    /// <summary>别名系统：删除一条绑定。</summary>
    private readonly Func<ActionTarget, Task>? _onAliasDelete;

    // ── K3 §4.7 命令 tab 状态 ──
    /// <summary>命令目录快照（从 broker 拉取）。</summary>
    private IReadOnlyList<CommandDescriptor> _commandCatalog = Array.Empty<CommandDescriptor>();
    /// <summary>命令编辑表（用户命令可编辑，内置只读展示）。</summary>
    private ObservableCollection<CommandEditItem> _commands = [];
    /// <summary>命令 pipe 客户端（由 App 注入，可能为 null=命令功能不可用）。</summary>
    private readonly PipeClient? _commandPipe;
    /// <summary>命令 tab 的实时预览文本。</summary>
    private string _commandPreviewText = "";
    /// <summary>命令 tab 的命名空间冲突提示。</summary>
    private string _commandConflictText = "";
    /// <summary>命令 tab 的通用状态提示。</summary>
    private string _commandStatusText = "";
    private readonly Func<Task>? _onCommandsChanged;

    private bool _autoStartEnabled;
    private HotkeyMode _hotkeyMode;
    private string _comboHotkey;
    private string _statusMessage = "";
    private WebEngineEditItem? _selectedEngine;
    private FilterTriggerEditItem? _selectedFilterTrigger;
    private sealed class TabIndex
    {
        public const int General = 0;
        public const int QuickAccess = 1;
        public const int Web = 2;
        public const int Filters = 3;
        public const int About = 4;
        public const int Commands = 5;
    }

    private int _selectedTab; // 0=常规 1=快速访问 2=网页搜索 3=关于
    private readonly List<string> _excludedPaths;
    private bool _historyEnabled;
    private bool _pinyinEnabled;
    private bool _currentDirectorySearchEnabled;
    private bool _explorerHostIntegrationEnabled;
    private bool _directoryOpusHostIntegrationEnabled;
    private string? _zipProgram;
    private bool _suggestionsEnabled;
    /// <summary>暂存区（2026-08-22 计划）：容量（字符串编辑，保存时解析报错）。</summary>
    private string _stagingCapacityText;
    private string _stagingAddHotkey;

    public event PropertyChangedEventHandler? PropertyChanged;

    public SettingsViewModel(
        SettingsStore store,
        AutoStartService autoStart,
        Action<Settings>? onApplied = null,
        Func<IReadOnlyList<WebEngine>, Task>? onEnginesChanged = null,
        Func<bool, bool, Task>? onPreferencesChanged = null,
        Func<Task>? onClearHistory = null,
        Action<IReadOnlyList<WebEngine>, bool>? onWebSettingsChanged = null,
        Action<IReadOnlyList<FilterTrigger>>? onFilterTriggersChanged = null,
        Func<string, bool>? onRequestFaviconGrant = null,
        Action<string>? onFaviconGranted = null,
        Func<Task<IReadOnlyList<AliasEntry>>>? onAliasList = null,
        Func<ActionTarget, Task>? onAliasDelete = null,
        PipeClient? commandPipe = null,
        Func<Task>? onCommandsChanged = null)
    {
        _store = store;
        _autoStart = autoStart;
        _onApplied = onApplied;
        _onEnginesChanged = onEnginesChanged;
        _onPreferencesChanged = onPreferencesChanged;
        _onClearHistory = onClearHistory;
        _onWebSettingsChanged = onWebSettingsChanged;
        _onFilterTriggersChanged = onFilterTriggersChanged;
        _onRequestFaviconGrant = onRequestFaviconGrant;
        _onFaviconGranted = onFaviconGranted;
        _onAliasList = onAliasList;
        _onAliasDelete = onAliasDelete;
        _commandPipe = commandPipe;
        _onCommandsChanged = onCommandsChanged;

        // 读失败（文件被锁/ACL 拒绝）带默认值打开设置页：Load 在持续 IO 失败时会
        // 上抛，不接住的话构造器在 UI 线程炸掉、设置窗无声打不开。Save 前会重新
        // Load 并守卫，文件恢复前保存会被拦下，不会用这里的默认值覆盖好文件。
        Settings settings;
        try
        {
            settings = store.Load();
        }
        catch (Exception)
        {
            settings = Settings.Default;
            StatusMessage = "设置文件暂时读不到，当前显示默认值";
        }
        _autoStartEnabled = settings.AutoStart;
        _hotkeyMode = settings.HotkeyMode;
        _comboHotkey = settings.ComboHotkey;
        _excludedPaths = settings.ExcludedPaths.ToList();
        _historyEnabled = settings.HistoryEnabled;
        _pinyinEnabled = settings.PinyinEnabled;
        _currentDirectorySearchEnabled = settings.CurrentDirectorySearchEnabled;
        _explorerHostIntegrationEnabled = settings.ExplorerHostIntegrationEnabled;
        _directoryOpusHostIntegrationEnabled = settings.DirectoryOpusHostIntegrationEnabled;
        _zipProgram = settings.ZipProgram;
        _suggestionsEnabled = settings.SuggestionsEnabled;
        _stagingCapacityText = settings.StagingCapacity.ToString();
        _stagingAddHotkey = settings.StagingAddHotkey ?? "";
        DataDir = store.DataDir;

        Engines = new ObservableCollection<WebEngineEditItem>(
            (settings.WebEngines.Count > 0 ? settings.WebEngines : Settings.DefaultEngines())
            .Select(e => new WebEngineEditItem(e)));

        FilterTriggers = new ObservableCollection<FilterTriggerEditItem>(
            (settings.FilterTriggers.Count > 0 ? settings.FilterTriggers : Settings.DefaultFilterTriggers())
            .Select(t => new FilterTriggerEditItem(t)));

        // 第一轮 bug 修复：默认没有任何行——用户点「添加动作」逐个加、自选动作
        // 与组合键；不再预列全部 15 个动作。已有绑定（旧设置）按行恢复。
        ActionHotkeys = new ObservableCollection<ActionHotkeyEditItem>(
            ActionHotkeyCatalog.Entries
                .Where(e => !string.IsNullOrWhiteSpace(settings.ActionHotkeys.GetValueOrDefault(e.Id)))
                .Select(e => new ActionHotkeyEditItem(e.Id, settings.ActionHotkeys[e.Id])));
        // 行上换选动作也要刷新各行的可选项（复审 E：只在增删行时刷新会让
        // 旧行的下拉还列着别行已占用的动作，可选出重复 Id）。
        foreach (var row in ActionHotkeys)
            HookRowIdChanges(row);
        RefreshActionChoices();

        AddEngineCommand = new RelayCommand(_ => AddEngine());
        RemoveEngineCommand = new RelayCommand(_ => RemoveSelectedEngine(), _ => SelectedEngine is not null);
        ResetEnginesCommand = new RelayCommand(_ => ResetEngines());
        AddFilterTriggerCommand = new RelayCommand(_ => AddFilterTrigger());
        RemoveFilterTriggerCommand = new RelayCommand(_ => RemoveSelectedFilterTrigger(), _ => SelectedFilterTrigger is not null);
        ResetFilterTriggersCommand = new RelayCommand(_ => ResetFilterTriggers());
        SaveCommand = new RelayCommand(_ => Save());
        ClearHistoryCommand = new RelayCommand(_ => _ = ClearHistoryAsync());
        RemoveAliasCommand = new RelayCommand(p => _ = RemoveAliasAsync(p as AliasEntry), _ => AliasEntries.Count > 0);
        AddActionHotkeyCommand = new RelayCommand(_ => AddActionHotkey());
        RemoveActionHotkeyCommand = new RelayCommand(
            p => RemoveActionHotkey(p as ActionHotkeyEditItem),
            _ => ActionHotkeys.Count > 0);
        // K3 §4.7：命令 tab 操作
        AddCommandCommand = new RelayCommand(_ => AddCommand());
        RemoveCommandCommand = new RelayCommand(_ => RemoveSelectedCommand(),
            _ => SelectedCommand is not null && !SelectedCommand.IsBuiltin);
        SaveCommandCommand = new RelayCommand(_ => _ = SaveCommandAsync(),
            _ => SelectedCommand is not null && !SelectedCommand.IsBuiltin);
        CommandPreviewCommand = new RelayCommand(_ => _ = PreviewCommandAsync(),
            _ => SelectedCommand is not null && _commandPipe is not null);
        ApplyTemplateCommand = new RelayCommand(p => ApplyTemplate(p as CommandTemplate));
        ExportCommandsCommand = new RelayCommand(_ => _ = ExportCommandsAsync(),
            _ => _commandPipe is not null);
        ImportCommandsCommand = new RelayCommand(_ => _ = ImportCommandsAsync(),
            _ => _commandPipe is not null);
        SelectTabCommand = new RelayCommand(p =>
        {
            if (p is int i) SelectedTab = i;
            else if (p is string s && int.TryParse(s, out var n)) SelectedTab = n;
        });
    }

    /// <summary>实际数据目录（安装目录\data 或 %LocalAppData%\Prism）。</summary>
    public string DataDir { get; }

    /// <summary>关于页版本号。</summary>
    public string Version =>
        typeof(SettingsViewModel).Assembly.GetName().Version?.ToString(3) ?? "0.1.0";

    public int SelectedTab
    {
        get => _selectedTab;
        set
        {
            if (_selectedTab == value) return;
            _selectedTab = value;
            OnPropertyChanged();
            OnPropertyChanged(nameof(IsGeneralTab));
            OnPropertyChanged(nameof(IsQuickAccessTab));
            OnPropertyChanged(nameof(IsWebTab));
            OnPropertyChanged(nameof(IsFiltersTab));
            OnPropertyChanged(nameof(IsAboutTab));
            OnPropertyChanged(nameof(IsCommandsTab));
            // K3 §4.7：切到命令 tab 时异步拉取命令目录
            if (value == TabIndex.Commands)
                _ = LoadCommandsAsync();
        }
    }

    public bool IsGeneralTab => SelectedTab == TabIndex.General;
    public bool IsQuickAccessTab => SelectedTab == TabIndex.QuickAccess;
    public bool IsWebTab => SelectedTab == TabIndex.Web;
    public bool IsFiltersTab => SelectedTab == TabIndex.Filters;
    public bool IsAboutTab => SelectedTab == TabIndex.About;
    public bool IsCommandsTab => SelectedTab == TabIndex.Commands;

    /// <summary>是否使用双击 Ctrl 呼出（与 IsComboMode 互斥）。</summary>
    public bool IsDoubleCtrlMode
    {
        get => _hotkeyMode == HotkeyMode.DoubleCtrl;
        set
        {
            if (!value || _hotkeyMode == HotkeyMode.DoubleCtrl) return;
            _hotkeyMode = HotkeyMode.DoubleCtrl;
            OnPropertyChanged();
            OnPropertyChanged(nameof(IsComboMode));
            OnPropertyChanged(nameof(IsComboEditorEnabled));
        }
    }

    /// <summary>是否使用自定义组合键呼出。</summary>
    public bool IsComboMode
    {
        get => _hotkeyMode == HotkeyMode.Combo;
        set
        {
            if (!value || _hotkeyMode == HotkeyMode.Combo) return;
            _hotkeyMode = HotkeyMode.Combo;
            OnPropertyChanged();
            OnPropertyChanged(nameof(IsDoubleCtrlMode));
            OnPropertyChanged(nameof(IsComboEditorEnabled));
        }
    }

    public bool IsComboEditorEnabled => IsComboMode;

    public string ComboHotkey
    {
        get => _comboHotkey;
        set
        {
            if (_comboHotkey == value) return;
            _comboHotkey = value ?? "";
            OnPropertyChanged();
        }
    }

    /// <summary>开机自启开关；切换时立即写注册表并落盘（与步骤 7 行为一致）。</summary>
    public bool AutoStart
    {
        get => _autoStartEnabled;
        set
        {
            if (_autoStartEnabled == value) return;
            try
            {
                _autoStart.SetEnabled(value);
            }
            catch (Exception ex)
            {
                StatusMessage = "自启设置失败：" + ex.Message;
                OnPropertyChanged(nameof(AutoStart));
                return;
            }

            _autoStartEnabled = value;
            // 只改磁盘上的 AutoStart，不覆盖用户尚未点保存的引擎/快捷键草稿。
            try
            {
                var disk = _store.Load();
                _store.Save(disk with { AutoStart = value });
            }
            catch (Exception ex)
            {
                StatusMessage = "自启已写入注册表，但设置文件保存失败：" + ex.Message;
                OnPropertyChanged();
                return;
            }

            StatusMessage = value ? "已开启开机自启" : "已关闭开机自启";
            OnPropertyChanged();
        }
    }

    public ObservableCollection<WebEngineEditItem> Engines { get; }

    /// <summary>触发词类型全集（与 FilterTriggerDetector.TryRewrite 分支同口径），
    /// 供类型列 ComboBox 与保存校验共用。</summary>
    public static readonly string[] FilterTypeOptions =
        { "ext", "path", "size", "dm", "dc", "file", "folder" };

    public ObservableCollection<FilterTriggerEditItem> FilterTriggers { get; }

    /// <summary>动作快捷键行集合：默认空，用户逐个添加（第一轮 bug 修复）。</summary>
    public ObservableCollection<ActionHotkeyEditItem> ActionHotkeys { get; }

    /// <summary>「添加动作」：追加一行并选中第一个未被占用的动作。</summary>
    public ICommand AddActionHotkeyCommand { get; }

    /// <summary>删除整行（连同该行动作选择；未保存前不影响现行绑定）。</summary>
    public ICommand RemoveActionHotkeyCommand { get; }

    private void AddActionHotkey()
    {
        var used = UsedActionIds(except: null);
        var next = ActionHotkeyCatalog.Entries.FirstOrDefault(e => !used.Contains(e.Id));
        if (next is null)
        {
            StatusMessage = $"最多 {ActionHotkeyCatalog.Entries.Count} 个动作，已全部添加";
            return;
        }
        var row = new ActionHotkeyEditItem(next.Id, "");
        HookRowIdChanges(row);
        ActionHotkeys.Add(row);
        RefreshActionChoices();
    }

    private void RemoveActionHotkey(ActionHotkeyEditItem? row)
    {
        if (row is null) return;
        row.PropertyChanged -= OnRowIdChanged;
        ActionHotkeys.Remove(row);
        RefreshActionChoices();
    }

    private void HookRowIdChanges(ActionHotkeyEditItem row) =>
        row.PropertyChanged += OnRowIdChanged;

    private void OnRowIdChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ActionHotkeyEditItem.Id))
            RefreshActionChoices();
    }

    private HashSet<string> UsedActionIds(ActionHotkeyEditItem? except) =>
        [.. ActionHotkeys.Where(r => !ReferenceEquals(r, except)).Select(r => r.Id)];

    /// <summary>刷新每行 ComboBox 的可选集：目录减去其他行已占用的动作。</summary>
    private void RefreshActionChoices()
    {
        foreach (var row in ActionHotkeys)
        {
            var others = UsedActionIds(except: row);
            row.AvailableActions = ActionHotkeyCatalog.Entries
                .Where(e => !others.Contains(e.Id))
                .ToList();
        }
    }

    public WebEngineEditItem? SelectedEngine
    {
        get => _selectedEngine;
        set
        {
            if (ReferenceEquals(_selectedEngine, value)) return;
            _selectedEngine = value;
            OnPropertyChanged();
            CommandManager.InvalidateRequerySuggested();
        }
    }

    public FilterTriggerEditItem? SelectedFilterTrigger
    {
        get => _selectedFilterTrigger;
        set
        {
            if (ReferenceEquals(_selectedFilterTrigger, value)) return;
            _selectedFilterTrigger = value;
            OnPropertyChanged();
            CommandManager.InvalidateRequerySuggested();
        }
    }

    public string StatusMessage
    {
        get => _statusMessage;
        private set
        {
            if (_statusMessage == value) return;
            _statusMessage = value;
            OnPropertyChanged();
        }
    }

    public ICommand AddEngineCommand { get; }
    public ICommand RemoveEngineCommand { get; }
    public ICommand ResetEnginesCommand { get; }
    public ICommand AddFilterTriggerCommand { get; }
    public ICommand RemoveFilterTriggerCommand { get; }
    public ICommand ResetFilterTriggersCommand { get; }
    public ICommand SaveCommand { get; }
    public ICommand SelectTabCommand { get; }
    public ICommand ClearHistoryCommand { get; }

    // K3 §4.7：命令 tab 的 ICommand 与属性
    public ICommand AddCommandCommand { get; }
    public ICommand RemoveCommandCommand { get; }
    public ICommand SaveCommandCommand { get; }
    public ICommand CommandPreviewCommand { get; }
    public ICommand ApplyTemplateCommand { get; }
    public ICommand ExportCommandsCommand { get; }
    public ICommand ImportCommandsCommand { get; }

    /// <summary>命令编辑列表（含内置只读展示 + 用户可编辑行）。</summary>
    public ObservableCollection<CommandEditItem> Commands
    {
        get => _commands;
        private set { _commands = value; OnPropertyChanged(); }
    }

    private CommandEditItem? _selectedCommand;
    public CommandEditItem? SelectedCommand
    {
        get => _selectedCommand;
        set
        {
            if (_selectedCommand == value) return;
            _selectedCommand = value;
            OnPropertyChanged();
            // 选中命令变化时清空预览/冲突状态
            CommandPreviewText = "";
            CommandConflictText = "";
        }
    }

    /// <summary>命令 tab 实时预览文本。</summary>
    public string CommandPreviewText
    {
        get => _commandPreviewText;
        set { if (_commandPreviewText != value) { _commandPreviewText = value; OnPropertyChanged(); } }
    }

    /// <summary>命令 tab 命名空间冲突提示。</summary>
    public string CommandConflictText
    {
        get => _commandConflictText;
        set { if (_commandConflictText != value) { _commandConflictText = value; OnPropertyChanged(); } }
    }

    /// <summary>命令 tab 通用状态提示。</summary>
    public string CommandStatusText
    {
        get => _commandStatusText;
        set { if (_commandStatusText != value) { _commandStatusText = value; OnPropertyChanged(); } }
    }

    /// <summary>命令功能是否可用（broker 未连接/未协商时为 false）。</summary>
    public bool CommandsAvailable => _commandPipe is not null;

    /// <summary>别名系统：设置页展示的绑定列表（打开时拉取）。</summary>
    public ObservableCollection<AliasEntry> AliasEntries { get; } = new();

    /// <summary>空列表提示的可见性（有绑定时隐藏）。</summary>
    public bool HasNoAliases => AliasEntries.Count == 0;

    public ICommand RemoveAliasCommand { get; }

    /// <summary>设置页打开时拉取别名列表（失败静默——后端未连接时列表为空）。</summary>
    public async Task LoadAliasesAsync()
    {
        if (_onAliasList is null)
        {
            OnPropertyChanged(nameof(HasNoAliases));
            return;
        }
        try
        {
            var entries = await _onAliasList().ConfigureAwait(true);
            AliasEntries.Clear();
            // 按 kind 序（应用→文件夹→文件）填充，分组内也按此序。
            foreach (var entry in entries.OrderBy(e => KindOrder(e.Target.Kind)))
                AliasEntries.Add(entry);
            // 一次性挂分组视图（防重复添加）；删除经 ObservableCollection 通知自动更新组。
            var view = CollectionViewSource.GetDefaultView(AliasEntries);
            if (view is not null && view.GroupDescriptions.OfType<PropertyGroupDescription>()
                .All(g => g.PropertyName != "KindLabel"))
            {
                view.GroupDescriptions.Add(new PropertyGroupDescription("KindLabel"));
            }
        }
        catch
        {
            // 后端未连接：列表留空，删除操作会给出错误提示。
        }
        OnPropertyChanged(nameof(HasNoAliases));
    }

    /// <summary>别名分组排序键：应用 0、文件夹 1、文件 2。</summary>
    private static int KindOrder(string kind) => kind switch
    {
        "application" => 0,
        "directory" => 1,
        _ => 2,
    };

    private async Task RemoveAliasAsync(AliasEntry? entry)
    {
        if (entry is null) return;
        if (_onAliasDelete is null)
        {
            StatusMessage = "后端未连接，无法删除别名";
            return;
        }
        try
        {
            await _onAliasDelete(entry.Target).ConfigureAwait(true);
            AliasEntries.Remove(entry);
            OnPropertyChanged(nameof(HasNoAliases));
            StatusMessage = "别名已删除";
        }
        catch (Exception ex)
        {
            StatusMessage = "删除别名失败：" + ex.Message;
        }
    }

    public bool HistoryEnabled
    {
        get => _historyEnabled;
        set
        {
            if (_historyEnabled == value) return;
            _historyEnabled = value;
            OnPropertyChanged();
        }
    }

    public bool PinyinEnabled
    {
        get => _pinyinEnabled;
        set
        {
            if (_pinyinEnabled == value) return;
            _pinyinEnabled = value;
            OnPropertyChanged();
        }
    }

    /// <summary>当前目录搜索总开关（G4）；保存后由 App 同步给范围状态机。</summary>
    public bool CurrentDirectorySearchEnabled
    {
        get => _currentDirectorySearchEnabled;
        set
        {
            if (_currentDirectorySearchEnabled == value) return;
            _currentDirectorySearchEnabled = value;
            OnPropertyChanged();
        }
    }

    /// <summary>Explorer 宿主联动（实验性，默认关；需兼容矩阵验收后才建议打开）。</summary>
    public bool ExplorerHostIntegrationEnabled
    {
        get => _explorerHostIntegrationEnabled;
        set
        {
            if (_explorerHostIntegrationEnabled == value) return;
            _explorerHostIntegrationEnabled = value;
            OnPropertyChanged();
        }
    }

    /// <summary>Directory Opus 宿主联动（实验性，默认关）。</summary>
    public bool DirectoryOpusHostIntegrationEnabled
    {
        get => _directoryOpusHostIntegrationEnabled;
        set
        {
            if (_directoryOpusHostIntegrationEnabled == value) return;
            _directoryOpusHostIntegrationEnabled = value;
            OnPropertyChanged();
        }
    }

    /// <summary>
    /// 自定义压缩程序可执行文件完整路径（G6 ZIP adapter）。
    /// 留空则自动探测 7-Zip，再回退到 Windows 内置 Shell 压缩。
    /// </summary>
    public string? ZipProgram
    {
        get => _zipProgram;
        set
        {
            if (_zipProgram == value) return;
            _zipProgram = string.IsNullOrWhiteSpace(value) ? null : value;
            OnPropertyChanged();
        }
    }

    /// <summary>G8 在线联想开关：默认关闭，只对内置引擎（Bing/百度/Google）生效。</summary>
    public bool SuggestionsEnabled
    {
        get => _suggestionsEnabled;
        set
        {
            if (_suggestionsEnabled == value) return;
            _suggestionsEnabled = value;
            OnPropertyChanged();
        }
    }

    /// <summary>暂存区容量（1..32，文本编辑；保存时解析，非法则拦在落盘前）。</summary>
    public string StagingCapacityText
    {
        get => _stagingCapacityText;
        set
        {
            if (_stagingCapacityText == value) return;
            _stagingCapacityText = value ?? "";
            OnPropertyChanged();
        }
    }

    /// <summary>「加入暂存区」组合键；空 = 禁用。HotkeyRecorderBox 直写。</summary>
    public string StagingAddHotkey
    {
        get => _stagingAddHotkey;
        set
        {
            if (_stagingAddHotkey == value) return;
            _stagingAddHotkey = value ?? "";
            OnPropertyChanged();
        }
    }

    private void AddEngine()
    {
        var item = new WebEngineEditItem
        {
            Keyword = "",
            Name = "新引擎",
            UrlTemplate = "https://example.com/search?q={q}",
        };
        Engines.Add(item);
        SelectedEngine = item;
        StatusMessage = "已添加一行，请填写关键词与 URL（需含 {q}）后点保存";
    }

    private void RemoveSelectedEngine()
    {
        if (SelectedEngine is null) return;
        var idx = Engines.IndexOf(SelectedEngine);
        Engines.Remove(SelectedEngine);
        SelectedEngine = Engines.Count == 0
            ? null
            : Engines[Math.Clamp(idx, 0, Engines.Count - 1)];
        StatusMessage = "已移除，点保存后生效";
    }

    private void ResetEngines()
    {
        Engines.Clear();
        foreach (var e in Settings.DefaultEngines())
            Engines.Add(new WebEngineEditItem(e));
        SelectedEngine = Engines.FirstOrDefault();
        StatusMessage = "已恢复预设（bi / b / g），点保存后生效";
    }

    private void AddFilterTrigger()
    {
        var item = new FilterTriggerEditItem
        {
            Keyword = "",
            FilterType = "ext",
            Description = "新触发词",
        };
        FilterTriggers.Add(item);
        SelectedFilterTrigger = item;
        StatusMessage = "已添加一行，请填写关键词后点保存";
    }

    private void RemoveSelectedFilterTrigger()
    {
        if (SelectedFilterTrigger is null) return;
        var idx = FilterTriggers.IndexOf(SelectedFilterTrigger);
        FilterTriggers.Remove(SelectedFilterTrigger);
        SelectedFilterTrigger = FilterTriggers.Count == 0
            ? null
            : FilterTriggers[Math.Clamp(idx, 0, FilterTriggers.Count - 1)];
        StatusMessage = "已移除，点保存后生效";
    }

    private void ResetFilterTriggers()
    {
        FilterTriggers.Clear();
        foreach (var t in Settings.DefaultFilterTriggers())
            FilterTriggers.Add(new FilterTriggerEditItem(t));
        SelectedFilterTrigger = FilterTriggers.FirstOrDefault();
        StatusMessage = "已恢复预设（tz / pp），点保存后生效";
    }

    private void Save()
    {
        if (_hotkeyMode == HotkeyMode.Combo && string.IsNullOrWhiteSpace(ComboHotkey))
        {
            StatusMessage = "请录制一个组合键，或改回双击 Ctrl";
            SelectedTab = TabIndex.QuickAccess;
            return;
        }

        // 动作快捷键（2026-08-21 设想）：先逐行规范化，再整表校验（保留键/撞键）。
        // 复审 E：行 Id 为空（下拉正在换选）或两行撞同一动作时明确报错，
        // 不做静默丢绑（旧写字典键覆盖会无声吞掉先到的那行）。
        var actionHotkeys = new Dictionary<string, string>();
        foreach (var row in ActionHotkeys)
        {
            if (string.IsNullOrWhiteSpace(row.Id))
            {
                StatusMessage = "有未选择动作的快捷键行，请选好动作或删除该行";
                SelectedTab = TabIndex.QuickAccess;
                return;
            }
            if (actionHotkeys.ContainsKey(row.Id))
            {
                StatusMessage = $"「{row.Label}」被添加了两次，请删除多余的一行";
                SelectedTab = TabIndex.QuickAccess;
                return;
            }
            var raw = row.Value.Trim();
            if (raw.Length == 0) continue;
            var canonical = ActionHotkeyTable.Canonicalize(raw);
            if (canonical is null)
            {
                StatusMessage = $"「{row.Label}」的组合键无法识别，请重新录制";
                SelectedTab = TabIndex.QuickAccess;
                return;
            }
            actionHotkeys[row.Id] = canonical;
        }
        var hotkeyError = ActionHotkeyTable.ValidateBindings(actionHotkeys);
        if (hotkeyError is not null)
        {
            StatusMessage = "动作快捷键：" + hotkeyError;
            SelectedTab = TabIndex.QuickAccess;
            return;
        }

        // 暂存区（2026-08-22 计划）：容量解析 + 快捷键规范化，非法拦在落盘之前。
        if (!int.TryParse(StagingCapacityText.Trim(), out var stagingCapacity)
            || stagingCapacity is < 1 or > 32)
        {
            StatusMessage = "暂存区容量需为 1–32 的整数";
            SelectedTab = TabIndex.QuickAccess;
            return;
        }
        var stagingHotkey = StagingAddHotkey.Trim();
        if (stagingHotkey.Length > 0)
        {
            var canonical = ActionHotkeyTable.Canonicalize(stagingHotkey);
            if (canonical is null)
            {
                StatusMessage = "「加入暂存区」的组合键无法识别，请重新录制";
                SelectedTab = TabIndex.QuickAccess;
                return;
            }
            // SettingsStore.Validate 侧再做保留键/撞键检查（与动作快捷键双侧校验同纪律）。
            stagingHotkey = canonical;
        }

        // G8: 保存前的旧设置，用于检测自定义引擎 origin 变化。
        // 读不到磁盘真值就放弃保存——拿默认值当 prevSettings 会把用户已有的
        // 引擎/快捷键/授权全部覆盖掉（设置文件被短暂锁住即触发）。
        Settings prevSettings;
        try
        {
            prevSettings = _store.Load();
        }
        catch (Exception ex)
        {
            StatusMessage = "保存失败：无法读取当前设置文件（" + ex.Message + "）";
            return;
        }

        var engines = new List<WebEngine>();
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var row in Engines)
        {
            var eng = row.ToEngine();
            if (string.IsNullOrEmpty(eng.Keyword))
            {
                StatusMessage = "引擎关键词不能为空";
                SelectedTab = TabIndex.Web;
                SelectedEngine = row;
                return;
            }
            if (eng.Keyword.Any(char.IsWhiteSpace))
            {
                StatusMessage = $"关键词「{eng.Keyword}」不能含空格";
                SelectedTab = TabIndex.Web;
                SelectedEngine = row;
                return;
            }
            if (string.IsNullOrEmpty(eng.Name))
            {
                StatusMessage = "引擎显示名不能为空";
                SelectedTab = TabIndex.Web;
                SelectedEngine = row;
                return;
            }
            if (string.IsNullOrEmpty(eng.UrlTemplate) || !eng.UrlTemplate.Contains("{q}", StringComparison.Ordinal))
            {
                StatusMessage = $"引擎「{eng.Name}」的 URL 必须包含 {{q}} 占位符";
                SelectedTab = TabIndex.Web;
                SelectedEngine = row;
                return;
            }
            if (!seen.Add(eng.Keyword))
            {
                StatusMessage = $"关键词「{eng.Keyword}」重复";
                SelectedTab = TabIndex.Web;
                SelectedEngine = row;
                return;
            }
            engines.Add(eng);
        }

        if (engines.Count == 0)
        {
            StatusMessage = "至少保留一个网页搜索引擎";
            SelectedTab = TabIndex.Web;
            return;
        }

        var filterTriggers = new List<FilterTrigger>();
        var seenTriggers = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var row in FilterTriggers)
        {
            var trig = row.ToTrigger();
            if (string.IsNullOrEmpty(trig.Keyword))
            {
                StatusMessage = "过滤触发词关键词不能为空";
                SelectedTab = TabIndex.Filters;
                SelectedFilterTrigger = row;
                return;
            }
            if (trig.Keyword.Any(char.IsWhiteSpace))
            {
                StatusMessage = $"关键词「{trig.Keyword}」不能含空格";
                SelectedTab = TabIndex.Filters;
                SelectedFilterTrigger = row;
                return;
            }
            if (!FilterTypeOptions.Contains(trig.FilterType, StringComparer.Ordinal))
            {
                StatusMessage = $"触发词「{trig.Keyword}」的类型必须是 {string.Join(" / ", FilterTypeOptions)}";
                SelectedTab = TabIndex.Filters;
                SelectedFilterTrigger = row;
                return;
            }
            if (!seenTriggers.Add(trig.Keyword))
            {
                StatusMessage = $"关键词「{trig.Keyword}」重复";
                SelectedTab = TabIndex.Filters;
                SelectedFilterTrigger = row;
                return;
            }
            filterTriggers.Add(trig);
        }

        var next = new Settings
        {
            SchemaVersion = Settings.CurrentSchemaVersion,
            HotkeyMode = _hotkeyMode,
            ComboHotkey = ComboHotkey.Trim(),
            AutoStart = _autoStartEnabled,
            WebEngines = engines,
            FilterTriggers = filterTriggers,
            ExcludedPaths = _excludedPaths,
            HistoryEnabled = HistoryEnabled,
            PinyinEnabled = PinyinEnabled,
            CurrentDirectorySearchEnabled = CurrentDirectorySearchEnabled,
            ExplorerHostIntegrationEnabled = ExplorerHostIntegrationEnabled,
            DirectoryOpusHostIntegrationEnabled = DirectoryOpusHostIntegrationEnabled,
            ZipProgram = string.IsNullOrWhiteSpace(ZipProgram) ? null : ZipProgram,
            SuggestionsEnabled = _suggestionsEnabled,
            FaviconGrants = prevSettings.FaviconGrants,
            ActionHotkeys = actionHotkeys,
            StagingCapacity = stagingCapacity,
            StagingAddHotkey = stagingHotkey,
        };

        try
        {
            _store.Save(next);
        }
        catch (Exception ex)
        {
            StatusMessage = "保存失败：" + ex.Message;
            return;
        }

        try
        {
            _onApplied?.Invoke(next);
        }
        catch (Exception ex)
        {
            StatusMessage = "快捷键应用失败：" + ex.Message;
            return;
        }

        if (_onEnginesChanged is not null || _onPreferencesChanged is not null)
            _ = ApplyBackendAsync(engines, next.HistoryEnabled, next.PinyinEnabled);
        else
            StatusMessage = "已保存";

        // G8: 同步联想开关和引擎列表给 SearchViewModel（无论后端是否连接）。
        _onWebSettingsChanged?.Invoke(engines, _suggestionsEnabled);

        // 同步过滤触发词给 SearchViewModel（纯前端消费，无后端热重载）。
        _onFilterTriggersChanged?.Invoke(filterTriggers);

        // G8: 检查自定义引擎的新 origin，弹出 favicon 联网授权。
        if (_onRequestFaviconGrant is not null)
            RequestNewFaviconGrants(engines, prevSettings);
    }

    private async Task ApplyBackendAsync(
        List<WebEngine> engines,
        bool historyEnabled,
        bool pinyinEnabled)
    {
        try
        {
            if (_onEnginesChanged is not null)
                await _onEnginesChanged(engines).ConfigureAwait(true);
            if (_onPreferencesChanged is not null)
                await _onPreferencesChanged(historyEnabled, pinyinEnabled).ConfigureAwait(true);
            StatusMessage = "已保存并生效";
        }
        catch (Exception ex)
        {
            // 设置已落盘；后端未热重载时提示，重启后端或下次启动也会读到新配置。
            StatusMessage = "已保存设置；后端未刷新：" + ex.Message;
        }
    }

    /// <summary>
    /// G8：检查自定义引擎中是否有新 origin 需要单独征求 favicon 联网许可。
    /// 只对非内置引擎（WebModeDetector 不识别的 Name）触发，且只对磁盘上未授权的 origin 弹窗。
    /// 用户拒绝或失败时回退通用图标，不阻塞保存。
    /// </summary>
    private void RequestNewFaviconGrants(List<WebEngine> engines, Settings prevSettings)
    {
        var prevGrants = prevSettings.FaviconGrants;
        foreach (var eng in engines)
        {
            // 内置引擎图标随程序打包，不需要 favicon 授权。
            if (WebModeDetector.IsBuiltIn(eng))
                continue;

            var origin = FaviconCache.NormalizeOrigin(eng.UrlTemplate);
            if (origin is null)
                continue;

            // 已授权的 origin 不重复弹窗。
            if (prevGrants.ContainsKey(origin))
                continue;

            // 弹出授权对话框（由 App 提供 _onRequestFaviconGrant 回调）。
            var granted = _onRequestFaviconGrant!(origin);
            if (!granted)
                continue;

            // 授权成功：更新内存中的 FaviconGrants，并落盘。
            // 读不到磁盘真值就不落盘——拿默认值当底会把已授权列表整个覆盖掉。
            Settings disk;
            try
            {
                disk = _store.Load();
            }
            catch (Exception ex)
            {
                StatusMessage = "授权未保存（设置文件暂时读不到：" + ex.Message + "），稍后重新授权即可";
                break;
            }
            var grants = new Dictionary<string, FaviconGrant>(disk.FaviconGrants)
            {
                [origin] = new FaviconGrant(origin, DateTimeOffset.UtcNow.ToString("o")),
            };
            try
            {
                _store.Save(disk with { FaviconGrants = grants });
            }
            catch (Exception ex)
            {
                StatusMessage = "授权写盘失败：" + ex.Message;
                break;
            }

            // 触发下载：缓存落盘 + 图标内存缓存失效后，搜索结果下一次装饰即换上真图标。
            _onFaviconGranted?.Invoke(origin);
        }
    }

    private async Task ClearHistoryAsync()
    {
        if (_onClearHistory is null)
        {
            StatusMessage = "后端未连接，无法清除历史";
            return;
        }
        try
        {
            await _onClearHistory().ConfigureAwait(true);
            StatusMessage = "使用历史已清除";
        }
        catch (Exception ex)
        {
            StatusMessage = "清除历史失败：" + ex.Message;
        }
    }

    // ── K3 §4.7 命令 tab 方法 ──────────────────────────────────────────

    /// <summary>拉取命令目录并填充编辑列表。</summary>
    public async Task LoadCommandsAsync()
    {
        if (_commandPipe is null)
        {
            CommandStatusText = "命令功能不可用（后端未连接）";
            return;
        }
        CommandStatusText = "加载中…";
        try
        {
            var result = await _commandPipe.CommandListAsync().ConfigureAwait(true);
            if (result is null)
            {
                CommandStatusText = "命令功能不可用";
                return;
            }
            _commandCatalog = result.Value.Items;
            Commands = new ObservableCollection<CommandEditItem>(
                result.Value.Items.Select(d => new CommandEditItem(d)));
            SelectedCommand = Commands.FirstOrDefault();
            CommandStatusText = Commands.Count > 0 ? "" : "暂无命令，点「新建命令」添加";
        }
        catch (Exception ex)
        {
            CommandStatusText = "加载失败：" + ex.Message;
        }
    }

    private void AddCommand()
    {
        // K3: broker 不分配 id——validate_command_id 拒绝空串。新建时立即生成
        // user.<guid> 形式的唯一 id（小写无连字符，满足 [a-z0-9._-] 语法）。
        var suffix = Guid.NewGuid().ToString("N");
        var item = new CommandEditItem { Id = "user." + suffix, Handler = "open_url" };
        Commands.Add(item);
        SelectedCommand = item;
        CommandStatusText = "编辑后点「保存到 broker」生效";
    }

    private void RemoveSelectedCommand()
    {
        if (SelectedCommand is null || SelectedCommand.IsBuiltin) return;
        if (!string.IsNullOrEmpty(SelectedCommand.Id) && _commandPipe is not null)
            _ = DeleteCommandAsync(SelectedCommand.Id);
        Commands.Remove(SelectedCommand);
        SelectedCommand = Commands.FirstOrDefault();
    }

    private async Task SaveCommandAsync()
    {
        if (SelectedCommand is null || SelectedCommand.IsBuiltin) return;
        if (string.IsNullOrWhiteSpace(SelectedCommand.Title))
        {
            CommandStatusText = "标题不能为空";
            return;
        }
        if (_commandPipe is null)
        {
            CommandStatusText = "命令功能不可用";
            return;
        }

        // §4.5：保存前校验触发词命名空间冲突。Trigger 与关键字都查——broker 侧
        // 逐个关键字查（keywords 恒占命名空间），且 Trigger 留空时 ToDefinition
        // 会回退用首关键字做路由触发词，只查 Trigger 会漏掉关键字冲突。
        var excludeId = string.IsNullOrEmpty(SelectedCommand.Id) ? null : SelectedCommand.Id;
        var candidates = new List<string>();
        var trigger = SelectedCommand.Trigger.Trim();
        if (trigger.Length > 0) candidates.Add(trigger);
        foreach (var kw in SelectedCommand.KeywordsText.Split(','))
        {
            var t = kw.Trim();
            if (t.Length > 0 && !candidates.Contains(t, StringComparer.OrdinalIgnoreCase))
                candidates.Add(t);
        }
        foreach (var candidate in candidates)
        {
            var ns = await _commandPipe.ValidateTriggerNamespaceAsync(
                candidate, "command", excludeId).ConfigureAwait(true);
            if (ns is { Ok: false })
            {
                CommandConflictText = ns.Conflict is { } c
                    ? $"冲突：{c.Kind}（{c.OwnerLabel}）"
                    : "冲突";
                CommandStatusText = "保存被拒：触发词冲突";
                return;
            }
        }
        CommandConflictText = "";

        CommandStatusText = "保存中…";
        try
        {
            var def = SelectedCommand.ToDefinition();
            var msg = await _commandPipe.CommandSetAsync(def).ConfigureAwait(true);
            if (!string.IsNullOrEmpty(msg))
            {
                CommandStatusText = "保存失败：" + msg;
                return;
            }

            // 快捷键绑定
            if (!string.IsNullOrWhiteSpace(SelectedCommand.ShortcutCombo))
            {
                var scMsg = await _commandPipe.SetCommandShortcutAsync(
                    def.Id, SelectedCommand.ShortcutCombo.Trim()).ConfigureAwait(true);
                if (!string.IsNullOrEmpty(scMsg))
                    CommandStatusText = "命令已保存，快捷键失败：" + scMsg;
                else
                    CommandStatusText = "已保存";
            }
            else
            {
                CommandStatusText = "已保存";
            }

            // 刷新目录
            await LoadCommandsAsync().ConfigureAwait(true);
            // 通知搜索 VM 刷新共享 CommandCatalog，否则关键字路由看不到新命令
            if (_onCommandsChanged is not null)
                await _onCommandsChanged().ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            CommandStatusText = "保存失败：" + ex.Message;
        }
    }

    private async Task DeleteCommandAsync(string commandId)
    {
        if (_commandPipe is null) return;
        try
        {
            await _commandPipe.CommandDeleteAsync(commandId).ConfigureAwait(true);
            if (_onCommandsChanged is not null)
                await _onCommandsChanged().ConfigureAwait(true);
        }
        catch { /* 幂等，静默 */ }
    }

    private async Task PreviewCommandAsync()
    {
        if (SelectedCommand is null || _commandPipe is null) return;
        CommandPreviewText = "预览中…";
        try
        {
            // 用临时 id 发预览——如果是新建命令（id 空），需要先让 broker 知道。
            // broker 的 CommandPreview 基于 command_id 查目录。新建未保存的命令
            // 无法预览（broker 没有 id）。编辑已存在的命令可以直接预览。
            if (string.IsNullOrEmpty(SelectedCommand.Id))
            {
                CommandPreviewText = "请先保存命令再预览";
                return;
            }
            var result = await _commandPipe.CommandPreviewAsync(
                SelectedCommand.Id, "root", "test").ConfigureAwait(true);
            if (result is null)
            {
                CommandPreviewText = "预览不可用";
                return;
            }
            if (!result.Ok)
            {
                CommandPreviewText = "预览失败：" + result.Message;
                return;
            }
            if (result.Url is not null)
                CommandPreviewText = "URL: " + result.Url;
            else if (result.ProgramPath is not null)
                CommandPreviewText = "程序: " + result.ProgramPath +
                    (result.ProgramArgs.Count > 0
                        ? "\n参数: " + string.Join(" ", result.ProgramArgs)
                        : "") +
                    (result.ProgramWorkingDir is not null
                        ? "\n工作目录: " + result.ProgramWorkingDir
                        : "");
            else
                CommandPreviewText = "预览完成（无输出）";
        }
        catch (Exception ex)
        {
            CommandPreviewText = "预览失败：" + ex.Message;
        }
    }

    private void ApplyTemplate(CommandTemplate? template)
    {
        if (template is null || SelectedCommand is null) return;
        SelectedCommand.Handler = template.Handler;
        SelectedCommand.Title = template.Title;
        SelectedCommand.UrlTemplate = template.UrlTemplate;
        SelectedCommand.ProgramPath = template.ProgramPath;
        SelectedCommand.ArgsTemplate = template.ArgsTemplate;
        SelectedCommand.WorkingDir = template.WorkingDir;
        SelectedCommand.KeywordsText = template.KeywordsText;
        SelectedCommand.IconGlyph = template.IconGlyph;
        // 触发 PropertyChanged 让 UI 刷新 handler 可见区域
        OnPropertyChanged(nameof(SelectedCommand));
    }

    /// <summary>§4.7 模板预设 6–8 个，预填表单不绕过校验。</summary>
    public static IReadOnlyList<CommandTemplate> CommandTemplates { get; } = new[]
    {
        new CommandTemplate("Google 搜索", "open_url", "Google 搜索",
            "https://www.google.com/search?q={query}", "", "", "", "g", "&#xE721;"),
        new CommandTemplate("百度搜索", "open_url", "百度搜索",
            "https://www.baidu.com/s?wd={query}", "", "", "", "bd", "&#xE721;"),
        new CommandTemplate("GitHub 仓库搜索", "open_url", "GitHub 搜索",
            "https://github.com/search?q={query}&type=repositories", "", "", "", "gh", "&#xE721;"),
        new CommandTemplate("用记事本打开", "launch_program", "用记事本打开",
            "", "C:\\Windows\\System32\\notepad.exe", "{selection.target}", "", "np", "&#xE70F;"),
        new CommandTemplate("用 VS Code 打开", "launch_program", "用 VS Code 打开",
            "", "C:\\Program Files\\Microsoft VS Code\\Code.exe", "{selection.target}", "", "code", "&#xE70F;"),
        new CommandTemplate("浏览器打开本地 HTML", "open_url", "打开本地 HTML",
            "file:///{query}", "", "", "", "html", "&#xE774;"),
    };

    // ── K3 §4.8 导入导出 ──────────────────────────────────────────────

    /// <summary>导出用户命令到 JSON 文件。</summary>
    private async Task ExportCommandsAsync()
    {
        if (_commandPipe is null)
        {
            CommandStatusText = "命令功能不可用";
            return;
        }
        CommandStatusText = "导出中…";
        try
        {
            var result = await _commandPipe.CommandExportAsync().ConfigureAwait(true);
            if (result is null)
            {
                CommandStatusText = "导出失败：后端不可用";
                return;
            }
            var (commands, exportedAt) = result.Value;

            var dlg = new Microsoft.Win32.SaveFileDialog
            {
                Filter = "Prism 命令导出 (*.json)|*.json",
                FileName = "prism-commands.json",
            };
            if (dlg.ShowDialog() != true) { CommandStatusText = ""; return; }

            var envelope = new
            {
                schema_version = 1,
                data = new { commands },
                exported_at = exportedAt,
            };
            var json = System.Text.Json.JsonSerializer.Serialize(envelope,
                new System.Text.Json.JsonSerializerOptions
                {
                    WriteIndented = true,
                    Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
                });
            await System.IO.File.WriteAllTextAsync(dlg.FileName, json).ConfigureAwait(true);
            CommandStatusText = $"已导出 {commands.Count} 条命令";
        }
        catch (Exception ex)
        {
            CommandStatusText = "导出失败：" + ex.Message;
        }
    }

    /// <summary>导入命令 JSON 文件。解析→展示清单→逐条 CommandSet(enabled=false)。</summary>
    private async Task ImportCommandsAsync()
    {
        if (_commandPipe is null)
        {
            CommandStatusText = "命令功能不可用";
            return;
        }

        var dlg = new Microsoft.Win32.OpenFileDialog
        {
            Filter = "Prism 命令导出 (*.json)|*.json",
        };
        if (dlg.ShowDialog() != true) { CommandStatusText = ""; return; }

        CommandStatusText = "导入中…";
        try
        {
            var json = await System.IO.File.ReadAllTextAsync(dlg.FileName).ConfigureAwait(true);
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var root = doc.RootElement;

            // 解析命令列表
            var importCommands = new List<UserCommandDefinition>();
            if (root.TryGetProperty("data", out var dataEl)
                && dataEl.TryGetProperty("commands", out var cmdsEl)
                && cmdsEl.ValueKind == System.Text.Json.JsonValueKind.Array)
            {
                foreach (var el in cmdsEl.EnumerateArray())
                {
                    if (el.ValueKind != System.Text.Json.JsonValueKind.Object) continue;
                    var cmd = ParseImportCommand(el);
                    if (cmd is not null) importCommands.Add(cmd);
                }
            }

            if (importCommands.Count == 0)
            {
                CommandStatusText = "导入文件无有效命令";
                return;
            }

            // 逐条 CommandSet，一律 enabled=false（§4.8 安全语义）
            int imported = 0;
            var errors = new List<string>();
            foreach (var cmd in importCommands)
            {
                cmd.Enabled = false;
                // 新 id 避免覆盖现有命令——加 user.imported. 前缀
                if (string.IsNullOrEmpty(cmd.Id) || !cmd.Id.StartsWith("user."))
                    cmd.Id = "user.imported." + (cmd.Id.Length > 0 ? cmd.Id : Guid.NewGuid().ToString("N"));
                try
                {
                    var msg = await _commandPipe.CommandSetAsync(cmd).ConfigureAwait(true);
                    if (!string.IsNullOrEmpty(msg))
                        errors.Add($"{cmd.Title}: {msg}");
                    else
                        imported++;
                }
                catch (Exception ex)
                {
                    errors.Add($"{cmd.Title}: {ex.Message}");
                }
            }

            await LoadCommandsAsync().ConfigureAwait(true);
            if (_onCommandsChanged is not null)
                await _onCommandsChanged().ConfigureAwait(true);
            if (errors.Count > 0)
                CommandStatusText = $"导入 {imported} 条，{errors.Count} 条失败：{errors[0]}";
            else
                CommandStatusText = $"已导入 {imported} 条命令（默认禁用，需手动启用）";
        }
        catch (Exception ex)
        {
            CommandStatusText = "导入失败：" + ex.Message;
        }
    }

    private static UserCommandDefinition? ParseImportCommand(System.Text.Json.JsonElement el)
    {
        try
        {
            var cmd = new UserCommandDefinition
            {
                Id = el.TryGetProperty("id", out var id) ? id.GetString() ?? "" : "",
                Title = el.TryGetProperty("title", out var t) ? t.GetString() ?? "" : "",
                Subtitle = el.TryGetProperty("subtitle", out var s) ? s.GetString() ?? "" : "",
                IconGlyph = el.TryGetProperty("icon_glyph", out var ig) ? ig.GetString() ?? "" : "",
                Danger = el.TryGetProperty("danger", out var d) ? d.GetString() ?? "normal" : "normal",
                Handler = el.TryGetProperty("handler", out var h) ? h.GetString() ?? "open_url" : "open_url",
                // K4a：导入文件可携带 fallback 标记。
                Fallback = el.TryGetProperty("fallback", out var fb) && fb.ValueKind == System.Text.Json.JsonValueKind.True,
            };

            if (el.TryGetProperty("keywords", out var kw) && kw.ValueKind == System.Text.Json.JsonValueKind.Array)
                foreach (var k in kw.EnumerateArray())
                    if (k.ValueKind == System.Text.Json.JsonValueKind.String)
                        cmd.Keywords.Add(k.GetString() ?? "");

            if (el.TryGetProperty("handler_params", out var hp) && hp.ValueKind == System.Text.Json.JsonValueKind.Object)
                foreach (var p in hp.EnumerateObject())
                    if (p.Value.ValueKind == System.Text.Json.JsonValueKind.String)
                        cmd.HandlerParams[p.Name] = p.Value.GetString() ?? "";

            return cmd;
        }
        catch { return null; }
    }

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

/// <summary>轻量 ICommand，避免引入额外 NuGet 依赖。</summary>
internal sealed class RelayCommand : ICommand
{
    private readonly Action<object?> _execute;
    private readonly Func<object?, bool>? _canExecute;

    public RelayCommand(Action<object?> execute, Func<object?, bool>? canExecute = null)
    {
        _execute = execute;
        _canExecute = canExecute;
    }

    public event EventHandler? CanExecuteChanged
    {
        add => CommandManager.RequerySuggested += value;
        remove => CommandManager.RequerySuggested -= value;
    }

    public bool CanExecute(object? parameter) => _canExecute?.Invoke(parameter) ?? true;
    public void Execute(object? parameter) => _execute(parameter);
}
