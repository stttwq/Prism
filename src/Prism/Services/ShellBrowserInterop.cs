using System.Runtime.InteropServices;
using System.Text;

namespace Prism.Services;

/// <summary>
/// 判定 Windows 11 标签页 Explorer「哪个标签是活动标签」所需的最小公开 COM 面。
///
/// 标签页 Explorer 里每个标签都是独立的 <c>IShellBrowser</c>，但它们共用同一个顶层
/// <c>CabinetWClass</c> 窗口，所以 <c>IShellWindows</c> 会为一个 HWND 返回多条记录，
/// 只靠 HWND 无法区分。取视图窗口仍走官方接口：
/// <c>IServiceProvider(SID_STopLevelBrowser) → IShellBrowser → QueryActiveShellView →
/// IShellView::GetWindow</c>。
///
/// 判据是视图所属 <c>ShellTabWindowClass</c> 在兄弟中的 Z 序：**活动标签恒为最前（0）**。
/// 实机（Win11 22631）采样 14 次切换，两个标签的 Z 序每次都立刻互换，活动标签始终为 0。
///
/// 注意曾经用过、但实测无效的判据：
/// <list type="bullet">
/// <item><c>IsWindowVisible</c>：所有标签的视图窗口都恒为 true，无法区分。</item>
/// <item><c>DWMWA_CLOAKED</c>：所有标签恒为 0。</item>
/// <item>键盘焦点归属：多数时候与活动标签一致，但实测出现过焦点留在非活动标签的瞬间，
/// 不能作主判据。</item>
/// </list>
///
/// 全部走公开 COM 接口与只读窗口查询，无 DLL 注入、无窗口消息伪造（G4 安全边界）。
/// </summary>
internal static class ShellBrowserInterop
{
    private static readonly Guid TopLevelBrowserService =
        new("4C96BE40-915C-11CF-99D3-00AA004AE837");

    private static readonly Guid ShellBrowserInterface =
        new("000214E2-0000-0000-C000-000000000046");

    /// <summary>
    /// 一个 ShellWindows 项的活动标签状态。<see cref="Known"/> 为 false 表示接口不可用
    /// （旧系统、非标签页 Explorer 或调用失败），调用方必须按「无法区分」保守处理，
    /// 不能当成「不是活动标签」。
    /// </summary>
    internal readonly record struct TabVisibility(bool Known, bool IsVisible)
    {
        internal static TabVisibility Unknown { get; } = new(false, false);
    }

    /// <summary>
    /// 判断这个 ShellWindows 项是否为**活动标签**：取其视图窗口所属的
    /// <c>ShellTabWindowClass</c>，看它在兄弟中的 Z 序是否最前。
    ///
    /// 单标签窗口同样返回 <c>(true, true)</c>（唯一的 tab 必然在最前），
    /// 所以调用方无需为单标签特殊处理。
    /// </summary>
    internal static TabVisibility GetTabVisibility(object shellWindow)
    {
        if (!Marshal.IsComObject(shellWindow))
            return TabVisibility.Unknown;

        if (!TryGetActiveViewWindow(shellWindow, out var viewWindow))
            return TabVisibility.Unknown;

        var tab = FindTabWindow(viewWindow);
        if (tab == IntPtr.Zero)
            return TabVisibility.Unknown;

        return TryIsFrontmostTab(tab, out var frontmost)
            ? new TabVisibility(true, frontmost)
            : TabVisibility.Unknown;
    }

    /// <summary>从视图窗口向上找到所属的 <c>ShellTabWindowClass</c>；找不到返回 0。</summary>
    private static IntPtr FindTabWindow(IntPtr viewWindow)
    {
        // 实测父链：view → CtrlNotifySink → DirectUIHWND → DUIViewWndClassName
        //          → ShellTabWindowClass → CabinetWClass。留出余量防御布局变化。
        const int maxDepth = 8;
        var current = viewWindow;
        for (var depth = 0; current != IntPtr.Zero && depth < maxDepth; depth++)
        {
            if (IsClass(current, "ShellTabWindowClass"))
                return current;
            current = GetParent(current);
        }
        return IntPtr.Zero;
    }

    /// <summary>
    /// 这个 tab 窗口是否在同类兄弟里 Z 序最前。父窗口取不到或兄弟里找不到自己时返回
    /// false（由调用方降级为「无法区分」），绝不猜。
    /// </summary>
    private static bool TryIsFrontmostTab(IntPtr tab, out bool frontmost)
    {
        frontmost = false;
        var parent = GetParent(tab);
        if (parent == IntPtr.Zero)
            return false;

        // GW_CHILD 返回 Z 序最前的子窗口，GW_HWNDNEXT 依次向后。
        var sibling = GetWindow(parent, GwChild);
        var seen = false;
        while (sibling != IntPtr.Zero)
        {
            if (IsClass(sibling, "ShellTabWindowClass"))
            {
                // 第一个同类兄弟就是最前的那个。
                frontmost = sibling == tab;
                seen = true;
                break;
            }
            sibling = GetWindow(sibling, GwHwndNext);
        }
        return seen;
    }

