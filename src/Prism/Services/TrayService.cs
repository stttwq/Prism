using System.Drawing;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Threading;
using System.Windows.Forms;
using System.Windows.Interop;
// NotifyIcon 在 WinForms 命名空间；WPF Application 由 GlobalUsings 统一别名。

namespace Prism.Services;

/// <summary>
/// 系统托盘图标与右键菜单（frontend-spec：打开设置 / 重建索引 / 退出）。
/// 左键单击托盘图标请求呼出搜索框。右键弹出 WPF ContextMenu（深色主题统一）。
/// 必须在 UI 线程创建与销毁；事件回调统一派发到 WPF Dispatcher。
/// </summary>
public sealed class TrayService : IDisposable
{
    private readonly NotifyIcon _notify;
    private readonly Dispatcher _dispatcher;
    private readonly Icon? _ownedIcon;
    private bool _disposed;

    // 右键菜单用 WPF ContextMenu（主题资源统一）。懒创建，首次右键时建。
    private ContextMenu? _menu;
    // 托盘弹 WPF 菜单的锚点窗（防"点外面不关闭"标准手法）。
    private Window? _anchor;
    private bool _anchorShown;

    /// <summary>用户选择「打开设置」。</summary>
    public event Action? OpenSettingsRequested;

    /// <summary>用户选择「重建索引」。</summary>
    public event Action? RebuildIndexRequested;

    /// <summary>用户选择「退出」。</summary>
    public event Action? ExitRequested;

    /// <summary>左键单击托盘，请求显示/切换搜索框。</summary>
    public event Action? ShowSearchRequested;

    public TrayService()
    {
        // 构造须在 UI 线程；记下 Dispatcher 供 WinForms 托盘回调回投。
        _dispatcher = Application.Current?.Dispatcher
            ?? throw new InvalidOperationException("TrayService 须在 WPF Application 启动后创建");

        _ownedIcon = LoadIcon();
        _notify = new NotifyIcon
        {
            Text = "Prism",
            Icon = _ownedIcon,
            // 不再设 ContextMenuStrip——右键由 OnMouseClick 弹 WPF ContextMenu。
            Visible = true,
        };
        _notify.MouseClick += OnMouseClick;
    }

    /// <summary>更新托盘气泡提示文字（可在状态变化时调用）。</summary>
    public void SetTooltip(string text)
    {
        if (_disposed) return;
        // NotifyIcon.Text 最长 63 字符。
        void Apply()
        {
            if (_disposed) return;
            _notify.Text = text.Length <= 63 ? text : text[..63];
        }

        if (_dispatcher.CheckAccess())
            Apply();
        else
            _dispatcher.BeginInvoke(Apply);
    }

    private void OnMouseClick(object? sender, MouseEventArgs e)
    {
        if (e.Button == MouseButtons.Left)
        {
            // 同线程直调分支绕过 WPF Dispatcher 兜底：包一层 try/catch 兜底（5.4），
            // 否则异常沿 WinForms NotifyIcon.WndProc 原生回调栈上抛直接杀进程。
            try { Raise(ShowSearchRequested); }
            catch (Exception ex) { Prism.App.LogException("TrayService.OnMouseClick", ex); }
        }
        else if (e.Button == MouseButtons.Right)
        {
            ShowContextMenu();
        }
    }

    /// <summary>弹出 WPF 右键菜单（托盘标准手法：锚点窗 + SetForegroundWindow）。</summary>
    private void ShowContextMenu()
    {
        if (_disposed) return;
        if (_dispatcher.CheckAccess())
            ShowContextMenuOnDispatcher();   // 方法体内已有 5.4 兜底
        else
            _dispatcher.BeginInvoke(ShowContextMenuOnDispatcher);
    }

