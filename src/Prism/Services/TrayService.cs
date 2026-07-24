using System.Drawing;
using System.Windows.Forms;
using System.Windows.Threading;
// NotifyIcon 在 WinForms 命名空间；WPF Application 由 GlobalUsings 统一别名。

namespace Prism.Services;

/// <summary>
/// 系统托盘图标与右键菜单（frontend-spec：打开设置 / 重建索引 / 退出）。
/// 左键单击托盘图标请求呼出搜索框。必须在 UI 线程创建与销毁；
/// 事件回调统一派发到 WPF Dispatcher，避免跨线程碰 UI。
/// </summary>
public sealed class TrayService : IDisposable
{
    private readonly NotifyIcon _notify;
    private readonly Dispatcher _dispatcher;
    private readonly Icon? _ownedIcon;
    private bool _disposed;

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

        var menu = new ContextMenuStrip();
        menu.Items.Add("打开设置", null, (_, _) => Raise(OpenSettingsRequested));
        menu.Items.Add("重建索引", null, (_, _) => Raise(RebuildIndexRequested));
        menu.Items.Add(new ToolStripSeparator());
        menu.Items.Add("退出", null, (_, _) => Raise(ExitRequested));

        _ownedIcon = LoadIcon();
        _notify = new NotifyIcon
        {
            Text = "Prism",
            Icon = _ownedIcon,
            ContextMenuStrip = menu,
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
            Raise(ShowSearchRequested);
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

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _notify.MouseClick -= OnMouseClick;
        _notify.Visible = false;
        _notify.Icon = null;
        _notify.Dispose();
        _ownedIcon?.Dispose();
    }
}