    private static bool IsClass(IntPtr window, string expected)
    {
        var buffer = new StringBuilder(64);
        return GetClassName(window, buffer, buffer.Capacity) > 0
            && string.Equals(buffer.ToString(), expected, StringComparison.Ordinal);
    }

    /// <summary>
    /// 取该 ShellBrowser 当前活动视图的窗口句柄。任何一步失败都返回 false，
    /// 由调用方降级为「无法区分标签」。
    /// </summary>
    private static bool TryGetActiveViewWindow(object shellWindow, out IntPtr viewWindow)
    {
        viewWindow = IntPtr.Zero;
        if (shellWindow is not IShellServiceProvider provider)
            return false;

        var serviceId = TopLevelBrowserService;
        var interfaceId = ShellBrowserInterface;
        var browserPtr = IntPtr.Zero;
        try
        {
            if (provider.QueryService(ref serviceId, ref interfaceId, out browserPtr) != 0
                || browserPtr == IntPtr.Zero)
                return false;

            // GetObjectForIUnknown 自己 AddRef，所以下面 RCW 和裸指针都要各自释放一次。
            if (Marshal.GetObjectForIUnknown(browserPtr) is not IShellBrowser browser)
                return false;
            try
            {
                if (browser.QueryActiveShellView(out var view) != 0 || view is null)
                    return false;
                try
                {
                    return view.GetWindow(out viewWindow) == 0 && viewWindow != IntPtr.Zero;
                }
                finally
                {
                    if (Marshal.IsComObject(view))
                        Marshal.FinalReleaseComObject(view);
                }
            }
            finally
            {
                if (Marshal.IsComObject(browser))
                    Marshal.FinalReleaseComObject(browser);
            }
        }
        catch
        {
            // 标签正在关闭或 shell 拒绝调用：按无法区分处理。
            return false;
        }
        finally
        {
            if (browserPtr != IntPtr.Zero)
                Marshal.Release(browserPtr);
        }
    }

    private const uint GwHwndNext = 2;
    private const uint GwChild = 5;

    [DllImport("user32.dll")]
    private static extern IntPtr GetParent(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr hWnd, StringBuilder buffer, int max);

    /// <summary>
    /// <c>IServiceProvider</c>。名字避开 <see cref="System.IServiceProvider"/>；
    /// COM 只按 GUID 和 vtable 顺序绑定，托管名无关。
    /// </summary>
    [ComImport]
    [Guid("6D5140C1-7436-11CE-8034-00AA006009FA")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellServiceProvider
    {
        [PreserveSig]
        int QueryService(ref Guid guidService, ref Guid riid, out IntPtr ppvObject);
    }

    /// <summary>
    /// <c>IShellBrowser</c>。<see cref="QueryActiveShellView"/> 之前的槽位必须逐个声明
    /// 才能保证 vtable 偏移正确；这些占位方法永远不会被调用，所以用 IntPtr 简化签名。
    /// </summary>
    [ComImport]
    [Guid("000214E2-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellBrowser
    {
        // IOleWindow
        [PreserveSig] int GetWindow(out IntPtr phwnd);
        [PreserveSig] int ContextSensitiveHelp([MarshalAs(UnmanagedType.Bool)] bool fEnterMode);

        // IShellBrowser（占位，仅用于保持 vtable 顺序）
        [PreserveSig] int InsertMenusSB(IntPtr hmenuShared, IntPtr lpMenuWidths);
        [PreserveSig] int SetMenuSB(IntPtr hmenuShared, IntPtr holemenuRes, IntPtr hwndActiveObject);
        [PreserveSig] int RemoveMenusSB(IntPtr hmenuShared);
        [PreserveSig] int SetStatusTextSB(IntPtr pszStatusText);
        [PreserveSig] int EnableModelessSB([MarshalAs(UnmanagedType.Bool)] bool fEnable);
        [PreserveSig] int TranslateAcceleratorSB(IntPtr pmsg, ushort wID);
        [PreserveSig] int BrowseObject(IntPtr pidl, uint wFlags);
        [PreserveSig] int GetViewStateStream(uint grfMode, out IntPtr ppStrm);
        [PreserveSig] int GetControlWindow(uint id, out IntPtr phwnd);
        [PreserveSig] int SendControlMsg(uint id, uint uMsg, IntPtr wParam, IntPtr lParam, out IntPtr pret);

        [PreserveSig]
        int QueryActiveShellView([MarshalAs(UnmanagedType.Interface)] out IShellView ppshv);
    }

    /// <summary>
    /// <c>IShellView</c>。只需要 IOleWindow 的 <see cref="GetWindow"/>，
    /// 后续槽位不声明也不调用。
    /// </summary>
    [ComImport]
    [Guid("000214E3-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellView
    {
        [PreserveSig] int GetWindow(out IntPtr phwnd);
        [PreserveSig] int ContextSensitiveHelp([MarshalAs(UnmanagedType.Bool)] bool fEnterMode);
    }
}
