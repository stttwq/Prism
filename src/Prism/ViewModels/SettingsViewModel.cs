using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
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

    private bool _autoStartEnabled;
    private HotkeyMode _hotkeyMode;
    private string _comboHotkey;
    private string _statusMessage = "";
    private WebEngineEditItem? _selectedEngine;
    private int _selectedTab; // 0=常规 1=网页搜索 2=关于
    private readonly List<string> _excludedPaths;
    private bool _historyEnabled;
    private bool _pinyinEnabled;

    public event PropertyChangedEventHandler? PropertyChanged;

    public SettingsViewModel(
        SettingsStore store,
        AutoStartService autoStart,
        Action<Settings>? onApplied = null,
        Func<IReadOnlyList<WebEngine>, Task>? onEnginesChanged = null,
        Func<bool, bool, Task>? onPreferencesChanged = null,
        Func<Task>? onClearHistory = null)
    {
        _store = store;
        _autoStart = autoStart;
        _onApplied = onApplied;
        _onEnginesChanged = onEnginesChanged;
        _onPreferencesChanged = onPreferencesChanged;
        _onClearHistory = onClearHistory;

        var settings = store.Load();
        _autoStartEnabled = settings.AutoStart;
        _hotkeyMode = settings.HotkeyMode;
        _comboHotkey = settings.ComboHotkey;
        _excludedPaths = settings.ExcludedPaths.ToList();
        _historyEnabled = settings.HistoryEnabled;
        _pinyinEnabled = settings.PinyinEnabled;
        DataDir = store.DataDir;

        Engines = new ObservableCollection<WebEngineEditItem>(
            (settings.WebEngines.Count > 0 ? settings.WebEngines : Settings.DefaultEngines())
            .Select(e => new WebEngineEditItem(e)));

        AddEngineCommand = new RelayCommand(_ => AddEngine());
        RemoveEngineCommand = new RelayCommand(_ => RemoveSelectedEngine(), _ => SelectedEngine is not null);
        ResetEnginesCommand = new RelayCommand(_ => ResetEngines());
        SaveCommand = new RelayCommand(_ => Save());
        ClearHistoryCommand = new RelayCommand(_ => _ = ClearHistoryAsync());
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
            OnPropertyChanged(nameof(IsWebTab));
            OnPropertyChanged(nameof(IsAboutTab));
        }
    }

    public bool IsGeneralTab => SelectedTab == 0;
    public bool IsWebTab => SelectedTab == 1;
    public bool IsAboutTab => SelectedTab == 2;

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
    public ICommand SaveCommand { get; }
    public ICommand SelectTabCommand { get; }
    public ICommand ClearHistoryCommand { get; }

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

    private void Save()
    {
        if (_hotkeyMode == HotkeyMode.Combo && string.IsNullOrWhiteSpace(ComboHotkey))
        {
            StatusMessage = "请录制一个组合键，或改回双击 Ctrl";
            SelectedTab = 0;
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
                SelectedTab = 1;
                SelectedEngine = row;
                return;
            }
            if (eng.Keyword.Any(char.IsWhiteSpace))
            {
                StatusMessage = $"关键词「{eng.Keyword}」不能含空格";
                SelectedTab = 1;
                SelectedEngine = row;
                return;
            }
            if (string.IsNullOrEmpty(eng.Name))
            {
                StatusMessage = "引擎显示名不能为空";
                SelectedTab = 1;
                SelectedEngine = row;
                return;
            }
            if (string.IsNullOrEmpty(eng.UrlTemplate) || !eng.UrlTemplate.Contains("{q}", StringComparison.Ordinal))
            {
                StatusMessage = $"引擎「{eng.Name}」的 URL 必须包含 {{q}} 占位符";
                SelectedTab = 1;
                SelectedEngine = row;
                return;
            }
            if (!seen.Add(eng.Keyword))
            {
                StatusMessage = $"关键词「{eng.Keyword}」重复";
                SelectedTab = 1;
                SelectedEngine = row;
                return;
            }
            engines.Add(eng);
        }

        if (engines.Count == 0)
        {
            StatusMessage = "至少保留一个网页搜索引擎";
            SelectedTab = 1;
            return;
        }

        var next = new Settings
        {
            SchemaVersion = Settings.CurrentSchemaVersion,
            HotkeyMode = _hotkeyMode,
            ComboHotkey = ComboHotkey.Trim(),
            AutoStart = _autoStartEnabled,
            WebEngines = engines,
            ExcludedPaths = _excludedPaths,
            HistoryEnabled = HistoryEnabled,
            PinyinEnabled = PinyinEnabled,
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
