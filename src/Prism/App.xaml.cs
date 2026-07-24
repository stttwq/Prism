using System.Runtime.InteropServices;
using System.Windows;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Prism.Windows;

namespace Prism;

/// <summary>
/// 应用入口。第四步：设置 → 快捷键 → 管道 → SearchViewModel → 搜索窗口。
/// 尚无托盘时：启动后自动呼出一次搜索框，并在父控制台打印提示。
/// </summary>
public partial class App : Application
{
    private const int AttachParentProcess = -1;

    [DllImport("kernel32.dll")]
    private static extern bool AttachConsole(int dwProcessId);

    private SettingsStore? _store;
    private HotkeyService? _hotkey;
    private PipeClient? _pipe;
    private IconCache? _icons;
    private AppState? _state;
    private SearchViewModel? _vm;
    private SearchWindow? _searchWindow;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        ShutdownMode = ShutdownMode.OnExplicitShutdown;

        // 从 `dotnet run` / 终端启动时挂上父控制台，便于看到"已启动"提示。
        // 失败则忽略（双击 exe 启动时没有控制台）。
        try { AttachConsole(AttachParentProcess); } catch { /* ignore */ }

        _store = new SettingsStore();
        var settings = _store.Load();

        _state = new AppState();
        _pipe = new PipeClient();
        _icons = new IconCache();
        _vm = new SearchViewModel(_state, _pipe);

        _searchWindow = new SearchWindow();
        _searchWindow.Attach(_vm, _icons);

        _hotkey = new HotkeyService();
        _hotkey.Triggered += ToggleSearchWindow;
        _hotkey.Apply(settings);

        Log("Prism 已启动。");
        Log("  · 双击 Ctrl 呼出搜索框，Esc 或点别处隐藏");
        Log("  · 输入文件名即时搜索；回车打开，Ctrl+Enter 打开所在文件夹");
        Log("  · 结束请在任务管理器结束 Prism.exe，或关闭此终端");

        // 尚无托盘图标时，启动后自动呼出一次，避免用户以为"没反应"。
        Dispatcher.BeginInvoke(() =>
        {
            _searchWindow?.ShowAndFocus();
        }, System.Windows.Threading.DispatcherPriority.ApplicationIdle);

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
            }
            catch { /* 忽略 */ }
        }
        catch (Exception ex)
        {
            _state.IsBackendConnected = false;
            Log("后端暂未连上：" + ex.Message);
            Log("（可先 cargo build --manifest-path src/prism-core/Cargo.toml）");
        }
    }

    private static void Log(string msg)
    {
        try { Console.WriteLine(msg); } catch { /* 无控制台时忽略 */ }
        System.Diagnostics.Debug.WriteLine("[Prism] " + msg);
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _hotkey?.Dispose();
        _pipe?.Dispose();
        base.OnExit(e);
    }
}
