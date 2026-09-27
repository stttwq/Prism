using System.Runtime.InteropServices;

namespace Prism.Services;

/// <summary>
/// A1①（去分层窗口）：Win11 DWM 窗口效果——系统圆角 + 隐藏 DWM 自带边框。
/// SearchWindow 不再使用 AllowsTransparency 分层窗口（每帧 UpdateLayeredWindow
/// 整窗位图上传，是一切动画的逐帧成本上限），圆角与阴影交还 DWM 硬件合成；
/// 阴影由 WindowChrome 保留的 DWM 帧提供（见 SearchWindow.xaml），DWM 自带的
/// 1px 边框隐藏以保留卡片自绘的 Divider 边框。
/// 非 Win11 或被系统策略禁用时调用失败即静默退化（直角、无阴影），功能不受损。
/// </summary>
internal static class DwmWindowEffects
{
    private const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
    private const int DWMWA_BORDER_COLOR = 34;
    private const int DWMWCP_ROUND = 2;
    /// <summary>COLORREF 哨兵：DWM 不画边框。</summary>
    private static readonly int DwmwaColorNone = unchecked((int)0xFFFFFFFE);

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int value, int size);

    /// <summary>窗口句柄创建后应用一次；幂等，失败静默。</summary>
    public static void Apply(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero) return;
        var corner = DWMWCP_ROUND;
        _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref corner, sizeof(int));
        var border = DwmwaColorNone;
        _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, ref border, sizeof(int));
    }
}
