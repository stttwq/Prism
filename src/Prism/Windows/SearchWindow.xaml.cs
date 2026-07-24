using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media.Animation;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;

namespace Prism.Windows;

/// <summary>
/// 主搜索窗口（无边框、置顶、宽 660px，顶部距屏幕 25% 高度）。
/// 第四步：接入 SearchHeader / ResultList / SearchViewModel，完成输入→搜索→打开链路。
/// </summary>
public partial class SearchWindow : Window
{
    private const double FadeInMs = 120;
    private const double FadeOutMs = 80;
    private const double SlideOffsetPx = 6;

    private SearchViewModel? _vm;
    private bool _suppressQueryEvent;
    private bool _hiding;
    /// <summary>呼出后短时间内忽略失焦，避免 Show/Activate 过程中被立刻关掉。</summary>
    private bool _ignoreDeactivate;

    // ── 强制前台（启动自终端时 Activate 经常失败，导致空态 Esc/失焦都不生效）──
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hWnd, IntPtr pid);
    [DllImport("user32.dll")] private static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
    private const int SW_SHOW = 5;

    public SearchWindow()
    {
        InitializeComponent();
        // 窗口级隧道键：即使焦点不在 TextBox（或 TextBox 未拿到键盘焦点），Esc 也能隐藏。
        PreviewKeyDown += OnWindowPreviewKeyDown;
        Deactivated += OnDeactivated;

        Header.QueryChanged += OnHeaderQueryChanged;
        Header.QueryKeyDown += OnHeaderKeyDown;
        Results.SelectedIndexChanged += OnResultsSelected;
        Results.ItemInvoked += async r =>
        {
            if (_vm is null) return;
            var idx = IndexOfResult(r);
            if (idx >= 0) _vm.State.SelectedIndex = idx;
            await _vm.ExecuteSelectedAsync();
        };
    }

    /// <summary>由 App 在启动时注入 ViewModel 与图标缓存。</summary>
    public void Attach(SearchViewModel vm, IconCache icons)
    {
        _vm = vm;
        Results.SetIconCache(icons);
        vm.HideRequested += () =>
        {
            if (Dispatcher.CheckAccess()) HideAnimated();
            else Dispatcher.Invoke(HideAnimated);
        };
        vm.State.PropertyChanged += OnStateChanged;
    }

    public bool IsPinned
    {
        get => _vm?.State.IsPinned ?? false;
        set { if (_vm is not null) _vm.State.IsPinned = value; }
    }

    /// <summary>呼出：重置状态、定位、淡入、焦点到输入框。</summary>
    public void ShowAndFocus()
    {
        _hiding = false;
        _ignoreDeactivate = true;
        BeginAnimation(OpacityProperty, null); // 取消进行中的淡出

        _vm?.ResetForShow();
        _suppressQueryEvent = true;
        Header.ClearQuery();
        _suppressQueryEvent = false;
        ApplyState(_vm?.State);

        PositionWindow();
        Opacity = 0;
        Show();
        ForceActivate();
        Topmost = true; // 确保浮在最前
        // 某些情况下需要"闪一下" Topmost 才能压过其它置顶窗。
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
        var transform = new System.Windows.Media.TranslateTransform();
        RenderTransform = transform;

        BeginAnimation(OpacityProperty, fadeIn);
        transform.BeginAnimation(System.Windows.Media.TranslateTransform.YProperty, slideIn);

        Dispatcher.BeginInvoke(() =>
        {
            ForceActivate();
            Header.FocusQuery();
            // 约 300ms 后再响应失焦隐藏，躲开启动/焦点切换的瞬时 Deactivated。
            var t = new System.Windows.Threading.DispatcherTimer
            {
                Interval = TimeSpan.FromMilliseconds(300),
            };
            t.Tick += (_, _) => { t.Stop(); _ignoreDeactivate = false; };
            t.Start();
        }, System.Windows.Threading.DispatcherPriority.Input);
    }

    /// <summary>
    /// 尽可能抢到前台焦点。从 `dotnet run`/终端拉起时，普通 Activate() 常被系统拒绝，
    /// 窗口看得见但不是前台——此时 Esc 进不来、点别处也不会 Deactivated。
    /// </summary>
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

            // AttachThreadInput 技巧：把当前线程与前台线程输入队列临时绑在一起，绕过前台限制。
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
            // 抢焦点失败不致命，用户点一下输入框仍可继续用。
            try { Activate(); } catch { /* ignore */ }
        }
    }

    public void HideAnimated()
    {
        if (_hiding || !IsVisible) return;
        _hiding = true;
        _ignoreDeactivate = true; // 隐藏过程中的失焦不再重入
        var fadeOut = new DoubleAnimation(Opacity, 0, TimeSpan.FromMilliseconds(FadeOutMs));
        fadeOut.Completed += (_, _) =>
        {
            Hide();
            _hiding = false;
            // 隐藏后清掉键盘焦点，避免下次呼出时焦点状态错乱。
            try { Keyboard.ClearFocus(); } catch { /* ignore */ }
        };
        BeginAnimation(OpacityProperty, fadeOut);
    }

    private void OnWindowPreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Escape)
        {
            HideAnimated();
            e.Handled = true;
        }
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        base.OnKeyDown(e);
        if (e.Key == Key.Escape)
        {
            HideAnimated();
            e.Handled = true;
        }
    }

    private void OnDeactivated(object? sender, EventArgs e)
    {
        if (_ignoreDeactivate || IsPinned || _hiding) return;
        // 空态 / 有内容 一视同仁：失焦即藏。
        HideAnimated();
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
                if (Keyboard.Modifiers.HasFlag(ModifierKeys.Control))
                    await _vm.RevealSelectedAsync();
                else
                    await _vm.ExecuteSelectedAsync();
                e.Handled = true;
                break;
            case Key.Escape:
                HideAnimated();
                e.Handled = true;
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

    private async Task CtrlNumber(int n, KeyEventArgs e)
    {
        if (_vm is null || !Keyboard.Modifiers.HasFlag(ModifierKeys.Control)) return;
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
        ApplyState(_vm?.State);
    }

    private void ApplyState(AppState? state)
    {
        if (state is null) return;

        // 有输入 / 有结果 / 有状态提示时展开列表区。
        var showList = !string.IsNullOrEmpty(state.Query)
            || state.Results.Count > 0
            || !string.IsNullOrEmpty(state.StatusMessage);

        Divider.Visibility = showList ? Visibility.Visible : Visibility.Collapsed;
        Results.Visibility = showList ? Visibility.Visible : Visibility.Collapsed;

        if (!ReferenceEquals(Results.Items, state.Results))
            Results.Items = state.Results;
        if (Results.SelectedIndex != state.SelectedIndex)
            Results.SelectedIndex = state.SelectedIndex;

        // 无结果时显示空态/索引提示；有结果时仍显示"打开失败"等错误。
        Results.StatusMessage = state.StatusMessage;
    }

    private void SyncSelectionToList()
    {
        if (_vm is null) return;
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
