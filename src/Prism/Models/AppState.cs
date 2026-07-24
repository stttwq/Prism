using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace Prism.Models;

/// <summary>搜索窗口面板模式（frontend-spec.md §2）。</summary>
public enum PanelMode
{
    Idle,
    Results,
    Actions,
}

/// <summary>应用浅/深色（跟随系统）。</summary>
public enum AppTheme
{
    Light,
    Dark,
}

/// <summary>
/// 全局 UI 状态。Idle / Results / Actions 与深浅色主题。
/// 属性变更通知走 INotifyPropertyChanged（不引入 CommunityToolkit 依赖）。
/// </summary>
public sealed class AppState : INotifyPropertyChanged
{
    private PanelMode _mode = PanelMode.Idle;
    private string _query = "";
    private IReadOnlyList<SearchResult> _results = Array.Empty<SearchResult>();
    private int _selectedIndex;
    private SearchResult? _actionTarget;
    private IReadOnlyList<ActionItem> _actions = Array.Empty<ActionItem>();
    private int _selectedActionIndex;
    private bool _isPinned;
    private bool _isIndexing;
    private bool _isBackendConnected;
    private string _statusMessage = "";
    private AppTheme _theme = AppTheme.Light;

    public event PropertyChangedEventHandler? PropertyChanged;

    public PanelMode Mode
    {
        get => _mode;
        set => Set(ref _mode, value);
    }

    public string Query
    {
        get => _query;
        set => Set(ref _query, value);
    }

    public IReadOnlyList<SearchResult> Results
    {
        get => _results;
        set => Set(ref _results, value);
    }

    public int SelectedIndex
    {
        get => _selectedIndex;
        set => Set(ref _selectedIndex, value);
    }

    /// <summary>Actions 状态针对的文件/文件夹结果。</summary>
    public SearchResult? ActionTarget
    {
        get => _actionTarget;
        set => Set(ref _actionTarget, value);
    }

    public IReadOnlyList<ActionItem> Actions
    {
        get => _actions;
        set => Set(ref _actions, value);
    }

    public int SelectedActionIndex
    {
        get => _selectedActionIndex;
        set => Set(ref _selectedActionIndex, value);
    }

    public bool IsPinned
    {
        get => _isPinned;
        set => Set(ref _isPinned, value);
    }

    public bool IsIndexing
    {
        get => _isIndexing;
        set => Set(ref _isIndexing, value);
    }

    public bool IsBackendConnected
    {
        get => _isBackendConnected;
        set => Set(ref _isBackendConnected, value);
    }

    /// <summary>列表区单行提示（索引中 / 重连中 / 错误）。空串表示无提示。</summary>
    public string StatusMessage
    {
        get => _statusMessage;
        set => Set(ref _statusMessage, value);
    }

    /// <summary>仅由 ThemeWatcher 写入。</summary>
    public AppTheme Theme
    {
        get => _theme;
        set => Set(ref _theme, value);
    }

    public SearchResult? SelectedResult =>
        SelectedIndex >= 0 && SelectedIndex < Results.Count ? Results[SelectedIndex] : null;

    public ActionItem? SelectedAction =>
        SelectedActionIndex >= 0 && SelectedActionIndex < Actions.Count
            ? Actions[SelectedActionIndex]
            : null;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
        if (name is nameof(SelectedIndex) or nameof(Results))
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(SelectedResult)));
        if (name is nameof(SelectedActionIndex) or nameof(Actions))
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(SelectedAction)));
    }
}
