using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Effects;
using System.Windows.Threading;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;

namespace Prism.Windows;

/// <summary>
/// 主搜索窗口（无边框、置顶、宽 660px，顶部距屏幕 25% 高度）。
/// 第九步：PinButton / ActionPanel / 深浅色 / 展开动画。
/// </summary>
public partial class SearchWindow : Window
{
    private const double FadeInMs = 120;
    private const double FadeOutMs = 80;
    private const double SlideOffsetPx = 6;
    private const double PanelExpandMs = 100;

    private readonly IIndexGenerationClient _generationClient;
    private readonly HostScopeController _scope;
    private SearchViewModel? _vm;
    private IconCache? _icons;
    private ThemeWatcher? _theme;
    private WebIconProvider? _webIcons;
    private PipeClient? _pipe;
    /// <summary>窗口级动作快捷键表（2026-08-21 设想）：未装配时空表，恒不命中。</summary>
    private ActionHotkeyTable _actionHotkeys = ActionHotkeyTable.Empty;
    private bool _suppressQueryEvent;
    private bool _hiding;
    /// <summary>呼出后短时间内忽略失焦，避免 Show/Activate 过程中被立刻关掉。</summary>
    private bool _ignoreDeactivate;
    private bool _contextMenuOpen;
    private bool _contextMenuActionPending;
    private int _contextMenuRequestSeq;
    private double _panelTargetHeight;
    /// <summary>PanelHost 高度动画在途标记：同目标的重复通知不得重启或打断在途动画。</summary>
    private bool _heightAnimating;
    /// <summary>ApplyState 上一次应用的面板三态：用于区分状态机跃迁与内容搅动（见 ApplyState）。</summary>
    private PanelKind _lastPanelKind;

    private enum PanelKind { None, Results, Actions }
    /// <summary>呼出捕获的串台序号：快速呼出/隐藏/再呼出时丢弃过期的后台识别结果。</summary>
    private int _captureSeq;
    /// <summary>隐藏后延迟 Trim 的计时器；再呼出时取消，避免影响下次呼出响应。</summary>
    private DispatcherTimer? _idleTrimTimer;

    // ── Win32 foreground hook ─────────────────────────────────────────────
    // Deactivated 事件在两个 Topmost 窗口交互时可能不触发，SetWinEventHook 以
    // 系统级事件兜底。OUTOFCONTEXT 保证回调在 UI 线程消息循环上执行。
    private const uint EVENT_SYSTEM_FOREGROUND = 0x0003;
    private const uint WINEVENT_OUTOFCONTEXT = 0x0000;
    private const uint WINEVENT_SKIPOWNPROCESS = 0x0002;

    private delegate void WinEventProc(
        IntPtr hWinEventHook, uint eventCode, IntPtr hwnd,
        int idObject, int idChild, uint dwEventThread, uint dwmsEventTime);

    [DllImport("user32.dll")] private static extern IntPtr SetWinEventHook(
        uint eventMin, uint eventMax, IntPtr hmodWinEventProc,
        WinEventProc lpfnWinEventProc, uint idProcess, uint idThread, uint dwFlags);
    [DllImport("user32.dll")] private static extern bool UnhookWinEvent(IntPtr hWinEventHook);

    private WinEventProc? _foregroundHookProc; // keep-alive: GC must not collect the delegate
    private IntPtr _foregroundHook;
    private IntPtr _hwnd;

    public SearchWindow() : this(new IndexerGenerationClient())
    {
    }

    public SearchWindow(IIndexGenerationClient generationClient)
        : this(generationClient, new HostScopeController())
    {
    }

    public SearchWindow(IIndexGenerationClient generationClient, HostScopeController scope)
    {
        _generationClient = generationClient;
        _scope = scope;
        InitializeComponent();
        PreviewKeyDown += OnWindowPreviewKeyDown;
        Deactivated += OnDeactivated;
        _foregroundHookProc = OnForegroundChanged;
        _foregroundHook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, IntPtr.Zero,
            _foregroundHookProc, 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SizeChanged += (_, _) => UpdateCardClip();
        Loaded += (_, _) => { _hwnd = new WindowInteropHelper(this).Handle; UpdateCardClip(); };
        Closed += (_, _) =>
        {
            if (_foregroundHook != IntPtr.Zero) { UnhookWinEvent(_foregroundHook); _foregroundHook = IntPtr.Zero; }
            _hwnd = IntPtr.Zero;
            _generationClient.Dispose();
        };
        _generationClient.GenerationChanged += _ => Dispatcher.BeginInvoke(() =>
        {
            if (IsVisible && !_hiding && _vm is not null
                && !string.IsNullOrWhiteSpace(_vm.State.Query))
            {
                _vm.OnIndexGenerationChanged();
            }
        });

        Header.QueryChanged += OnHeaderQueryChanged;
        Header.QueryKeyDown += OnHeaderKeyDown;
        Header.ScopeToggleRequested += ToggleSearchScope;
        _scope.Changed += OnScopeChanged;
        Results.SelectedIndexChanged += OnResultsSelected;
        Results.ContextMenuRequested += OnContextMenuRequested;
        Results.ItemInvoked += async r =>
        {
            if (_vm is null) return;
            var idx = IndexOfResult(r);
            if (idx >= 0) _vm.State.SelectedIndex = idx;
            await _vm.ExecuteSelectedAsync();
        };
        Actions.SelectedIndexChanged += idx =>
        {
            if (_vm is not null) _vm.State.SelectedActionIndex = idx;
        };
        Actions.ActionInvoked += async a =>
        {
            if (_vm is null) return;
            // 同步选中再执行。
            var list = _vm.State.Actions;
            for (var i = 0; i < list.Count; i++)
            {
                if (ReferenceEquals(list[i], a) || list[i].Id == a.Id && list[i].Label == a.Label)
                {
                    _vm.State.SelectedActionIndex = i;
                    break;
                }
            }
            await _vm.ExecuteActionAsync();
        };
        Pin.IsPinnedChanged += pinned =>
        {
            if (_vm is not null) _vm.State.IsPinned = pinned;
        };
    }

