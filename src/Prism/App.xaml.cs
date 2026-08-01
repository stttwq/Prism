using System.Runtime.InteropServices;
using System.Windows;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Prism.Windows;

namespace Prism;

/// <summary>
/// 应用入口。托盘常驻；搜索窗懒创建；隐藏后清空图标缓存并修剪工作集。
/// </summary>
public partial class App : Application
{
    private const int AttachParentProcess = -1;

    [DllImport("kernel32.dll")]
    private static extern bool AttachConsole(int dwProcessId);

    /// <summary>
    /// 把进程工作集还给系统（仅影响 Working Set 显示，不释放私有提交；
    /// 适合托盘空闲时让任务管理器数字更接近真实占用）。
    /// </summary>
    [DllImport("psapi.dll")]
    private static extern bool EmptyWorkingSet(IntPtr hProcess);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    private SettingsStore? _store;
    private AutoStartService? _autoStart;
    private HotkeyService? _hotkey;
    private PipeClient? _pipe;
    private IconCache? _icons;
    private AppState? _state;
    private SearchViewModel? _vm;
    private SearchWindow? _searchWindow;
    private SettingsWindow? _settingsWindow;
    private TrayService? _tray;
    private ThemeWatcher? _theme;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        ShutdownMode = ShutdownMode.OnExplicitShutdown;

        // 从 `dotnet run` / 终端启动时挂上父控制台，便于看到"已启动"提示。
        try { AttachConsole(AttachParentProcess); } catch { /* ignore */ }

        _store = new SettingsStore();
        var settings = _store.Load();

        _autoStart = new AutoStartService();
        try
        {
            _autoStart.Apply(settings.AutoStart);
        }
        catch (Exception ex)
        {
            Log("自启同步失败：" + ex.Message);
        }

        _state = new AppState();
        _pipe = new PipeClient();
        _icons = new IconCache();
        _vm = new SearchViewModel(_state, _pipe);
        ApplySearchExclusions(settings);

        // 深浅色跟随系统（替换 App.xaml 中的 Tokens 字典）。
        _theme = new ThemeWatcher(_state);
        _theme.Start();

        // 搜索窗懒创建：冷启动托盘常驻时不先建 WPF 视觉树，降低初始工作集。
        // 第一次双击 Ctrl / 托盘左键时再 EnsureSearchWindow。

        _hotkey = new HotkeyService();
        _hotkey.Triggered += ToggleSearchWindow;
        _hotkey.Apply(settings);

        _tray = new TrayService();
        _tray.ShowSearchRequested += ToggleSearchWindow;
        _tray.OpenSettingsRequested += OpenSettings;
        _tray.RebuildIndexRequested += OnRebuildIndex;
        _tray.ExitRequested += () => Shutdown();

        Log("Prism 已启动（托盘常驻）。");
        Log("  · 双击 Ctrl 呼出搜索框，Esc 或点别处隐藏");
        Log("  · 托盘图标：左键呼出，右键打开设置 / 重建索引 / 退出");
        Log("  · 设置页可改快捷键、网页搜索引擎、开机自启");
        Log("  · 输入文件名即时搜索；回车打开，Ctrl+Enter 打开所在文件夹");
        Log("  · 选中文件后按 → 打开动作面板（打开所在文件夹/复制/剪切/复制路径）");
        Log($"  · 当前主题：{(_state.Theme == AppTheme.Dark ? "深色" : "浅色")}（跟随系统）");

        _ = TryStartBackendAsync();

