using System.Runtime.InteropServices;
using System.Windows;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Prism.Windows;

namespace Prism;

/// <summary>
/// 应用入口。步骤 7：托盘 + 自启 + 最小设置窗；快捷键与管道搜索保持步骤 1–6 行为。
/// </summary>
public partial class App : Application
{
    private const int AttachParentProcess = -1;

    [DllImport("kernel32.dll")]
    private static extern bool AttachConsole(int dwProcessId);

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

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        ShutdownMode = ShutdownMode.OnExplicitShutdown;

        // 从 `dotnet run` / 终端启动时挂上父控制台，便于看到"已启动"提示。
        // 失败则忽略（双击 exe 启动时没有控制台）。
        try { AttachConsole(AttachParentProcess); } catch { /* ignore */ }

        _store = new SettingsStore();
        var settings = _store.Load();

        _autoStart = new AutoStartService();
        try
        {
            // 让注册表与 settings.json 一致（例如用户换了安装路径后重新写入）。
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

        _searchWindow = new SearchWindow();
        _searchWindow.Attach(_vm, _icons);

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
        Log("  · 输入文件名即时搜索；回车打开，Ctrl+Enter 打开所在文件夹");

        _ = TryStartBackendAsync();
    }

    private void ToggleSearchWindow()
    {
        if (_searchWindow is null) return;
        if (_searchWindow.IsVisible)
            _searchWindow.HideAnimated();
        else
            _searchWindow.ShowAndFocus();
    }

    private void OpenSettings()
    {
        if (_store is null || _autoStart is null) return;

        if (_settingsWindow is { IsVisible: true })
        {
            _settingsWindow.Activate();
            return;
        }

        var vm = new SettingsViewModel(_store, _autoStart);
        _settingsWindow = new SettingsWindow(vm);
        _settingsWindow.Closed += (_, _) => _settingsWindow = null;
        _settingsWindow.Show();
        _settingsWindow.Activate();
    }

    private void OnRebuildIndex()
    {
        // 后端暂无 reindex IPC（仅有定时 refresh）；步骤 7 先提示，后续再补协议。
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
        _pipe?.Dispose();
        base.OnExit(e);
    }
}
