using System.Drawing;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Threading;
using System.Windows.Forms;
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
            Raise(ShowSearchRequested);
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
            ShowContextMenuOnDispatcher();
        else
            _dispatcher.BeginInvoke(ShowContextMenuOnDispatcher);
    }

    private void ShowContextMenuOnDispatcher()
    {
        if (_disposed) return;

        _menu ??= BuildMenu();

        // 锚点窗：0 尺寸、无任务栏、离屏。首次需 Show 一次让 HWND 就绪，
        // 之后 SetForegroundWindow 让它获得激活态——这是托盘弹窗菜单
        // 点击外部正常收起的标准条件。
        _anchor ??= new Window
        {
            Width = 0,
            Height = 0,
            WindowStyle = WindowStyle.None,
            ShowInTaskbar = false,
            AllowsTransparency = true,
            Visibility = Visibility.Hidden,
            Left = -32000,
            Top = -32000,
        };

        if (!_anchorShown)
        {
            _anchor.Show();
            _anchor.Hide();
            _anchorShown = true;
        }

        _anchor.Show();
        _anchor.Activate();
        var hwnd = new System.Windows.Interop.WindowInteropHelper(_anchor).Handle;
        if (hwnd != IntPtr.Zero)
            SetForegroundWindow(hwnd);

        _menu.Placement = PlacementMode.MousePoint;
        _menu.StaysOpen = false;
        _menu.IsOpen = true;
    }

    private ContextMenu BuildMenu()
    {
        var menu = new ContextMenu();
        menu.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuStyle");

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