        // 启动稳定后修剪一次工作集（后端已连上前后各可再剪）。
        _ = Dispatcher.BeginInvoke(new Action(TrimWorkingSet), System.Windows.Threading.DispatcherPriority.ApplicationIdle);
    }

    private SearchWindow EnsureSearchWindow()
    {
        if (_searchWindow is not null)
            return _searchWindow;

        if (_vm is null || _icons is null)
            throw new InvalidOperationException("SearchViewModel / IconCache 尚未初始化");

        _searchWindow = new SearchWindow();
        _searchWindow.Attach(_vm, _icons, _theme);
        return _searchWindow;
    }

    private void ToggleSearchWindow()
    {
        var win = EnsureSearchWindow();
        if (win.IsVisible)
            win.HideAnimated();
        else
            win.ShowAndFocus();
    }

    private void OpenSettings()
    {
        if (_store is null || _autoStart is null) return;

        if (_settingsWindow is { IsVisible: true })
        {
            _settingsWindow.Activate();
            return;
        }

        var vm = new SettingsViewModel(
            _store,
            _autoStart,
            onApplied: ApplySettings,
            onEnginesChanged: ReloadBackendEnginesAsync,
            onPreferencesChanged: UpdateBackendPreferencesAsync,
            onClearHistory: ClearBackendHistoryAsync);
        _settingsWindow = new SettingsWindow(vm);
        _settingsWindow.Closed += (_, _) =>
        {
            _settingsWindow = null;
            // 设置窗关掉后也修剪一下工作集。
            _ = Dispatcher.BeginInvoke(new Action(TrimWorkingSet), System.Windows.Threading.DispatcherPriority.ApplicationIdle);
        };
        _settingsWindow.Show();
        _settingsWindow.Activate();
    }

    private void ApplySettings(Settings settings)
    {
        try
        {
            _hotkey?.Apply(settings);
            ApplySearchExclusions(settings);
            Log($"快捷键已应用：{settings.HotkeyMode}" +
                (settings.HotkeyMode == HotkeyMode.Combo ? $" ({settings.ComboHotkey})" : ""));
        }
        catch (Exception ex)
        {
            Log("快捷键应用失败：" + ex.Message);
            throw;
        }
    }

    private void ApplySearchExclusions(Settings settings)
    {
        var filters = settings.ExcludedPaths
            .Select(path => new SearchFilterOption("exclude_path", path))
            .ToArray();
        _vm?.SetSearchContext(SearchContext.Default with { Filters = filters });
    }

    private async Task ReloadBackendEnginesAsync(IReadOnlyList<WebEngine> engines)
    {
        if (_pipe is null || !_pipe.IsConnected)
            throw new InvalidOperationException("后端未连接，引擎将在下次启动时生效");

        await _pipe.ReloadEnginesAsync(engines).ConfigureAwait(true);
        Log($"后端引擎已热重载（{engines.Count} 个）");
    }

    private async Task UpdateBackendPreferencesAsync(bool historyEnabled, bool pinyinEnabled)
    {
        if (_pipe is null || !_pipe.IsConnected)
            throw new InvalidOperationException("后端未连接，设置将在下次启动时生效");
        await _pipe.UpdatePreferencesAsync(historyEnabled, pinyinEnabled).ConfigureAwait(true);
    }

    private async Task ClearBackendHistoryAsync()
    {
        if (_pipe is null || !_pipe.IsConnected)
            throw new InvalidOperationException("后端未连接");
        await _pipe.ClearHistoryAsync().ConfigureAwait(true);
    }

    private void OnRebuildIndex()
    {
        MessageBox.Show(
            "索引会在后台自动建立与定期刷新。\n手动重建将在后续版本提供。",
            "Prism",
            MessageBoxButton.OK,
            MessageBoxImage.Information);
    }

    private async Task TryStartBackendAsync()
    {
        if (_pipe is null || _state is null) return;
        try
        {
            await _pipe.StartAsync();
            _state.IsBackendConnected = true;
            if (_store is not null)
            {
                var settings = _store.Load();
                await _pipe.UpdatePreferencesAsync(
                    settings.HistoryEnabled,
                    settings.PinyinEnabled).ConfigureAwait(true);
            }
            try
            {
                var ver = await _pipe.PingAsync();
                Log($"后端已连接（prism-core {ver}），索引后台构建中…");
                _tray?.SetTooltip($"Prism · 后端 {ver}");
            }
            catch { /* 忽略 */ }
        }
        catch (Exception ex)
        {
            _state.IsBackendConnected = false;
            Log("后端暂未连上：" + ex.Message);
            Log("（可先 cargo build --manifest-path src/prism-core/Cargo.toml）");
            _tray?.SetTooltip("Prism · 后端未连接");
        }
        finally
        {
            // 后端拉起后工作集会涨一截；空闲修剪。
            _ = Dispatcher.BeginInvoke(new Action(TrimWorkingSet), System.Windows.Threading.DispatcherPriority.ApplicationIdle);
        }
    }

    /// <summary>空闲时调用：缩小任务管理器显示的工作集（私有提交不变）。</summary>
    internal static void TrimWorkingSet()
    {
        try
        {
            EmptyWorkingSet(GetCurrentProcess());
        }
        catch
        {
            // 权限/兼容失败忽略。
        }
    }

    private static void Log(string msg)
    {
        try { Console.WriteLine(msg); } catch { /* 无控制台时忽略 */ }
        System.Diagnostics.Debug.WriteLine("[Prism] " + msg);
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _tray?.Dispose();
        _hotkey?.Dispose();
        _theme?.Dispose();
        _pipe?.Dispose();
        base.OnExit(e);
    }
}