    /// <summary>由 App 在启动时注入 ViewModel、图标缓存与主题监听。
    /// pipe 为可选的别名通道（右键「设置别名…」）；为空时该菜单项不出现。</summary>
    public void Attach(
        SearchViewModel vm,
        IconCache icons,
        ThemeWatcher? theme = null,
        WebIconProvider? webIcons = null,
        PipeClient? pipe = null)
    {
        _vm = vm;
        _icons = icons;
        _theme = theme;
        _webIcons = webIcons ?? new WebIconProvider();
        _pipe = pipe;
        Results.SetIconCache(icons);
        Results.SetWebIconProvider(_webIcons);
        vm.HideRequested += () =>
        {
            // L 批次（FRESH-AUDIT-3-2026-08-20）：BeginInvoke——后台线程的隐藏
            // 请求不能阻塞等待 UI 线程（与 RootRejected 同一权衡）。
            if (Dispatcher.CheckAccess()) HideAnimated();
            else Dispatcher.BeginInvoke(HideAnimated);
        };
        vm.IdleMemoryReleaseRequested += () =>
        {
            // 清空查询丢弃了结果引用，但窗口仍可见不会走 ReleaseIdleMemory。
            // 在 ApplicationIdle 上做一次轻量回收（保留扩展名图标缓存），
            // 不在 hot path 上阻塞。Trim 延迟到隐藏后统一执行。
            // G3（FRESH-AUDIT-2）：可见期只收 Gen0/1——Gen2 强制压缩会阻塞 UI 线程
            // 数十至数百毫秒；全量压缩留给隐藏后的 _idleTrimTimer 路径。
            // L 批次（FRESH-AUDIT-3-2026-08-20）：进一步降为 Optimized 非阻塞、
            // 不压缩——Gen1 强制压缩在 UI 线程仍可感知；真正的回收取舍交给
            // 隐藏后的全量 Trim 路径。
            Dispatcher.BeginInvoke(new Action(() =>
            {
                _icons?.ClearPathKeys();
                GC.Collect(0, GCCollectionMode.Optimized, blocking: false, compacting: false);
            }), System.Windows.Threading.DispatcherPriority.ApplicationIdle);
        };
        vm.State.PropertyChanged += OnStateChanged;
        // 复审 M1（2026-08-21）：copy_to/move_to 的模态文件夹选择器夺走前台时，
        // 挂起失活隐藏——否则窗口在对话框后面隐藏并清空查询/结果。与右键
        // 菜单动作行的守卫（_contextMenuActionPending + 延时释放）同一纪律。
        vm.ModalPickStarted += () =>
        {
            _contextMenuActionPending = true;
            _ignoreDeactivate = true;
        };
        vm.ModalPickEnded += () =>
        {
            _contextMenuActionPending = false;
            if (!_contextMenuOpen)
                ReleaseDeactivateGuardAfterDelay();
        };
        vm.RootRejected += rejection =>
        {
            // BeginInvoke 而非 Invoke：后台线程触发的 Invalidate 不能阻塞等待 UI，
            // 否则与控制器内部状态锁互等（见 HostScopeController._gate 注释）。
            Dispatcher.BeginInvoke(() => _scope.Invalidate(rejection));
        };
        if (_theme is not null)
            _theme.ThemeApplied += OnThemeApplied;
        Pin.IsPinned = vm.State.IsPinned;
    }

    public bool IsPinned
    {
        get => _vm?.State.IsPinned ?? false;
        set
        {
            if (_vm is not null) _vm.State.IsPinned = value;
            Pin.IsPinned = value;
        }
    }

