using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
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
    private bool _suppressQueryEvent;
    private bool _hiding;
    /// <summary>呼出后短时间内忽略失焦，避免 Show/Activate 过程中被立刻关掉。</summary>
    private bool _ignoreDeactivate;
    private bool _contextMenuOpen;
    private bool _contextMenuActionPending;
    private int _contextMenuRequestSeq;
    private double _panelTargetHeight;

    // ── 强制前台 ──
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, IntPtr pid);
    [DllImport("user32.dll")] private static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
    private const int SW_SHOW = 5;

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
        SizeChanged += (_, _) => UpdateCardClip();
        Loaded += (_, _) => UpdateCardClip();
        Closed += (_, _) => _generationClient.Dispose();
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

    /// <summary>由 App 在启动时注入 ViewModel、图标缓存与主题监听。</summary>
    public void Attach(SearchViewModel vm, IconCache icons, ThemeWatcher? theme = null)
    {
        _vm = vm;
        _icons = icons;
        _theme = theme;
        Results.SetIconCache(icons);
        vm.HideRequested += () =>
        {
            if (Dispatcher.CheckAccess()) HideAnimated();
            else Dispatcher.Invoke(HideAnimated);
        };
        vm.State.PropertyChanged += OnStateChanged;
        vm.RootRejected += rejection =>
        {
            if (Dispatcher.CheckAccess()) _scope.Invalidate(rejection);
            else Dispatcher.Invoke(() => _scope.Invalidate(rejection));
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
        var foreground = GetForegroundWindow();

        _hiding = false;
        _ignoreDeactivate = true;
        BeginAnimation(OpacityProperty, null);

        _vm?.ResetForShow();
        _suppressQueryEvent = true;
        Header.ClearQuery();
        Header.SetMode(PanelMode.Idle);
        _suppressQueryEvent = false;
        // 每次呼出重新识别宿主，绝不沿用上一次目录。
        _scope.Capture(foreground);
        ApplyScopeUi();
        ApplyState(_vm?.State, animatePanel: false);

        PositionWindow();
        Opacity = 0;
        Show();
        SyncGenerationPolling();
        ForceActivate();
        Topmost = true;
        Topmost = false;
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

            ShowWindow(hwnd, SW_SHOW);
            BringWindowToTop(hwnd);

            var foreground = GetForegroundWindow();
            if (foreground == hwnd)
            {
                SetForegroundWindow(hwnd);
                return;
            }

            var foreThread = GetWindowThreadProcessId(foreground, IntPtr.Zero);
            var appThread = GetCurrentThreadId();
            if (foreThread != appThread && foreThread != 0)
            {
                AttachThreadInput(appThread, foreThread, true);
                try
                {
                    BringWindowToTop(hwnd);
                    SetForegroundWindow(hwnd);
                }
                finally
                {
                    AttachThreadInput(appThread, foreThread, false);
                }
            }
            else
            {
                SetForegroundWindow(hwnd);
            }

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
        var fadeOut = new DoubleAnimation(Opacity, 0, TimeSpan.FromMilliseconds(FadeOutMs));
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
            _icons?.Clear();
            Dispatcher.BeginInvoke(() =>
            {
                GC.Collect(2, GCCollectionMode.Optimized, blocking: false, compacting: true);
                App.TrimWorkingSet();
            }, System.Windows.Threading.DispatcherPriority.ApplicationIdle);
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

        var separatorPending = false;
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
                        // 先选中目标结果，再进入动作面板。
                        var idx = IndexOfResult(target);
                        if (idx >= 0) _vm.State.SelectedIndex = idx;
                        await EnterActionsUiAsync().ConfigureAwait(true);
                        // 进入面板后手动选中 rename 动作并执行。
                        var actions = _vm.State.Actions;
                        for (var i = 0; i < actions.Count; i++)
                        {
                            if (actions[i].Id == "rename" && !actions[i].IsSectionHeader)
                            {
                                _vm.State.SelectedActionIndex = i;
                                break;
                            }
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

    /// <summary>范围标签点击或 `Ctrl+G`：在当前目录与全局之间切换。</summary>
    private void ToggleSearchScope() => _scope.ToggleScope();

    private void OnScopeChanged()
    {
        if (!Dispatcher.CheckAccess())
        {
            Dispatcher.Invoke(OnScopeChanged);
            return;
        }
        ApplyScopeUi();
    }

    private void ApplyScopeUi()
    {
        Header.SetScope(_scope.IsScopeLabelVisible, _scope.ScopeLabel, _scope.ScopeTooltip);
        ScopeNotice.Text = _scope.Notice;
        ScopeNotice.Visibility = string.IsNullOrEmpty(_scope.Notice)
            ? Visibility.Collapsed
            : Visibility.Visible;
        _vm?.SetScopeRoot(_scope.Root);
        UpdateCardClip();
    }

    private void PositionWindow()
    {
        var screen = SystemParameters.WorkArea;
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
                e.Handled = true;
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
                    await EnterActionsUiAsync();
                    e.Handled = true;
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

        var reveal = _scope.TryRevealInHost(path, isDirectory: item.Kind == "folder");
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
        await _vm.ExecuteIndexAsync(n);
        e.Handled = true;
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
            Dispatcher.Invoke(() => OnStateChanged(sender, e));
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
            Dispatcher.Invoke(OnThemeApplied);
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

        Divider.Visibility = (showResults || showActions) ? Visibility.Visible : Visibility.Collapsed;

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
            AnimatePanelHeight(Actions.Height > 0 ? Actions.Height : Actions.MinHeight, animatePanel);
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
            AnimatePanelHeight(Results.Height > 0 ? Results.Height : Results.MinHeight, animatePanel);
            SetActionStatus("");
        }
        else
        {
            Results.Visibility = Visibility.Collapsed;
            Actions.Visibility = Visibility.Collapsed;
            Results.StatusMessage = state.StatusMessage;
            AnimatePanelHeight(0, animatePanel);
            SetActionStatus("");
        }

        UpdateCardClip();
    }

    private void SetActionStatus(string? message)
    {
        var text = message ?? "";
        ActionStatus.Text = text;
        ActionStatus.Visibility = string.IsNullOrEmpty(text)
            ? Visibility.Collapsed
            : Visibility.Visible;
    }

    private void AnimatePanelHeight(double target, bool animate)
    {
        if (double.IsNaN(target) || target < 0) target = 0;
        if (Math.Abs(_panelTargetHeight - target) < 0.5)
        {
            // AppState raises several notifications for one response. Do not
            // restart an in-flight animation when its destination is unchanged.
            if (!animate || !IsVisible)
            {
                PanelHost.BeginAnimation(HeightProperty, null);
                PanelHost.Height = target;
            }
            return;
        }
        _panelTargetHeight = target;

        if (!animate || !IsVisible)
        {
            PanelHost.BeginAnimation(HeightProperty, null);
            PanelHost.Height = target;
            return;
        }

        var from = double.IsNaN(PanelHost.Height) ? 0 : PanelHost.ActualHeight;
        if (from <= 0 && PanelHost.Visibility == Visibility.Visible)
            from = PanelHost.ActualHeight;
        var anim = new DoubleAnimation(from, target, TimeSpan.FromMilliseconds(PanelExpandMs))
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            FillBehavior = FillBehavior.Stop,
        };
        anim.Completed += (_, _) =>
        {
            PanelHost.Height = target;
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
