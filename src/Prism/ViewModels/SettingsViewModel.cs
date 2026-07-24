using System.ComponentModel;
using System.Runtime.CompilerServices;
using Prism.Models;
using Prism.Services;

namespace Prism.ViewModels;

/// <summary>
/// 设置页视图模型（步骤 7 最小实现：开机自启 + 数据目录显示）。
/// 快捷键 / 网页引擎完整 UI 留到步骤 8。
/// </summary>
public sealed class SettingsViewModel : INotifyPropertyChanged
{
    private readonly SettingsStore _store;
    private readonly AutoStartService _autoStart;
    private Settings _settings;
    private bool _autoStartEnabled;

    public event PropertyChangedEventHandler? PropertyChanged;

    public SettingsViewModel(SettingsStore store, AutoStartService autoStart)
    {
        _store = store;
        _autoStart = autoStart;
        _settings = store.Load();
        _autoStartEnabled = _settings.AutoStart;
        DataDir = store.DataDir;
    }

    /// <summary>实际数据目录（安装目录\data 或 %LocalAppData%\Prism）。</summary>
    public string DataDir { get; }

    /// <summary>开机自启开关；写入注册表并持久化到 settings.json。</summary>
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
                // 绑定可能已把勾选翻过去；通知 AutoStart 回滚 UI，并刷新状态行。
                StatusMessage = "自启设置失败：" + ex.Message;
                OnPropertyChanged(nameof(AutoStart));
                OnPropertyChanged(nameof(StatusMessage));
                return;
            }

            _autoStartEnabled = value;
            _settings = _settings with { AutoStart = value };
            _store.Save(_settings);
            StatusMessage = value ? "已开启开机自启" : "已关闭开机自启";
            OnPropertyChanged();
            OnPropertyChanged(nameof(StatusMessage));
        }
    }

    /// <summary>底部一行状态提示。</summary>
    public string StatusMessage { get; private set; } = "";

    private void OnPropertyChanged([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