    /// <summary>呼出：重置状态、定位、淡入、焦点到输入框。</summary>
    public void ShowAndFocus()
    {
        // 必须在 Show/Activate 之前取前台窗口，之后前台就是 Prism 自己了。
        var foreground = ForegroundInterop.GetForegroundWindow();

        _hiding = false;
        _ignoreDeactivate = true;
        BeginAnimation(OpacityProperty, null);
        // A4：取消隐藏时启动的延迟 trim——再呼出不应被 GC/Trim 打断。
        _idleTrimTimer?.Stop();
        _idleTrimTimer = null;

        _vm?.ResetForShow();
        _suppressQueryEvent = true;
        Header.ClearQuery();
        Header.SetMode(PanelMode.Idle);
        _suppressQueryEvent = false;
        // 每次呼出重新识别宿主，绝不沿用上一次目录。识别跑在后台线程：
        // 适配器做跨进程 COM / 子进程等待（Opus 最长数秒），放 UI 线程会冻住呼出动画，
        // 且低级键盘钩子同线程，卡顿超时会被 Windows 静默摘除热键。
        // 前台窗口必须同步采样（Show 之前），范围先重置为全局，结果异步回来再应用。
        var captureSeq = ++_captureSeq;
        _scope.ResetForCapture();
        ApplyScopeUi();
        ApplyState(_vm?.State, animatePanel: false);

        PositionWindow(foreground);
        Opacity = 0;
        Show();
        SyncGenerationPolling();
        ForceActivate();
        Topmost = true;

        var fadeIn = new DoubleAnimation(0, 1, TimeSpan.FromMilliseconds(FadeInMs))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
        };
        var slideIn = new DoubleAnimation(SlideOffsetPx, 0, TimeSpan.FromMilliseconds(FadeInMs))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
        };
        var transform = new TranslateTransform();
        RenderTransform = transform;

        BeginAnimation(OpacityProperty, fadeIn);
        transform.BeginAnimation(TranslateTransform.YProperty, slideIn);

        Dispatcher.BeginInvoke(() =>
        {
            ForceActivate();
            Header.FocusQuery();
            ReleaseDeactivateGuardAfterDelay();
        }, System.Windows.Threading.DispatcherPriority.Input);

        _ = RunCaptureAsync(foreground, captureSeq);
    }

    /// <summary>
    /// 后台识别宿主并回到 UI 线程应用。迟到无害：空查询下 root 到达由
    /// SetSearchContext 触发「当前目录最近使用」重搜，用户已输入则重搜自动带上 root。
    ///
    /// 必须在 STA 线程上执行：Explorer adapter 的 <c>Shell.Application</c> COM 调用
    /// （<c>IShellWindows</c> 枚举 + <c>IServiceProvider</c>→<c>IShellBrowser</c> 链）
    /// 是 STA-only 对象，线程池 MTA 线程上调用会静默返回空，导致永远拿不到当前目录。
    /// </summary>
    private async Task RunCaptureAsync(IntPtr foreground, int seq)
    {
        HostContext result;
        try
        {
            result = await RunOnStaThread(() => _scope.ComputeCapture(foreground)).ConfigureAwait(true);
        }
        catch
        {
            result = HostContext.Cleared(HostDetectionStatus.DetectFailed);
        }
        // 过期结果：窗口已隐藏或已再次呼出，直接丢弃。
        if (seq != _captureSeq || !IsVisible || _hiding) return;
        _scope.ApplyCapture(result);
    }

    /// <summary>
    /// L 批次（FRESH-AUDIT-3-2026-08-20）：常驻 STA 工作线程——此前每次呼出
    /// 都新建一次性 STA 线程做宿主识别（线程创建 + COM init 的成本白付），
    /// 且多个识别并发时线程数随呼出次数增长。Shell COM 对象
    /// （<c>Shell.Application</c> / <c>IShellWindows</c> / <c>IShellBrowser</c>）
    /// 是 STA-only，线程池 MTA 线程上调用会静默返回空；单一常驻 STA 线程
    /// 串行服务全部调用（宿主识别是唯一用户，串行即足够）。
    /// </summary>
    private static readonly System.Collections.Concurrent.BlockingCollection<Action> StaWork =
        new(new System.Collections.Concurrent.ConcurrentQueue<Action>());

    private static readonly Lazy<System.Threading.Thread> StaWorker = new(() =>
    {
        var thread = new System.Threading.Thread(StaWorkerLoop)
        {
            IsBackground = true,
            Name = "prism-sta-host",
        };
        thread.SetApartmentState(System.Threading.ApartmentState.STA);
        thread.Start();
        return thread;
    });

    private static void StaWorkerLoop()
    {
        foreach (var work in StaWork.GetConsumingEnumerable())
        {
            work();
        }
    }

    private static Task<T> RunOnStaThread<T>(Func<T> action)
    {
        var tcs = new TaskCompletionSource<T>(TaskCreationOptions.RunContinuationsAsynchronously);
        StaWork.Add(() =>
        {
            try { tcs.SetResult(action()); }
            catch (Exception ex) { tcs.SetException(ex); }
        });
        _ = StaWorker.Value; // 惰性启动（首次调用时拉起常驻线程）。
        return tcs.Task;
    }

    private void ForceActivate()
    {
        try
        {
            Activate();
            Focus();

            var helper = new WindowInteropHelper(this);
            var hwnd = helper.EnsureHandle();
            if (hwnd == IntPtr.Zero) return;

            ForegroundInterop.ShowWindow(hwnd, ForegroundInterop.SW_SHOW);
            if (ForegroundInterop.TryForceForeground(hwnd))
            {
                Activate();
                return;
            }

            // Same-thread case: foreground was already ours, just re-assert.
            ForegroundInterop.SetForegroundWindow(hwnd);
            Activate();
        }
        catch
        {
            try { Activate(); } catch { /* ignore */ }
        }
    }

    public void HideAnimated()
    {
        if (_hiding || !IsVisible) return;
        _hiding = true;
        _generationClient.SetActive(false);
        _ignoreDeactivate = true;
        var fadeOut = new DoubleAnimation(Opacity, 0, TimeSpan.FromMilliseconds(FadeOutMs))
        {
            // 入场 EaseOut / 退场 EaseIn 对称：收尾不发硬。
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseIn },
        };
        fadeOut.Completed += (_, _) =>
        {
            Hide();
            _hiding = false;
            try { Keyboard.ClearFocus(); } catch { /* ignore */ }
            ReleaseIdleMemory();
        };
        BeginAnimation(OpacityProperty, fadeOut);
    }

    private void ReleaseIdleMemory()
    {
        try
        {
            _vm?.ResetForShow();
            Results.Items = Array.Empty<SearchResult>();
            Results.StatusMessage = "";
            Actions.Items = Array.Empty<ActionItem>();
            // A4：只清路径类图标键，保留 ext:/dir: 扩展名键——下次呼出扩展名图标立即可见。
            _icons?.ClearPathKeys();
            // Bug 1：清空 WebIconProvider 按 origin 无界增长的瞬态缓存（每个 origin 持一个
            // frozen ImageSource 强引用，GC 回收不了）。下次网页搜索从磁盘 FaviconCache 重读。
            _webIcons?.ClearTransientCaches();
            // AUDIT-2026-08-18 C-D4: 删除 EmptyWorkingSet 调用（只逐出工作集不降私有提交，
            // 下次呼出软缺页变慢）。改 GC.Collect Forced + blocking:true + compacting:true
            // 确保真正压缩堆降私有提交。窗口已隐藏不卡交互。
            // 再呼出（ShowAndFocus）会取消此 timer。
            _idleTrimTimer?.Stop();
            _idleTrimTimer = new DispatcherTimer
            {
                Interval = TimeSpan.FromMinutes(3),
            };
            _idleTrimTimer.Tick += (_, _) =>
            {
                _idleTrimTimer?.Stop();
                _idleTrimTimer = null;
                // AUDIT-4 B7（2026-08-21）：Tick 已入队但窗口再呼出（ShowAndFocus
                // 停表后再呼出）的竞态下，阻塞式 Gen2 压缩会撞上正在使用的 UI 线程
                // （违背 A4 承诺）——收集前再核一次窗口状态。
                if (IsVisible || _hiding) return;
                GC.Collect(2, GCCollectionMode.Forced, blocking: true, compacting: true);
            };
            _idleTrimTimer.Start();
        }
        catch
        {
            // 释放失败不影响下次呼出。
        }
    }

    private void OnWindowPreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Escape)
        {
            HandleEscape();
            e.Handled = true;
        }
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        base.OnKeyDown(e);
        if (e.Key == Key.Escape)
        {
            HandleEscape();
            e.Handled = true;
        }
    }

    private void HandleEscape()
    {
        if (_vm?.State.Mode == PanelMode.Actions)
        {
            LeaveActionsUi();
            return;
        }
        HideAnimated();
    }

    private void LeaveActionsUi()
    {
        if (_vm is null) return;
        _vm.LeaveActions();
        _suppressQueryEvent = true;
        Header.Query = _vm.State.Query;
        Header.SetMode(PanelMode.Results);
        _suppressQueryEvent = false;
        ApplyState(_vm.State, animatePanel: true);
        Header.FocusQuery();
    }

    private void OnDeactivated(object? sender, EventArgs e)
    {
        if (!SearchWindowFocusPolicy.ShouldHide(
                _ignoreDeactivate,
                _contextMenuOpen,
                _contextMenuActionPending,
                IsPinned,
                _hiding))
            return;
        HideAnimated();
    }

    private void OnForegroundChanged(
        IntPtr hWinEventHook, uint eventCode, IntPtr hwnd,
        int idObject, int idChild, uint dwEventThread, uint dwmsEventTime)
    {
        if (idObject != 0) return;
        // 先检查 guard，不碰任何 WPF 状态——避免窗口初始化期间的重入崩溃。
        // ShowAndFocus 在 Show() 之前设 _ignoreDeactivate=true，300ms 后才释放，
        // 此期间 Topmost 切换可能触发本回调，此时访问 WindowInteropHelper.Handle
        // 会强制创建未完成的 HwndSource，导致 coreclr.dll Access Violation。
        if (_ignoreDeactivate || _hiding) return;
        if (!IsVisible) return;
        if (hwnd != IntPtr.Zero && hwnd == _hwnd) return;
        if (!SearchWindowFocusPolicy.ShouldHide(
                _ignoreDeactivate,
                _contextMenuOpen,
                _contextMenuActionPending,
                IsPinned,
                _hiding))
            return;
        HideAnimated();
    }

    private async void OnContextMenuRequested(SearchResult target)
    {
        if (_vm is null) return;

        var requestSeq = ++_contextMenuRequestSeq;
        var actions = await _vm.GetActionsForAsync(target).ConfigureAwait(true);
        if (requestSeq != _contextMenuRequestSeq || !IsVisible || _hiding || IndexOfResult(target) < 0)
            return;
        if (actions.Count == 0) return;

        var menu = new ContextMenu
        {
            PlacementTarget = Results,
            Placement = PlacementMode.MousePoint,
        };
        menu.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuStyle");

        // 别名系统（2026-08-21 设想）：file/folder/app 行首位放「设置别名…」。
        if (_pipe is not null && target.Kind is "app" or "file" or "folder"
            && !string.IsNullOrEmpty(target.ExecuteId))
        {
            var aliasItem = new MenuItem { Header = "设置别名…" };
            aliasItem.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuItemStyle");
            aliasItem.Icon = new TextBlock
            {
                Text = "\uE8AC",
                FontFamily = (System.Windows.Media.FontFamily)FindResource("IconFontFamily"),
                FontSize = 14,
                Foreground = (Brush)FindResource("TextSubtitle"),
                Width = 18,
                Height = 18,
                VerticalAlignment = VerticalAlignment.Center,
                HorizontalAlignment = System.Windows.HorizontalAlignment.Center,
                TextAlignment = TextAlignment.Center,
            };
            aliasItem.Click += async (_, _) =>
            {
                _contextMenuActionPending = true;
                _ignoreDeactivate = true;
                try
                {
                    await ShowAliasDialogAsync(target).ConfigureAwait(true);
                }
                finally
                {
                    _contextMenuActionPending = false;
                    if (!_contextMenuOpen)
                        ReleaseDeactivateGuardAfterDelay();
                }
            };
            menu.Items.Add(aliasItem);
        }

        var separatorPending = menu.Items.Count > 0;
        foreach (var action in actions)
        {
            if (action.IsSectionHeader)
            {
                separatorPending = menu.Items.Count > 0;
                continue;
            }

            if (separatorPending)
            {
                var separator = new Separator();
                separator.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuSeparatorStyle");
                menu.Items.Add(separator);
                separatorPending = false;
            }

            var item = new MenuItem { Header = action.Label };
            item.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuItemStyle");
            // U7：右键菜单加图标列，与动作面板视觉统一。
            if (!action.IsSectionHeader && !string.IsNullOrEmpty(action.IconGlyph))
            {
                item.Icon = new TextBlock
                {
                    Text = action.IconGlyph,
                    FontFamily = (System.Windows.Media.FontFamily)FindResource("IconFontFamily"),
                    FontSize = 14,
                    Foreground = (Brush)FindResource("TextSubtitle"),
                    Width = 18,
                    Height = 18,
                    VerticalAlignment = VerticalAlignment.Center,
                    HorizontalAlignment = System.Windows.HorizontalAlignment.Center,
                    TextAlignment = TextAlignment.Center,
                };
            }
            item.Click += async (_, _) =>
            {
                if (_vm is null) return;
                _contextMenuActionPending = true;
                _ignoreDeactivate = true;
                try
                {
                    // rename 需要可见的动作面板才能编辑新文件名，
                    // 右键菜单场景先进入 Actions 面板再触发 rename。
                    if (action.Id == "rename")
                    {
                        // 复审中危（2026-08-21 全仓重审）：菜单打开期间 generation
                        // 刷新可能已把目标行换掉——IndexOfResult 落空必须中止，
                        // 否则 rename 会作用于当前选中的另一个文件。
                        var idx = IndexOfResult(target);
                        if (idx < 0) return;
                        _vm.State.SelectedIndex = idx;
                        await EnterActionsUiAsync().ConfigureAwait(true);
                        // 进入面板后选中 rename 动作并执行；列表无 rename（broker
                        // 按目标状态裁剪）时退回结果，绝不静默执行其他动作。
                        if (!TrySelectAction("rename"))
                        {
                            LeaveActionsUi();
                            return;
                        }
                        await _vm.ExecuteActionAsync().ConfigureAwait(true);
                    }
                    else
                    {
                        await _vm.RunActionOnAsync(target, action).ConfigureAwait(true);
                    }
                }
                finally
                {
                    _contextMenuActionPending = false;
                    if (!_contextMenuOpen)
                        ReleaseDeactivateGuardAfterDelay();
                }
            };
            menu.Items.Add(item);
        }

        if (menu.Items.Count == 0) return;

        _contextMenuOpen = true;
        _ignoreDeactivate = true;
        menu.Closed += (_, _) =>
        {
            _contextMenuOpen = false;
            if (IsVisible && !_hiding)
                Dispatcher.BeginInvoke(Header.FocusQuery, DispatcherPriority.Input);
            if (!_contextMenuActionPending)
                ReleaseDeactivateGuardAfterDelay();
        };
        menu.IsOpen = true;
    }

    /// <summary>
    /// 别名系统（2026-08-21 设想）：右键「设置别名…」的输入对话框。
    /// 逗号/空白分隔多个词；清空并确定 = 解绑。预填该目标的现有词。
    /// </summary>
    private async Task ShowAliasDialogAsync(SearchResult target)
    {
        if (_pipe is null || _vm is null) return;
        var aliasTarget = target.ExecutionTarget;

        // 预填：拉取现有词表（失败静默按空处理）。
        var existing = Array.Empty<string>();
        try
        {
            if (!_pipe.IsConnected)
                await _pipe.StartAsync().ConfigureAwait(true);
            var entries = await _pipe.AliasListAsync().ConfigureAwait(true);
            existing = entries
                .FirstOrDefault(entry => entry.Target.Kind == aliasTarget.Kind
                    && string.Equals(entry.Target.Value, aliasTarget.Value, StringComparison.OrdinalIgnoreCase))
                ?.Words.ToArray() ?? Array.Empty<string>();
        }
        catch
        {
            // 后端未就绪时仍允许打开对话框（保存时报错）。
        }

        var input = new System.Windows.Controls.TextBox
        {
            Text = string.Join(", ", existing),
            FontSize = 14,
            Padding = new Thickness(8, 6, 8, 6),
        };
        var dialog = new Window
        {
            Title = "设置别名",
            Width = 460,
            SizeToContent = SizeToContent.Height,
            WindowStartupLocation = WindowStartupLocation.CenterOwner,
            Owner = this,
            ResizeMode = ResizeMode.NoResize,
            ShowInTaskbar = false,
            WindowStyle = WindowStyle.ToolWindow,
        };
        var okButton = new System.Windows.Controls.Button { Content = "保存", Padding = new Thickness(16, 6, 16, 6), IsDefault = true };
        var cancelButton = new System.Windows.Controls.Button { Content = "取消", Padding = new Thickness(16, 6, 16, 6), IsCancel = true };
        var panel = new StackPanel { Margin = new Thickness(16) };
        panel.Children.Add(new TextBlock
        {
            Text = System.IO.Path.GetFileName(target.ExecuteId),
            FontSize = 13,
            FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(0, 0, 0, 4),
        });
        panel.Children.Add(new TextBlock
        {
            Text = "输入一个或多个别名（逗号或空格分隔）；输入词与查询完全一致时置顶显示该文件。清空后保存 = 删除别名。",
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Opacity = 0.7,
            Margin = new Thickness(0, 0, 0, 10),
        });
        panel.Children.Add(input);
        var buttons = new StackPanel
        {
            Orientation = System.Windows.Controls.Orientation.Horizontal,
            HorizontalAlignment = System.Windows.HorizontalAlignment.Right,
            Margin = new Thickness(0, 14, 0, 0),
        };
        buttons.Children.Add(okButton);
        buttons.Children.Add(cancelButton);
        cancelButton.Margin = new Thickness(8, 0, 0, 0);
        panel.Children.Add(buttons);
        dialog.Content = panel;

        var confirmed = false;
        okButton.Click += (_, _) =>
        {
            confirmed = true;
            dialog.Close();
        };
        dialog.Loaded += (_, _) => { input.Focus(); input.SelectAll(); };
        dialog.ShowDialog();
        if (!confirmed) return;

        var words = input.Text
            .Split([',', '，', ';', '；'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Select(word => word.Trim())
            .Where(word => word.Length > 0)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .ToList();

        try
        {
            if (!_pipe.IsConnected)
                await _pipe.StartAsync().ConfigureAwait(true);
            await _pipe.AliasSetAsync(aliasTarget, words).ConfigureAwait(true);
            _vm.State.StatusMessage = words.Count > 0
                ? $"别名已保存：{string.Join("、", words)}"
                : "别名已删除";
        }
        catch (Exception ex)
        {
            _vm.State.StatusMessage = "别名保存失败：" + ex.Message;
        }
    }

    private void ReleaseDeactivateGuardAfterDelay()
    {
        var timer = new DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(300),
        };
        timer.Tick += (_, _) =>
        {
            timer.Stop();
            if (!_contextMenuOpen && !_contextMenuActionPending && !_hiding)
                _ignoreDeactivate = false;
        };
        timer.Start();
    }

    /// <summary>当前目录 / 全局 范围状态机，供 App 同步设置里的总开关。</summary>
    public HostScopeController Scope => _scope;

    /// <summary>
    /// 更新窗口级动作快捷键表（设置加载后与每次保存后由 App 调用；UI 线程）。
    /// 非法条目（未知 id/无修饰键/保留键）在表构建时已被丢弃，这里无需再防。
    /// </summary>
    public void SetActionHotkeys(IReadOnlyDictionary<string, string> bindings) =>
        _actionHotkeys = ActionHotkeyTable.FromSettings(bindings);

    /// <summary>
    /// 范围标签点击或 `Ctrl+G`：在当前目录与全局之间切换。
    /// AUDIT-4 A2（2026-08-21）：切回「当前目录」方向的路径复验含磁盘 I/O
    /// （RootValidation：Exists + 目录枚举探权限），网络盘/休眠盘可达数秒——
    /// 挪到常驻 STA 线程执行，UI 线程只做快查与结果应用。Global 方向无 I/O，
    /// 保持同步切换。
    /// </summary>
    private void ToggleSearchScope()
    {
        if (_scope.Scope == SearchScope.CurrentDirectory)
        {
            _scope.ToggleScope();
            return;
        }

        var workload = _scope.PrepareToggleToCurrentDirectory(out var capturedWindow);
        if (workload is null) return;
        _ = RunToggleValidationAsync(workload, capturedWindow);
    }

    private async Task RunToggleValidationAsync(
        Func<HostScopeController.ScopeToggleValidation> workload,
        IntPtr capturedWindow)
    {
        HostScopeController.ScopeToggleValidation result;
        try
        {
            result = await RunOnStaThread(workload).ConfigureAwait(true);
        }
        catch
        {
            result = new HostScopeController.ScopeToggleValidation(
                false, HostDetectionStatus.DetectFailed);
        }
        _ = Dispatcher.BeginInvoke(() => _scope.ApplyToggleValidation(capturedWindow, result));
    }

    private void OnScopeChanged()
    {
        if (!Dispatcher.CheckAccess())
        {
            // BeginInvoke：OnScopeChanged 可能在持控制器状态锁的后台线程上触发，
            // 同步 Invoke 会与 UI 线程的锁获取互等。FIFO 派发保序即可。
            Dispatcher.BeginInvoke(OnScopeChanged);
            return;
        }
        ApplyScopeUi();
    }

    private void ApplyScopeUi()
    {
        // 小问题 Q2：网页搜索与目录范围无关，模式下隐藏范围标签（仅表现层）。
        var scopeVisible = _scope.IsScopeLabelVisible
            && !(_vm is { State.IsWebMode: true });
        Header.SetScope(scopeVisible, _scope.ScopeLabel, _scope.ScopeTooltip);
        ScopeNotice.Text = _scope.Notice;
        ScopeNotice.Visibility = string.IsNullOrEmpty(_scope.Notice)
            ? Visibility.Collapsed
            : Visibility.Visible;
        _vm?.SetScopeRoot(_scope.Root);
        UpdateCardClip();
    }

    /// <summary>
    /// 在呼出前的前台窗口所在显示器居中、顶部 25%——副屏工作时不再跳回主屏。
    /// 采样失败（零句柄/枚举失败）回退主屏工作区，行为与旧版一致。
    /// </summary>
    private void PositionWindow(IntPtr foreground)
    {
        var screen = ForegroundInterop.GetWorkAreaForWindow(foreground);
        Left = screen.Left + (screen.Width - Width) / 2;
        Top = screen.Top + screen.Height * 0.25;
    }

    private void OnHeaderQueryChanged(string text)
    {
        if (_suppressQueryEvent || _vm is null) return;
        _vm.OnQueryChanged(text);
    }

    private async void OnHeaderKeyDown(KeyEventArgs e)
    {
        if (_vm is null) return;

        // 动作快捷键（2026-08-21 设想）：先于输入/导航查用户表；保留键（导航/
        // Ctrl+Enter/Ctrl+G/Ctrl+数字）永远进不去用户表，原有按键行为零改动。
        // Alt 组合经 Key.System 到达，取 SystemKey 再匹配（与录键框同规则）。
        // 适用性（模式/类型/ExecuteId）同步预检：不适用则不吞键、不执行，
        // 按键原样落回原逻辑（与「类型不适用忽略」的设想一致）。
        var hotkeyKey = e.Key == Key.System ? e.SystemKey : e.Key;
        if (!ActionHotkeyTable.IsReserved(hotkeyKey, Keyboard.Modifiers)
            && _actionHotkeys.TryMatch(hotkeyKey, Keyboard.Modifiers, out var actionId)
            && ActionShortcutApplies(actionId))
        {
            // 与 Key.Enter 同纪律：先置 Handled 再 await。
            e.Handled = true;
            await RunActionShortcutAsync(actionId);
            return;
        }

        var mode = _vm.State.Mode;

        switch (e.Key)
        {
            case Key.Up:
                _vm.MoveSelection(-1);
                SyncSelectionToList();
                e.Handled = true;
                break;
            case Key.Down:
                _vm.MoveSelection(1);
                SyncSelectionToList();
                e.Handled = true;
                break;
            case Key.Enter:
                // G3（FRESH-AUDIT-2）：先置 Handled 再 await——await 让出后按键路由已经
                // 冒泡完毕，事后置位无效，Enter 可能被其他元素二次消费。
                e.Handled = true;
                if (mode == PanelMode.Actions)
                {
                    await _vm.ExecuteActionAsync();
                }
                else if (Keyboard.Modifiers.HasFlag(ModifierKeys.Control))
                {
                    // G4：Ctrl+Enter 优先在原宿主定位；无宿主时保持 broker reveal。
                    await RevealSelectedPreferringHostAsync();
                }
                else
                {
                    // 普通 Enter 始终走 broker 打开，绝不自动导航宿主或确认对话框。
                    await _vm.ExecuteSelectedAsync();
                }
                break;
            case Key.Escape:
                HandleEscape();
                e.Handled = true;
                break;
            case Key.Right:
                // 仅当当前选中 file/folder 时吞掉 → 并进入动作；
                // 否则留给 TextBox 移动光标（app/web/more 或无选中）。
                if (mode == PanelMode.Results
                    && Keyboard.Modifiers == ModifierKeys.None
                    && IsActionableSelection(_vm.State.SelectedResult))
                {
                    // 同 Enter：先置 Handled 再 await（见 Key.Enter 注释）。
                    e.Handled = true;
                    await EnterActionsUiAsync();
                }
                break;
            case Key.Left:
                if (mode == PanelMode.Actions && Keyboard.Modifiers == ModifierKeys.None)
                {
                    LeaveActionsUi();
                    e.Handled = true;
                }
                break;
            case Key.G:
                if (Keyboard.Modifiers.HasFlag(ModifierKeys.Control))
                {
                    ToggleSearchScope();
                    e.Handled = true;
                }
                break;
            case Key.D1: case Key.NumPad1: await CtrlNumber(1, e); break;
            case Key.D2: case Key.NumPad2: await CtrlNumber(2, e); break;
            case Key.D3: case Key.NumPad3: await CtrlNumber(3, e); break;
            case Key.D4: case Key.NumPad4: await CtrlNumber(4, e); break;
            case Key.D5: case Key.NumPad5: await CtrlNumber(5, e); break;
            case Key.D6: case Key.NumPad6: await CtrlNumber(6, e); break;
            case Key.D7: case Key.NumPad7: await CtrlNumber(7, e); break;
            case Key.D8: case Key.NumPad8: await CtrlNumber(8, e); break;
            case Key.D9: case Key.NumPad9: await CtrlNumber(9, e); break;
        }
    }

    private static bool IsActionableSelection(SearchResult? item) =>
        item is { Kind: "file" or "folder" } && !string.IsNullOrEmpty(item.ExecuteId);

    /// <summary>
    /// 动作快捷键的同步适用性预检：Results 模式 + 选中行类型适用 + 有执行 id。
    /// 预检通过后才置 Handled 并进入执行；不适用按设想忽略（键不吞、走原逻辑）。
    /// </summary>
    private bool ActionShortcutApplies(string actionId)
    {
        var target = _vm?.State.SelectedResult;
        return _vm?.State.Mode == PanelMode.Results
            && target is not null
            && !string.IsNullOrEmpty(target.ExecuteId)
            && ActionHotkeyCatalog.AppliesTo(actionId, target.Kind);
    }

    /// <summary>
    /// 动作快捷键命中后的执行入口（2026-08-21 设想）。与动作面板 Enter、
    /// 右键菜单共用 <see cref="SearchViewModel.RunActionOnAsync"/>——成功隐藏、
    /// mutation 刷新、copy_to/move_to 模态失活守卫、错误文案全部继承。
    /// 调用前已经过 <see cref="ActionShortcutApplies"/> 预检；这里的守卫是
    /// await 期间状态可能变化的防御性复核。
    /// rename 特殊：内联编辑需要动作面板态，与右键菜单同款序列——先进面板
    /// 再选中 rename 执行。
    /// </summary>
    private async Task RunActionShortcutAsync(string actionId)
    {
        if (!ActionShortcutApplies(actionId)) return;
        var target = _vm!.State.SelectedResult!;

        if (actionId == "rename")
        {
            var idx = IndexOfResult(target);
            if (idx >= 0) _vm.State.SelectedIndex = idx;
            await EnterActionsUiAsync().ConfigureAwait(true);
            if (_vm.State.Mode != PanelMode.Actions) return;
            if (!TrySelectAction("rename"))
            {
                // broker 对该目标未返回 rename：静默退回结果，绝不执行其他动作。
                LeaveActionsUi();
                return;
            }
            await _vm.ExecuteActionAsync().ConfigureAwait(true);
            return;
        }

        await _vm.RunActionOnAsync(target, ActionHotkeyCatalog.ToActionItem(actionId))
            .ConfigureAwait(true);
    }

    /// <summary>在当前动作面板列表中选中指定 id 的动作；未找到返回 false。</summary>
    private bool TrySelectAction(string actionId)
    {
        if (_vm is null) return false;
        var actions = _vm.State.Actions;
        for (var i = 0; i < actions.Count; i++)
        {
            if (actions[i].Id == actionId && !actions[i].IsSectionHeader)
            {
                _vm.State.SelectedActionIndex = i;
                return true;
            }
        }
        return false;
    }

    /// <summary>
    /// Ctrl+Enter：有可用原宿主时交回 <see cref="IHostAdapter.NavigateOrFill"/>；
    /// 否则沿用 broker <c>explorer /select</c>。定位失败保留 Prism 结果，不清空 root
    /// （除非宿主已消失/提权，由 <see cref="HostScopeController.TryRevealInHost"/> 处理）。
    /// </summary>
    private async Task RevealSelectedPreferringHostAsync()
    {
        if (_vm is null) return;
        if (_vm.State.Mode == PanelMode.Actions) return;

        var item = _vm.State.SelectedResult;
        if (item is null
            || item.Kind is not ("file" or "folder")
            || string.IsNullOrWhiteSpace(item.ExecuteId))
        {
            await _vm.RevealSelectedAsync().ConfigureAwait(true);
            return;
        }

        var path = item.ExecutionTarget.Value;
        if (string.IsNullOrWhiteSpace(path))
            path = item.ExecuteId;

        // 宿主定位同样在后台 STA 线程执行：Explorer 定位含最长 2 秒的轮询、
        // Opus 是子进程等待，绝不能挂在 UI 线程上。Shell COM 是 STA-only，
        // 线程池 MTA 线程上会静默失败。
        HostRevealResult reveal;
        try
        {
            reveal = await RunOnStaThread(() =>
                _scope.TryRevealInHost(path, isDirectory: item.Kind == "folder")).ConfigureAwait(true);
        }
        catch
        {
            reveal = HostRevealResult.Failure(HostFailureReason.ActionFailed);
        }
        if (!reveal.Attempted)
        {
            await _vm.RevealSelectedAsync().ConfigureAwait(true);
            return;
        }

        if (reveal.Succeeded)
        {
            HideAnimated();
            return;
        }

        // 失败：保留结果列表；HostGone/Elevated 已由 controller Invalidate。
        _vm.State.StatusMessage = HostRevealStatusMessage(reveal.Reason);
    }

    private static string HostRevealStatusMessage(HostFailureReason reason) => reason switch
    {
        HostFailureReason.HostGone => "原窗口已关闭，宿主定位失败",
        HostFailureReason.HostElevated => "无法控制提权宿主，宿主定位失败",
        HostFailureReason.AccessDenied => "宿主定位失败：访问被拒绝",
        HostFailureReason.Unsupported => "当前宿主不支持定位该结果",
        _ => "宿主定位失败",
    };

    private async Task EnterActionsUiAsync()
    {
        if (_vm is null) return;
        await _vm.EnterActionsAsync().ConfigureAwait(true);
        if (_vm.State.Mode != PanelMode.Actions) return;

        _suppressQueryEvent = true;
        Header.ClearQuery();
        Header.SetMode(PanelMode.Actions);
        _suppressQueryEvent = false;
        ApplyState(_vm.State, animatePanel: true);
        Header.FocusQuery();
    }

    private async Task CtrlNumber(int n, KeyEventArgs e)
    {
        if (_vm is null || !Keyboard.Modifiers.HasFlag(ModifierKeys.Control)) return;
        if (_vm.State.Mode == PanelMode.Actions) return;
        // 同 Enter：先置 Handled 再 await（见 Key.Enter 注释）。
        e.Handled = true;
        await _vm.ExecuteIndexAsync(n);
    }

    private void OnResultsSelected(int index)
    {
        if (_vm is null) return;
        _vm.State.SelectedIndex = index;
    }

    private void OnStateChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (!Dispatcher.CheckAccess())
        {
            // L 批次（FRESH-AUDIT-3-2026-08-20）：BeginInvoke——后台线程的属性变更
            // 不能阻塞等待 UI，否则与 ViewModel 的通知路径互等。
            Dispatcher.BeginInvoke(() => OnStateChanged(sender, e));
            return;
        }

        // IsPinned 同步到按钮（不经 Pin 自己的事件环）。
        if (e.PropertyName == nameof(AppState.IsPinned) && _vm is not null)
        {
            if (Pin.IsPinned != _vm.State.IsPinned)
                Pin.IsPinned = _vm.State.IsPinned;
        }

        if (e.PropertyName == nameof(AppState.Query))
            SyncGenerationPolling();

        // 小问题 Q2：网页模式进出时刷新范围标签可见性（ApplyScopeUi 读取 IsWebMode）。
        if (e.PropertyName == nameof(AppState.IsWebMode))
            ApplyScopeUi();

        ApplyState(_vm?.State, animatePanel: true);
    }

    private void SyncGenerationPolling()
    {
        var active = IsVisible
            && !_hiding
            && _vm is not null
            && !string.IsNullOrWhiteSpace(_vm.State.Query);
        _generationClient.SetActive(active);
    }

    private void OnThemeApplied()
    {
        if (!Dispatcher.CheckAccess())
        {
            // L 批次（FRESH-AUDIT-3-2026-08-20）：BeginInvoke——主题应用的回报
            // 不能阻塞等待 UI 线程。
            Dispatcher.BeginInvoke(OnThemeApplied);
            return;
        }
        Results.InvalidateThemeBrushes();
        Pin.RefreshTheme();
    }

    private void ApplyState(AppState? state, bool animatePanel)
    {
        if (state is null) return;

        Header.SetMode(state.Mode);

        var showResults = state.Mode is PanelMode.Results or PanelMode.Idle
            && (!string.IsNullOrEmpty(state.Query)
                || state.Results.Count > 0
                || !string.IsNullOrEmpty(state.StatusMessage));
        var showActions = state.Mode == PanelMode.Actions;

        // 高度动画只服务面板状态机跃迁（Results↔Actions，用户按 →/← 的主动切换）。
        // 查询驱动的内容增减——面板首现、行数/状态行变化——是内容搅动而非布局意图：
        // 50ms 防抖下每批结果重定向一次 100ms 动画，面板永远在动，还把
        // SizeToContent 的原生窗口 resize 摊进每一帧。内容变化直接落值。
        // （None→Results 首现不动画：首个结果批之后紧跟第二批通知，展开动画
        // 总会在半途被落值打断，与其制造"半展开+跳变"，不如一次到位。）
        var kind = showActions ? PanelKind.Actions
            : showResults ? PanelKind.Results
            : PanelKind.None;
        var animate = animatePanel
            && kind != _lastPanelKind
            && (kind == PanelKind.Actions || _lastPanelKind == PanelKind.Actions);
        _lastPanelKind = kind;

        // 分隔线从隐藏变可见时淡入（隐藏仍瞬时收起）。
        var showDivider = showResults || showActions;
        if (showDivider && Divider.Visibility != Visibility.Visible)
        {
            Divider.Visibility = Visibility.Visible;
            Divider.BeginAnimation(
                OpacityProperty,
                new DoubleAnimation(0, 1, TimeSpan.FromMilliseconds(100))
                {
                    EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
                });
        }
        else if (!showDivider && Divider.Visibility == Visibility.Visible)
        {
            Divider.Visibility = Visibility.Collapsed;
        }

        if (showActions)
        {
            Results.Visibility = Visibility.Collapsed;
            Actions.Visibility = Visibility.Visible;
            if (!ReferenceEquals(Actions.Items, state.Actions))
                Actions.Items = state.Actions;
            if (Actions.SelectedIndex != state.SelectedActionIndex)
                Actions.SelectedIndex = state.SelectedActionIndex;
            // Results 的 StatusMessage 在 Actions 态隐藏，改走底部 ActionStatus。
            Results.StatusMessage = "";
            AnimatePanelHeight(Actions.Height > 0 ? Actions.Height : Actions.MinHeight, animate);
            SetActionStatus(state.StatusMessage);
        }
        else if (showResults)
        {
            Actions.Visibility = Visibility.Collapsed;
            Results.Visibility = Visibility.Visible;
            if (!ReferenceEquals(Results.Items, state.Results))
                Results.Items = state.Results;
            if (Results.SelectedIndex != state.SelectedIndex)
                Results.SelectedIndex = state.SelectedIndex;
            Results.StatusMessage = state.StatusMessage;
            AnimatePanelHeight(Results.Height > 0 ? Results.Height : Results.MinHeight, animate);
            SetActionStatus("");
        }
        else
        {
            Results.Visibility = Visibility.Collapsed;
            Actions.Visibility = Visibility.Collapsed;
            Results.StatusMessage = state.StatusMessage;
            AnimatePanelHeight(0, animate);
            SetActionStatus("");
        }

        UpdateCardClip();
    }

    private void SetActionStatus(string? message)
    {
        var text = message ?? "";
        ActionStatus.Text = text;
        if (string.IsNullOrEmpty(text))
        {
            // A5：消失瞬时，防按键周期新闪烁。
            ActionStatus.BeginAnimation(OpacityProperty, null);
            ActionStatus.Opacity = 1;
            ActionStatus.Visibility = Visibility.Collapsed;
        }
        else
        {
            // A5：出现时淡入 100ms（EaseOut，与分隔线/面板展开同一节奏）。
            ActionStatus.Visibility = Visibility.Visible;
            ActionStatus.BeginAnimation(OpacityProperty,
                new DoubleAnimation(0, 1, TimeSpan.FromMilliseconds(100))
                {
                    EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
                });
        }
    }

    private void AnimatePanelHeight(double target, bool animate)
    {
        if (double.IsNaN(target) || target < 0) target = 0;

        // 动画在途且目的地未变：让它跑完。AppState 一次响应以通知突发到达，
        // 重启会把跃迁展开动画打断成抖动；落值路径也不得掐断它——目的地相同，
        // 让动画自然收尾与直接落值观感一致且更平滑。
        if (Math.Abs(_panelTargetHeight - target) < 0.5
            && (!animate || !IsVisible || _heightAnimating))
        {
            return;
        }

        if (!animate || !IsVisible)
        {
            // 内容驱动的高度变化直接落值（见 ApplyState 的 animate 判定）。
            // BeginAnimation(null) 同时是在途动画的取消：移除时钟不触发 Completed，
            // 必须在这里复位标记。
            _heightAnimating = false;
            _panelTargetHeight = target;
            PanelHost.BeginAnimation(HeightProperty, null);
            PanelHost.Height = target;
            return;
        }

        _panelTargetHeight = target;

        var from = double.IsNaN(PanelHost.Height) ? 0 : PanelHost.ActualHeight;
        if (from <= 0 && PanelHost.Visibility == Visibility.Visible)
            from = PanelHost.ActualHeight;
        _heightAnimating = true;
        var anim = new DoubleAnimation(from, target, TimeSpan.FromMilliseconds(PanelExpandMs))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            FillBehavior = FillBehavior.Stop,
        };
        anim.Completed += (_, _) =>
        {
            _heightAnimating = false;
            // FillBehavior.Stop 在完成后把值交还基值，这里以最终目标落定。
            PanelHost.Height = _panelTargetHeight;
        };
        PanelHost.BeginAnimation(HeightProperty, anim);
    }

    private void UpdateCardClip()
    {
        if (CardClip is null || Card is null) return;
        var w = Card.ActualWidth;
        var h = Card.ActualHeight;
        if (w <= 0 || h <= 0) return;
        CardClip.Rect = new Rect(0, 0, w, h);
    }

    private void SyncSelectionToList()
    {
        if (_vm is null) return;
        if (_vm.State.Mode == PanelMode.Actions)
            Actions.SelectedIndex = _vm.State.SelectedActionIndex;
        else
            Results.SelectedIndex = _vm.State.SelectedIndex;
    }

    private int IndexOfResult(SearchResult r)
    {
        if (_vm is null) return -1;
        for (var i = 0; i < _vm.State.Results.Count; i++)
        {
            var cur = _vm.State.Results[i];
            if (ReferenceEquals(cur, r)) return i;
            if (cur.Kind == r.Kind
                && cur.ExecuteId == r.ExecuteId
                && cur.Title == r.Title)
                return i;
        }
        return -1;
    }
}
