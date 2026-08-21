using System.IO;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Threading;
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
    private SingleInstance? _singleInstance;
    /// <summary>G8：favicon 磁盘缓存（含联网下载），数据目录与 settings.json 同级。</summary>
    private FaviconCache? _favicons;
    /// <summary>G8：搜索结果的网页图标提供器（授权门控走 _hostSettings）。</summary>
    private WebIconProvider? _webIcons;
    private bool _currentDirectorySearchEnabled = true;
    /// <summary>宿主 adapter 读取的设置快照；保存设置后更新，adapter 的 IsEnabled 委托读这里。</summary>
    private Settings _hostSettings = Settings.Default;
    /// <summary>动作快捷键绑定快照（2026-08-21 设想）：搜索窗懒创建时注入，保存后热更新。</summary>
    private IReadOnlyDictionary<string, string> _actionHotkeyBindings = new Dictionary<string, string>();
    /// <summary>暂存区（2026-08-22 计划）：独立 staging.json 持久化（不进 settings.json，
    /// 防设置页全量保存覆盖搜索窗侧写入）。</summary>
    private StagingStore? _stagingStore;
    private StagingArea? _staging;
    private string _stagingAddHotkey = Settings.Default.StagingAddHotkey;

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        ShutdownMode = ShutdownMode.OnExplicitShutdown;

        // AUDIT-2026-08-18 C-D1: 全局异常兜底。常驻托盘进程没有这三个钩子时，
        // 任何 UI 线程未捕获异常或后台 async void 异常都会让进程无声消失。
        DispatcherUnhandledException += OnDispatcherUnhandledException;
        TaskScheduler.UnobservedTaskException += OnUnobservedTaskException;
        AppDomain.CurrentDomain.UnhandledException += OnDomainUnhandledException;

        // 单实例守卫：必须在任何服务构造（尤其 TryStartBackendAsync）之前，
        // 防止第二个 Prism.exe 启动第二个 prism-core.exe / 注册第二个热键。
        // 已有实例时，向前者发 show 指令唤出搜索窗，然后本进程静默退出。
        _singleInstance = new SingleInstance();
        if (!_singleInstance.TryAcquire())
        {
            _singleInstance.SignalExistingInstance();
            Shutdown();
            return;
        }

        // 从 `dotnet run` / 终端启动时挂上父控制台，便于看到"已启动"提示。
        try { AttachConsole(AttachParentProcess); } catch { /* ignore */ }

        _store = new SettingsStore();
        var settings = _store.Load();
        _hostSettings = settings;
        _actionHotkeyBindings = settings.ActionHotkeys;
        _favicons = new FaviconCache(_store.DataDir);
        _stagingStore = new StagingStore(_store.DataDir);
        _staging = new StagingArea { Capacity = settings.StagingCapacity };
        _staging.Restore(_stagingStore.Load().Items);
        _staging.Changed += () => _stagingStore!.Save(
            _staging!.Items, Array.Empty<WorksetEntry>(), null);
        _stagingAddHotkey = settings.StagingAddHotkey;

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
        // watchdog 连接状态变化时更新 UI（在后台线程触发，需回到 UI 线程写 AppState）。
        _pipe.ConnectionChanged += connected =>
            Dispatcher.BeginInvoke(new Action(() =>
            {
                if (_state is null) return;
                _state.IsBackendConnected = connected;
                _tray?.SetTooltip(connected ? "Prism" : "Prism · 后端未连接");
                if (!connected)
                    _state.StatusMessage = "正在重连后端…";
            }));
        _icons = new IconCache();
        // G5: activation must run in this process — SetForegroundWindow only takes effect
        // from the foreground process, which is Prism at the moment Enter is pressed.
        // G8: suggestion service runs in the user session; broker/indexer never send requests.
        _vm = new SearchViewModel(
            _state,
            _pipe,
            activator: new Win32WindowActivator(),
            suggestions: new SuggestionService());
        ApplySearchExclusions(settings);
        _vm.UpdateWebSettings(settings.WebEngines, settings.SuggestionsEnabled);

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
        Log("  · 输入文件名即时搜索；回车打开，Ctrl+Enter 在原宿主中定位（无宿主时打开所在文件夹）");
        Log("  · 选中文件后按 → 打开动作面板（打开所在文件夹/复制/剪切/复制路径）");
        Log($"  · 当前主题：{(_state.Theme == AppTheme.Dark ? "深色" : "浅色")}（跟随系统）");

        _ = TryStartBackendAsync();

        // 前台监听：后续实例发来的 show 指令在此唤出搜索窗。
        // 放在服务构造完成后、TryStartBackendAsync 之后：此时 _vm / _icons 已就绪，
        // ToggleSearchWindow 可安全懒创建搜索窗。
        _singleInstance.StartForegroundListener(Dispatcher, ToggleSearchWindow);
    }

    private SearchWindow EnsureSearchWindow()
    {
        if (_searchWindow is not null)
            return _searchWindow;

        if (_vm is null || _icons is null)
            throw new InvalidOperationException("SearchViewModel / IconCache 尚未初始化");

        // 注入带设置驱动开关的真实 adapter 矩阵；SystemFileDialog 仍是占位。
        var scope = new HostScopeController(HostAdapterCatalog.Create(() => _hostSettings));
        _searchWindow = new SearchWindow(new IndexerGenerationClient(), scope);
        // 自定义引擎图标走 favicon 缓存 + 授权门控（内置引擎仍是矢量单例）。
        _webIcons = new WebIconProvider(
            _favicons,
            origin => _hostSettings.FaviconGrants.ContainsKey(origin));
        _searchWindow.Attach(_vm, _icons, _theme, _webIcons, _pipe, _staging);
        // 窗口是懒创建的，创建时补上设置里的当前目录搜索总开关。
        _searchWindow.Scope.SetCurrentDirectoryEnabled(_currentDirectorySearchEnabled);
        _searchWindow.SetActionHotkeys(_actionHotkeyBindings);
        _searchWindow.SetStagingAddHotkey(_stagingAddHotkey);
        return _searchWindow;
    }

    private void ToggleSearchWindow()
    {
        var win = EnsureSearchWindow();
        if (win.IsVisible)
            win.HideAnimated();
        else
            win.ShowAndFocus();
        // AUDIT-2026-08-18 C-D3: 呼出/隐藏时顺手重装低级键盘钩子，系统因回调超时
        // 摘钩后不必等 60s 定时器兜底——托盘呼出立刻自愈。重装本身被投递到钩子线程，
        // 不在 UI 线程上装钩（见 HotkeyService 的线程说明）。
        _hotkey?.RefreshHook();
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
            onClearHistory: ClearBackendHistoryAsync,
            onWebSettingsChanged: (engines, suggestionsEnabled) =>
                _vm?.UpdateWebSettings(engines, suggestionsEnabled),
            onRequestFaviconGrant: RequestFaviconGrant,
            onFaviconGranted: DownloadFavicon,
            onAliasList: ListBackendAliasesAsync,
            onAliasDelete: DeleteBackendAliasAsync);
        _settingsWindow = new SettingsWindow(vm);
        _settingsWindow.Closed += (_, _) =>
        {
            _settingsWindow = null;
        };
        _settingsWindow.Show();
        _settingsWindow.Activate();
    }

    /// <summary>
    /// G8：授权成功后异步下载 favicon。下载落盘 + 图标内存缓存失效后，
    /// 搜索结果下一次装饰自动换上新图标；失败静默保持通用图标。
    /// </summary>
    private void DownloadFavicon(string origin)
    {
        var cache = _favicons;
        if (cache is null) return;
        // 复审中危（2026-08-21 全仓重审）：WebIconProvider 的授权门闭包读
        // _hostSettings 快照，而 VM 侧授权落盘不经过 ApplySettings——快照不
        // 更新的话已授权图标要等下一次完整保存或重启才显示。此处同步补上
        // 该 origin（UI 线程调用；引用替换原子，adapter 委托读到旧或新皆合法）。
        var normalized = FaviconCache.NormalizeOrigin(origin);
        if (normalized is not null)
        {
            var grants = new Dictionary<string, FaviconGrant>(_hostSettings.FaviconGrants)
            {
                [normalized] = new FaviconGrant(normalized, DateTimeOffset.UtcNow.ToString("o")),
            };
            _hostSettings = _hostSettings with { FaviconGrants = grants };
        }
        _ = Task.Run(async () =>
        {
            try
            {
                await cache.DownloadFaviconAsync(origin).ConfigureAwait(false);
            }
            catch
            {
                // 下载失败静默回退通用图标，不打扰用户。
            }
            finally
            {
                _webIcons?.Invalidate();
            }
        });
    }

    private void ApplySettings(Settings settings)
    {
        try
        {
            _hotkey?.Apply(settings);
            ApplySearchExclusions(settings);
            _vm?.UpdateWebSettings(settings.WebEngines, settings.SuggestionsEnabled);
            _actionHotkeyBindings = settings.ActionHotkeys;
            _searchWindow?.SetActionHotkeys(settings.ActionHotkeys);
            // 暂存区容量/快捷键热更新（2026-08-22 计划）。
            if (_staging is not null)
                _staging.Capacity = settings.StagingCapacity;
            _stagingAddHotkey = settings.StagingAddHotkey;
            _searchWindow?.SetStagingAddHotkey(_stagingAddHotkey);
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
        _hostSettings = settings;
        var filters = settings.ExcludedPaths
            .Select(path => new SearchFilterOption("exclude_path", path))
            .ToArray();
        // 只改 Filters，保留当前范围（root）等其他上下文维度。
        _vm?.SetSearchContext(_vm.SearchContext with { Filters = filters });
        _searchWindow?.Scope.SetCurrentDirectoryEnabled(settings.CurrentDirectorySearchEnabled);
        _currentDirectorySearchEnabled = settings.CurrentDirectorySearchEnabled;
        // adapter.IsEnabled 读 _hostSettings；已捕获的 root 不在设置变更时复用旧目录——
        // 下次呼出 Capture 会先清空。此处不必 Invalidate。
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

    private async Task<IReadOnlyList<AliasEntry>> ListBackendAliasesAsync()
    {
        if (_pipe is null || !_pipe.IsConnected)
            throw new InvalidOperationException("后端未连接");
        return await _pipe.AliasListAsync().ConfigureAwait(true);
    }

    private async Task DeleteBackendAliasAsync(ActionTarget target)
    {
        if (_pipe is null || !_pipe.IsConnected)
            throw new InvalidOperationException("后端未连接");
        await _pipe.AliasDeleteAsync(target).ConfigureAwait(true);
    }

    /// <summary>
    /// G8：自定义引擎 origin 变化时弹出 favicon 联网许可对话框。
    /// 返回 true 表示用户同意联网获取该 origin 的 favicon。
    /// </summary>
    private bool RequestFaviconGrant(string origin)
    {
        var result = System.Windows.MessageBox.Show(
            $"是否允许 Prism 联网获取以下站点的图标？\n\n{origin}\n\n图标将缓存到本地。拒绝后使用通用图标，可在设置中重新添加引擎时再次授权。",
            "Prism · favicon 联网授权",
            MessageBoxButton.YesNo,
            MessageBoxImage.Question);
        return result == MessageBoxResult.Yes;
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
        // AUDIT-2026-08-18 C-D4: 删除 finally 块里的 TrimWorkingSet——
        // EmptyWorkingSet 只逐出工作集不降私有提交，下次呼出软缺页变慢。
        // 后端拉起后工作集涨是正常的，不需要逐出。
    }

    private static void Log(string msg)
    {
        try { Console.WriteLine(msg); } catch { /* 无控制台时忽略 */ }
        System.Diagnostics.Debug.WriteLine("[Prism] " + msg);
    }

    // ── 全局异常兜底（AUDIT-2026-08-18 C-D1）──────────────────────────────

    private static string LogFilePath =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Prism", "logs", "frontend.log");

    /// <summary>
    /// L 批次（FRESH-AUDIT-3-2026-08-20）：日志改投递到有界通道、由单一后台
    /// 任务批量落盘——此前异常处理内同步 AppendAllText 在 UI 线程做 I/O，
    /// 持续性异常（每帧抛）变成「UI 冻结 + 日志风暴」双重故障。
    /// 有界 + DropWrite：风暴时丢弃最旧等价物（新行），内存不涨。
    /// </summary>
    private static readonly Lazy<System.Threading.Channels.Channel<string>> LogQueue = new(() =>
    {
        var channel = System.Threading.Channels.Channel.CreateBounded<string>(
            new System.Threading.Channels.BoundedChannelOptions(512)
            {
                FullMode = System.Threading.Channels.BoundedChannelFullMode.DropWrite,
                SingleReader = true,
            });
        _ = Task.Run(async () =>
        {
            var batch = new System.Text.StringBuilder();
            while (await channel.Reader.WaitToReadAsync().ConfigureAwait(false))
            {
                batch.Clear();
                while (channel.Reader.TryRead(out var line))
                {
                    batch.Append(line).Append("\r\n");
                    if (batch.Length > 256 * 1024) break; // 单批上限，防一次写盘过大
                }
                AppendBatchToDisk(batch.ToString());
            }
        });
        return channel;
    });

    /// <summary>批量写盘（后台线程）。10MB 截断。绝不抛出。</summary>
    private static void AppendBatchToDisk(string batch)
    {
        try
        {
            var path = LogFilePath;
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            var info = new FileInfo(path);
            if (info.Exists && info.Length > 10 * 1024 * 1024)
                info.Delete();
            File.AppendAllText(path, batch);
        }
        catch { /* 日志 I/O 失败不能影响进程 */ }
    }

    /// <summary>投递一行日志（时间戳在此打）。队满即丢弃。</summary>
    private static void LogToFile(string msg)
    {
        _ = LogQueue.Value.Writer.TryWrite($"{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} {msg}");
    }

    /// <summary>同步直写（进程即将终结时的遗言，异步通道来不及冲刷）。</summary>
    private static void LogToFileSync(string msg)
    {
        try
        {
            var path = LogFilePath;
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.AppendAllText(path, $"{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} {msg}\r\n");
        }
        catch { /* 日志 I/O 失败不能影响进程 */ }
    }

    /// <summary>
    /// L 批次：异常预算——60 秒滚动窗口内 Dispatcher 异常超过 50 次即视为
    /// 持续性风暴，继续 Handled=true 只会无限冻结 UI，放行默认处理（退出）
    /// 反而更诚实。
    /// </summary>
    private static readonly object ExcessSync = new();
    private static long _excessWindowStart;
    private static int _excessCount;

    private static bool ExceedsExceptionBudget()
    {
        lock (ExcessSync)
        {
            var now = Environment.TickCount64;
            if (now - _excessWindowStart > 60_000)
            {
                _excessWindowStart = now;
                _excessCount = 0;
            }
            _excessCount++;
            return _excessCount > 50;
        }
    }

    private static void LogException(string source, Exception ex)
    {
        LogToFile($"{source}: {ex.GetType().Name}: {ex.Message}\r\n{ex.StackTrace}");
        // 同步输出到调试通道，方便开发期即时看到。
        System.Diagnostics.Debug.WriteLine($"[Prism] {source}: {ex}");
    }

    private void OnDispatcherUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        try
        {
            if (ExceedsExceptionBudget())
            {
                LogToFile($"DispatcherUnhandledException: 60s 内超过 50 次，放弃拦截（异常: {e.Exception.GetType().Name}）");
                e.Handled = false;
                return;
            }
            LogException("DispatcherUnhandledException", e.Exception);
        }
        catch { /* 吞掉日志异常 */ }
        // 标记已处理：进程不退出，让用户至少有日志可查。
        e.Handled = true;
    }

    private static void OnUnobservedTaskException(object? sender, UnobservedTaskExceptionEventArgs e)
    {
        try { LogException("UnobservedTaskException", e.Exception); }
        catch { /* 吞掉日志异常 */ }
        e.SetObserved();
    }

    private static void OnDomainUnhandledException(object sender, UnhandledExceptionEventArgs e)
    {
        // AppDomain 级异常无法阻止退出，但至少留一条遗言。
        // L 批次：isTerminating 时异步通道来不及冲刷——同步直写。
        try
        {
            var msg = e.ExceptionObject is Exception ex
                ? $"{ex.GetType().Name}: {ex.Message}\r\n{ex.StackTrace}"
                : e.ExceptionObject?.ToString() ?? "unknown";
            LogToFileSync($"AppDomain.UnhandledException (isTerminating={e.IsTerminating}): {msg}");
        }
        catch { /* 无法记日志就静默 */ }
    }

    protected override void OnExit(ExitEventArgs e)
    {
        _tray?.Dispose();
        _hotkey?.Dispose();
        _theme?.Dispose();
        _pipe?.Dispose();
        _singleInstance?.Dispose();
        base.OnExit(e);
    }
}