    private void ShowContextMenuOnDispatcher()
    {
        if (_disposed) return;

        try
        {
            _menu ??= BuildMenu();

            // 锚点窗：0 尺寸、无任务栏、离屏、透明。首次需 Show 一次让 HWND 就绪，
            // 之后 SetForegroundWindow 让它获得激活态——这是托盘弹窗菜单
            // 点击外部正常收起的标准条件。
            // 5.3：透明背景（否则系统默认白），并加扩展样式让它不进 Alt+Tab / Win+Tab。
            if (_anchor is null)
            {
                _anchor = new Window
                {
                    Width = 0,
                    Height = 0,
                    WindowStyle = WindowStyle.None,
                    ShowInTaskbar = false,
                    AllowsTransparency = true,
                    Background = System.Windows.Media.Brushes.Transparent,
                    Visibility = Visibility.Hidden,
                    Left = -32000,
                    Top = -32000,
                };
                // 5.2：锚点窗被外部关闭后清空状态，下次重建，避免复用死对象（崩溃根治点）。
                _anchor.Closed += (_, _) => { _anchor = null; _anchorShown = false; };
                // 5.3：WS_EX_TOOLWINDOW 去 Alt+Tab/任务视图条目；WS_EX_NOACTIVATE 防抢焦点。
                _anchor.SourceInitialized += (s, _) =>
                {
                    var hwnd = new WindowInteropHelper((Window)s!).Handle;
                    if (hwnd == IntPtr.Zero) return;
                    var ex = GetWindowLongPtr(hwnd, GwlExStyle);
                    SetWindowLongPtr(hwnd, GwlExStyle, ex | WsExToolWindow | WsExNoActivate);
                };
            }

            if (!_anchorShown)
            {
                _anchor.Show();
                _anchor.Hide();
                _anchorShown = true;
            }

            _anchor.Show();
            _anchor.Activate();
            var hwnd = new WindowInteropHelper(_anchor).Handle;
            if (hwnd != IntPtr.Zero)
                SetForegroundWindow(hwnd);

            // 问题3：PlacementTarget 挂锚点窗，让 ContextMenu 进应用资源树，
            // DynamicResource 令牌解析走应用主题字典（深色下不再退回系统浅色）。
            // 必须在 _anchor 非空之后赋值。
            _menu.PlacementTarget = _anchor;
            _menu.Placement = PlacementMode.MousePoint;
            _menu.StaysOpen = false;
            _menu.IsOpen = true;
        }
        catch (Exception ex)
        {
            // 5.4：兜底（额外第二层防线）。根治在 5.2（不复用关闭的窗口），
            // 这里只拦剩余异常，不让它沿 WinForms 回调栈杀进程。
            Prism.App.LogException("TrayService.ShowContextMenu", ex);
        }
    }

    private ContextMenu BuildMenu()
    {
        var menu = new ContextMenu();
        menu.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuStyle");
        // 5.1：菜单关闭即隐藏锚点窗，避免它在菜单关闭后仍 Visible 被窗口切换器看见。
        menu.Closed += (_, _) => _anchor?.Hide();

        var openSettings = new MenuItem { Header = "打开设置" };
        openSettings.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuItemStyle");
        openSettings.Click += (_, _) => Raise(OpenSettingsRequested);
        menu.Items.Add(openSettings);

        var rebuild = new MenuItem { Header = "重建索引" };
        rebuild.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuItemStyle");
        rebuild.Click += (_, _) => Raise(RebuildIndexRequested);
        menu.Items.Add(rebuild);

        var sep = new Separator();
        sep.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuSeparatorStyle");
        menu.Items.Add(sep);

        var exit = new MenuItem { Header = "退出" };
        exit.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuItemStyle");
        exit.Click += (_, _) => Raise(ExitRequested);
        menu.Items.Add(exit);

        return menu;
    }

    /// <summary>把托盘回调投递到 WPF UI 线程。</summary>
    private void Raise(Action? handler)
    {
        if (handler is null || _disposed) return;
        if (_dispatcher.CheckAccess())
            handler();
        else
            _dispatcher.BeginInvoke(handler);
    }

    /// <summary>
    /// 加载嵌入的 prism.ico；失败时克隆 SystemIcons.Application，
    /// 保证返回的 Icon 始终由本类 Dispose（不直接持有共享系统图标）。
    /// </summary>
    private static Icon LoadIcon()
    {
        try
        {
            var uri = new Uri("pack://application:,,,/Assets/prism.ico");
            var info = Application.GetResourceStream(uri);
            if (info?.Stream is { } stream)
            {
                using (stream)
                using (var temp = new Icon(stream))
                    return (Icon)temp.Clone();
            }
        }
        catch
        {
            // 资源缺失时降级，保证托盘仍可用。
        }

        return (Icon)SystemIcons.Application.Clone();
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    // 5.3：扩展样式常量。用 GetWindowLongW/SetWindowLongW（两架构都导出；
    // GWL_EXSTYLE 是 32 位值，64 位下经符号扩展仍正确）。
    private const int GwlExStyle = -20;
    private const int WsExToolWindow = 0x00000080;
    private const int WsExNoActivate = 0x08000000;

    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongW")]
    private static extern IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, IntPtr dwNewLong);

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _notify.MouseClick -= OnMouseClick;
        _notify.Visible = false;
        _notify.Icon = null;
        _notify.Dispose();
        _ownedIcon?.Dispose();
        if (_menu is not null)
        {
            _menu.IsOpen = false;
            _menu.Items.Clear();
        }
        if (_anchor is not null)
        {
            _anchor.Close();
            _anchor = null;
        }
        _menu = null;
    }
}
