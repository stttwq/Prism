using System.Globalization;
using System.Runtime.InteropServices;

// 复刻 ExplorerHostAdapter + ShellBrowserInterop 的真实调用，打印每条
// ShellWindows 记录的 HWND / 路径 / 活动标签判定结果，用于定位 E3。
Console.OutputEncoding = System.Text.Encoding.UTF8;

// Shell.Application 的 ShellWindows 只在 STA 下可用；控制台 Main 默认 MTA，
// 跨单元编组会失败（HWND=0、属性全空、QueryService 返回 E_NOINTERFACE）。
// Prism 是 WPF（STA），所以必须在 STA 线程上复刻才反映真实行为。
//
// 无参数 = 只采集一次；传 "scenario" = 自己搭多标签场景并在切换前后各采集一次。
// 场景编排也放在这里，避免再用 PowerShell（Opus 接管默认文管后
// Start-Process explorer.exe 会被拦截，必须用 shell: 参数）。
// watch = 只对现有窗口轮询采样，绝不改动用户的标签（scenario 会用 COM 导航，会破坏现场）。
var scenario = args.Length > 0 && args[0] == "scenario";
var watch = args.Length > 0 && args[0] == "watch";

var staThread = new Thread(() =>
{
    if (scenario) RunScenario();
    else if (watch) RunWatch();
    else Run();
});
staThread.SetApartmentState(ApartmentState.STA);
staThread.Start();
staThread.Join();

/// <summary>
/// 只读采样：找到真资源管理器窗口，连续轮询各标签的判据，签名变化时打印。
/// 不聚焦、不导航、不发按键——用户正在手动切标签，绝不能干扰现场。
/// </summary>
void RunWatch()
{
    var cabs = Native.FindCabinetWindows();
    if (cabs.Count == 0)
    {
        Console.WriteLine("FAILED: 没有找到真资源管理器（CabinetWClass）窗口。");
        Console.WriteLine("提示：Opus 已接管默认文管，请用 Win+E 打开真正的资源管理器。");
        return;
    }

    Console.WriteLine($"找到 {cabs.Count} 个 CabinetWClass 窗口：");
    foreach (var c in cabs)
        Console.WriteLine($"  0x{c.ToInt64():X}  tabs={Native.CountChildClass(c, "ShellTabWindowClass")}");
    Console.WriteLine();
    Console.WriteLine("=== 采样 40 秒，请来回切换标签（签名变化才打印）===");
    Console.WriteLine();

    var last = new Dictionary<long, string>();
    for (var tick = 0; tick < 25; tick++)
    {
        foreach (var cab in Native.FindCabinetWindows())
        {
            var sig = SampleTabs(cab);
            var key = cab.ToInt64();
            if (!last.TryGetValue(key, out var prev) || prev != sig)
            {
                Console.WriteLine($"[t={tick,2}s] 0x{key:X}");
                Console.WriteLine($"          {sig}");
                last[key] = sig;
            }
        }
        Thread.Sleep(1000);
    }
    Console.WriteLine();
    Console.WriteLine("=== 采样结束 ===");
}

void RunScenario()
{
    // 1) 确保有真资源管理器窗口（绕过 Opus 接管）
    if (Native.FindCabinetWindows().Count == 0)
    {
        Console.WriteLine("opening real Explorer via shell:MyComputerFolder ...");
        System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
        {
            FileName = "explorer.exe",
            Arguments = "shell:MyComputerFolder",
            UseShellExecute = true,
        });
        for (var i = 0; i < 30 && Native.FindCabinetWindows().Count == 0; i++)
            Thread.Sleep(400);
    }

    var cabs = Native.FindCabinetWindows();
    if (cabs.Count == 0)
    {
        Console.WriteLine("FAILED: no real Explorer (CabinetWClass) appeared");
        return;
    }
    var cab = cabs[0];
    Console.WriteLine($"explorer hwnd = 0x{cab.ToInt64():X}");
    Console.WriteLine();

    // 2) 确保有两个标签（键盘发 Ctrl+T；这一步没有 COM 替代）
    var tabCount = CountTabs(cab);
    if (tabCount < 2)
    {
        Native.FocusWindow(cab);
        Native.SendCtrlKey('T');
        Thread.Sleep(2000);
    }

    // 3) 用 COM Navigate 给两个标签设不同路径，避免依赖键盘输入地址栏。
    //    键盘自动化在实机上不可靠（焦点/输入法都可能吃掉按键）。
    NavigateTabsToDistinctPaths(cab, @"E:\LS", @"C:\Windows");
    Thread.Sleep(2000);

    // 4) 轮询采样：用户在这期间手动切标签，看判据是否跟着变。
    //    不用 Console.ReadLine，因为要能在非交互 shell 里跑。
    Console.WriteLine();
    Console.WriteLine("=== WATCH: 请在接下来 24 秒内手动来回切换标签 ===");
    Console.WriteLine();

    var lastSignature = "";
    for (var tick = 0; tick < 24; tick++)
    {
        var snapshot = SampleTabs(cab);
        if (snapshot != lastSignature)
        {
            Console.WriteLine($"[t={tick,2}s] {snapshot}");
            lastSignature = snapshot;
        }
        Thread.Sleep(1000);
    }
    Console.WriteLine();
    Console.WriteLine("=== WATCH done ===");
}

/// <summary>
/// 采一次样，返回单行签名：每个标签的 path + 各判据。
/// 只有签名变化时才打印，方便看出「切换标签时哪个判据真的跟着变」。
/// </summary>
static string SampleTabs(IntPtr cab)
{
    var type = Type.GetTypeFromProgID("Shell.Application");
    if (type is null) return "(no shell)";
    var shell = Activator.CreateInstance(type);
    if (shell is null) return "(no shell)";

    var windows = shell.GetType().InvokeMember(
        "Windows", System.Reflection.BindingFlags.InvokeMethod, null, shell, null);
    if (windows is null) return "(no windows)";

    var count = Convert.ToInt32(
        windows.GetType().InvokeMember(
            "Count", System.Reflection.BindingFlags.GetProperty, null, windows, null),
        CultureInfo.InvariantCulture);

    var parts = new List<string>();
    for (var i = 0; i < count; i++)
    {
        object? item = null;
        try
        {
            item = windows.GetType().InvokeMember(
                "Item", System.Reflection.BindingFlags.InvokeMethod, null, windows, [i]);
            if (item is null) continue;
            if (ReadHwnd(item) != cab) continue;

            var path = ReadFolderPath(item) ?? "?";
            var leaf = path.Length > 22 ? "…" + path[^22..] : path;
            parts.Add($"{leaf} => {TabSignature(item)}");
        }
        catch
        {
            // Opus 的条目读属性会抛，跳过。
        }
        finally
        {
            if (item is not null && Marshal.IsComObject(item))
                Marshal.FinalReleaseComObject(item);
        }
    }
    return string.Join(" | ", parts);
}

/// <summary>数一个顶层窗口下有几个 ShellTabWindowClass 子窗口。</summary>
static int CountTabs(IntPtr cab) => Native.CountChildClass(cab, "ShellTabWindowClass");

/// <summary>
/// 用每条 ShellWindows 记录自己的 Navigate 把两个标签导到不同目录。
/// 只处理属于目标 HWND、且能读到 HWND 的条目（Opus 的 Lister 读 HWND 会抛异常）。
/// </summary>
static void NavigateTabsToDistinctPaths(IntPtr cab, string first, string second)
{
    var type = Type.GetTypeFromProgID("Shell.Application");
    if (type is null) return;
    var shell = Activator.CreateInstance(type);
    if (shell is null) return;

    var windows = shell.GetType().InvokeMember(
        "Windows", System.Reflection.BindingFlags.InvokeMethod, null, shell, null);
    if (windows is null) return;

    var count = Convert.ToInt32(
        windows.GetType().InvokeMember(
            "Count", System.Reflection.BindingFlags.GetProperty, null, windows, null),
        CultureInfo.InvariantCulture);

    var assigned = 0;
    for (var i = 0; i < count && assigned < 2; i++)
    {
        object? item = null;
        try
        {
            item = windows.GetType().InvokeMember(
                "Item", System.Reflection.BindingFlags.InvokeMethod, null, windows, [i]);
            if (item is null) continue;
            if (ReadHwnd(item) != cab) continue;

            var path = assigned == 0 ? first : second;
            item.GetType().InvokeMember(
                "Navigate", System.Reflection.BindingFlags.InvokeMethod, null, item, [path]);
            Console.WriteLine($"  navigated tab #{assigned} -> {path}");
            assigned++;
            Thread.Sleep(1200);
        }
        catch (Exception ex)
        {
            Console.WriteLine($"  navigate skip [{i}]: {ex.GetType().Name}");
        }
        finally
        {
            if (item is not null && Marshal.IsComObject(item))
                Marshal.FinalReleaseComObject(item);
        }
    }
}

void Run()
{
var foreground = Native.GetForegroundWindow();
Console.WriteLine($"foreground hwnd = 0x{foreground.ToInt64():X}");
Console.WriteLine();

var type = Type.GetTypeFromProgID("Shell.Application");
if (type is null)
{
    Console.WriteLine("Shell.Application ProgID not found");
    return;
}

var shell = Activator.CreateInstance(type);
if (shell is null)
{
    Console.WriteLine("cannot create Shell.Application");
    return;
}

var windows = shell.GetType().InvokeMember(
    "Windows", System.Reflection.BindingFlags.InvokeMethod, null, shell, null);
if (windows is null)
{
    Console.WriteLine("Windows() returned null");
    return;
}

var count = Convert.ToInt32(
    windows.GetType().InvokeMember(
        "Count", System.Reflection.BindingFlags.GetProperty, null, windows, null),
    CultureInfo.InvariantCulture);

Console.WriteLine($"ShellWindows.Count = {count}");
Console.WriteLine();

for (var i = 0; i < count; i++)
{
    object? item = null;
    try
    {
        item = windows.GetType().InvokeMember(
            "Item", System.Reflection.BindingFlags.InvokeMethod, null, windows, [i]);
        if (item is null)
        {
            Console.WriteLine($"[{i}] <null item>");
            continue;
        }

        var hwnd = ReadHwnd(item);
        var cls = hwnd == IntPtr.Zero ? "-" : Native.ClassNameOf(hwnd);
        var path = ReadFolderPath(item);
        var probe = Inspect(item);
        var isFg = hwnd == foreground;

        Console.WriteLine($"[{i}] hwnd=0x{hwnd.ToInt64():X} class={cls}{(isFg ? "  <== FOREGROUND" : "")}");
        Console.WriteLine($"     path  : {path ?? "(null)"}");
        Console.WriteLine($"     probe : {probe}");
    }
    catch (Exception ex)
    {
        Console.WriteLine($"[{i}] EX {ex.GetType().Name}: {ex.Message}");
    }
    finally
    {
        if (item is not null && Marshal.IsComObject(item))
            Marshal.FinalReleaseComObject(item);
    }
}
} // Run

static IntPtr ReadHwnd(object item)
{
    try
    {
        var value = item.GetType().InvokeMember(
            "HWND", System.Reflection.BindingFlags.GetProperty, null, item, null);
        if (value is null) return IntPtr.Zero;
        return new IntPtr(Convert.ToInt64(value, CultureInfo.InvariantCulture));
    }
    catch
    {
        return IntPtr.Zero;
    }
}

static string? ReadFolderPath(object item)
{
    try
    {
        var document = item.GetType().InvokeMember(
            "Document", System.Reflection.BindingFlags.GetProperty, null, item, null);
        if (document is null) return null;
        var folder = document.GetType().InvokeMember(
            "Folder", System.Reflection.BindingFlags.GetProperty, null, document, null);
        if (folder is null) return null;
        var self = folder.GetType().InvokeMember(
            "Self", System.Reflection.BindingFlags.GetProperty, null, folder, null);
        if (self is null) return null;
        return Convert.ToString(
            self.GetType().InvokeMember(
                "Path", System.Reflection.BindingFlags.GetProperty, null, self, null),
            CultureInfo.InvariantCulture);
    }
    catch (Exception ex)
    {
        return $"(err {ex.GetType().Name})";
    }
}

/// <summary>
/// 一个标签的判据摘要（紧凑单行）：view 可见性、cloaked、tab 窗口 Z 序、焦点归属。
/// 用于在切换标签时观察哪个字段真的会变。
/// </summary>
static string TabSignature(object shellWindow)
{
    if (!TryGetViewWindow(shellWindow, out var vh))
        return "view=FAIL";
    return Native.SignatureFor(vh);
}

/// <summary>取该条 ShellWindows 记录的活动视图窗口句柄。</summary>
static bool TryGetViewWindow(object shellWindow, out IntPtr viewWindow)
{
    viewWindow = IntPtr.Zero;
    if (!Marshal.IsComObject(shellWindow)) return false;
    if (shellWindow is not Native.IShellServiceProvider provider) return false;

    var sid = Native.SidSTopLevelBrowser;
    var iid = Native.IidIShellBrowser;
    var bp = IntPtr.Zero;
    try
    {
        if (provider.QueryService(ref sid, ref iid, out bp) != 0 || bp == IntPtr.Zero)
            return false;
        if (Marshal.GetObjectForIUnknown(bp) is not Native.IShellBrowser browser)
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
                if (Marshal.IsComObject(view)) Marshal.FinalReleaseComObject(view);
            }
        }
        finally
        {
            if (Marshal.IsComObject(browser)) Marshal.FinalReleaseComObject(browser);
        }
    }
    catch
    {
        return false;
    }
    finally
    {
        if (bp != IntPtr.Zero) Marshal.Release(bp);
    }
}

static string Inspect(object shellWindow)
{
    if (!Marshal.IsComObject(shellWindow)) return "not a COM object";
    if (shellWindow is not Native.IShellServiceProvider provider)
        return "cast to IServiceProvider FAILED";

    var sid = Native.SidSTopLevelBrowser;
    var iid = Native.IidIShellBrowser;
    var bp = IntPtr.Zero;
    try
    {
        var hr = provider.QueryService(ref sid, ref iid, out bp);
        if (hr != 0) return $"QueryService hr=0x{hr:X8}";
        if (bp == IntPtr.Zero) return "QueryService gave null";

        if (Marshal.GetObjectForIUnknown(bp) is not Native.IShellBrowser browser)
            return "cast IShellBrowser FAILED";
        try
        {
            var hr2 = browser.QueryActiveShellView(out var view);
            if (hr2 != 0 || view is null) return $"QueryActiveShellView hr=0x{hr2:X8}";
            try
            {
                var hr3 = view.GetWindow(out var vh);
                if (hr3 != 0) return $"IShellView.GetWindow hr=0x{hr3:X8}";
                return Native.DescribeViewWindow(vh);
            }
            finally
            {
                if (Marshal.IsComObject(view)) Marshal.FinalReleaseComObject(view);
            }
        }
        finally
        {
            if (Marshal.IsComObject(browser)) Marshal.FinalReleaseComObject(browser);
        }
    }
    catch (Exception ex)
    {
        return $"EX {ex.GetType().Name}: {ex.Message}";
    }
    finally
    {
        if (bp != IntPtr.Zero) Marshal.Release(bp);
    }
}

internal static class Native
{
    internal static readonly Guid SidSTopLevelBrowser =
        new("4C96BE40-915C-11CF-99D3-00AA004AE837");

    internal static readonly Guid IidIShellBrowser =
        new("000214E2-0000-0000-C000-000000000046");

    [DllImport("user32.dll")]
    internal static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr GetParent(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr hWnd, uint flags);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);

    [DllImport("user32.dll")]
    private static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);

    [DllImport("user32.dll")]
    private static extern bool IsWindowEnabled(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern uint GetWindowLong(IntPtr hWnd, int index);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(
        IntPtr hWnd, uint attr, out int value, int size);

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    /// <summary>
    /// 采集 view 窗口的全部可判据：父链、矩形、同级 Z 序、cloaked、样式。
    /// 目的是找出「哪个标签真正呈现」的可靠判据（IsWindowVisible 已证明无效）。
    /// </summary>
    internal static string DescribeViewWindow(IntPtr vh)
    {
        if (vh == IntPtr.Zero) return "view=0 (null)";

        var visible = IsWindowVisible(vh);
        var enabled = IsWindowEnabled(vh);
        var parent = GetParent(vh);
        var root = GetAncestor(vh, 2 /* GA_ROOT */);

        var rectText = GetWindowRect(vh, out var r)
            ? $"{r.Right - r.Left}x{r.Bottom - r.Top}@({r.Left},{r.Top})"
            : "(rect failed)";

        // cloaked 是 DWM 层的「逻辑隐藏」，虚拟桌面/后台标签常用它而非 WS_VISIBLE。
        var cloakedText = DwmGetWindowAttribute(vh, 14 /* DWMWA_CLOAKED */, out var cloaked, 4) == 0
            ? cloaked.ToString(CultureInfo.InvariantCulture)
            : "n/a";

        // 每个标签有独立的 ShellTabWindowClass，都挂在同一个 CabinetWClass 下。
        // 活动标签的 tab 窗口应当在兄弟 Z 序里最靠前（idx 最小）。
        var tab = IntPtr.Zero;
        for (var p = vh; p != IntPtr.Zero; p = GetParent(p))
        {
            if (ClassNameOf(p) == "ShellTabWindowClass") { tab = p; break; }
        }

        var tabZ = -1;
        var tabSiblings = 0;
        if (tab != IntPtr.Zero)
        {
            var tabParent = GetParent(tab);
            if (tabParent != IntPtr.Zero)
            {
                var sibling = GetWindow(tabParent, 5 /* GW_CHILD */);
                var idx = 0;
                while (sibling != IntPtr.Zero)
                {
                    if (ClassNameOf(sibling) == "ShellTabWindowClass")
                    {
                        if (sibling == tab) tabZ = tabSiblings;
                        tabSiblings++;
                    }
                    idx++;
                    sibling = GetWindow(sibling, 2 /* GW_HWNDNEXT */);
                }
            }
        }

        var style = GetWindowLong(vh, -16 /* GWL_STYLE */);

        return $"view=0x{vh.ToInt64():X} visible={visible} enabled={enabled} "
            + $"cloaked={cloakedText} rect={rectText} style=0x{style:X}"
            + Environment.NewLine
            + $"                tab=0x{tab.ToInt64():X} tabZ={tabZ}/{tabSiblings}"
            + Environment.NewLine + "                chain: " + DescribeChain(vh);
    }

    /// <summary>
    /// 从 view 窗口向上走到顶层，逐级打印 class/visible/cloaked/rect。
    /// 标签化 Explorer 每个标签有独立的中间父窗口，判据很可能在这一层。
    /// </summary>
    private static string DescribeChain(IntPtr start)
    {
        var parts = new List<string>();
        var current = GetParent(start);
        var guard = 0;
        while (current != IntPtr.Zero && guard++ < 8)
        {
            var vis = IsWindowVisible(current);
            var ck = DwmGetWindowAttribute(current, 14, out var c, 4) == 0
                ? c.ToString(CultureInfo.InvariantCulture)
                : "n/a";
            var rect = GetWindowRect(current, out var r)
                ? $"{r.Right - r.Left}x{r.Bottom - r.Top}"
                : "?";
            parts.Add($"0x{current.ToInt64():X}[{ClassNameOf(current)}] vis={vis} cloaked={ck} {rect}");
            current = GetParent(current);
        }
        return string.Join(Environment.NewLine + "                     -> ", parts);
    }

    [DllImport("user32.dll")]
    internal static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int cmd);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc cb, IntPtr param);

    [DllImport("user32.dll")]
    private static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);

    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr param);

    private const uint KeyUp = 0x0002;
    private const byte VkControl = 0x11;
    private const byte VkMenu = 0x12;   // Alt
    private const byte VkReturn = 0x0D;

    /// <summary>枚举所有 CabinetWClass 顶层窗口（真资源管理器，非 Opus Lister）。</summary>
    internal static List<IntPtr> FindCabinetWindows()
    {
        var found = new List<IntPtr>();
        EnumWindows((h, _) =>
        {
            if (ClassNameOf(h) == "CabinetWClass" && IsWindowVisible(h))
                found.Add(h);
            return true;
        }, IntPtr.Zero);
        return found;
    }

    /// <summary>
    /// 判据摘要：vis / cloaked / tabZ / 焦点是否落在这个 view 的子树里。
    /// 焦点归属用 GetGUIThreadInfo(explorer 线程) 拿 hwndFocus 再往上比对。
    /// </summary>
    internal static string SignatureFor(IntPtr vh)
    {
        var vis = IsWindowVisible(vh) ? "1" : "0";

        var cloaked = DwmGetWindowAttribute(vh, 14, out var ck, 4) == 0
            ? ck.ToString(CultureInfo.InvariantCulture)
            : "?";

        // tab 窗口在兄弟中的 Z 序
        var tab = IntPtr.Zero;
        for (var p = vh; p != IntPtr.Zero; p = GetParent(p))
        {
            if (ClassNameOf(p) == "ShellTabWindowClass") { tab = p; break; }
        }
        var tabZ = -1;
        if (tab != IntPtr.Zero)
        {
            var tp = GetParent(tab);
            if (tp != IntPtr.Zero)
            {
                var sib = GetWindow(tp, 5);
                var idx = 0;
                while (sib != IntPtr.Zero)
                {
                    if (ClassNameOf(sib) == "ShellTabWindowClass")
                    {
                        if (sib == tab) { tabZ = idx; break; }
                        idx++;
                    }
                    sib = GetWindow(sib, 2);
                }
            }
        }

        // 焦点是否在这个 view 的子树内
        var focused = "0";
        var gti = new GUITHREADINFO { cbSize = Marshal.SizeOf<GUITHREADINFO>() };
        var tid = GetWindowThreadProcessId(vh, out _);
        if (tid != 0 && GetGUIThreadInfo(tid, ref gti) && gti.hwndFocus != IntPtr.Zero)
        {
            for (var f = gti.hwndFocus; f != IntPtr.Zero; f = GetParent(f))
            {
                if (f == vh) { focused = "1"; break; }
            }
        }

        // 新判据（ShellBrowserInterop 现在的实现）：同类兄弟里 Z 序最前 = 活动标签。
        var verdict = "?";
        if (tab != IntPtr.Zero)
        {
            var tp = GetParent(tab);
            if (tp != IntPtr.Zero)
            {
                var sib = GetWindow(tp, 5);
                while (sib != IntPtr.Zero)
                {
                    if (ClassNameOf(sib) == "ShellTabWindowClass")
                    {
                        verdict = sib == tab ? "ACTIVE" : "bg";
                        break;
                    }
                    sib = GetWindow(sib, 2);
                }
            }
        }

        return $"vis={vis} cloak={cloaked} tabZ={tabZ} focus={focused} => {verdict}";
    }

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);

    [DllImport("user32.dll")]
    private static extern bool GetGUIThreadInfo(uint idThread, ref GUITHREADINFO info);

    [StructLayout(LayoutKind.Sequential)]
    private struct GUITHREADINFO
    {
        public int cbSize;
        public int flags;
        public IntPtr hwndActive;
        public IntPtr hwndFocus;
        public IntPtr hwndCapture;
        public IntPtr hwndMenuOwner;
        public IntPtr hwndMoveSize;
        public IntPtr hwndCaret;
        public RECT rcCaret;
    }

    /// <summary>数某个父窗口下指定类名的直接子窗口个数。</summary>
    internal static int CountChildClass(IntPtr parent, string className)
    {
        var n = 0;
        var child = GetWindow(parent, 5 /* GW_CHILD */);
        while (child != IntPtr.Zero)
        {
            if (ClassNameOf(child) == className) n++;
            child = GetWindow(child, 2 /* GW_HWNDNEXT */);
        }
        return n;
    }

    internal static void FocusWindow(IntPtr hWnd)
    {
        ShowWindow(hWnd, 9 /* SW_RESTORE */);
        SetForegroundWindow(hWnd);
        Thread.Sleep(600);
    }

    private static void Tap(byte vk)
    {
        keybd_event(vk, 0, 0, IntPtr.Zero);
        Thread.Sleep(40);
        keybd_event(vk, 0, KeyUp, IntPtr.Zero);
        Thread.Sleep(40);
    }

    internal static void SendCtrlKey(char key)
    {
        keybd_event(VkControl, 0, 0, IntPtr.Zero);
        Thread.Sleep(40);
        Tap((byte)char.ToUpperInvariant(key));
        keybd_event(VkControl, 0, KeyUp, IntPtr.Zero);
        Thread.Sleep(60);
    }

    /// <summary>Alt+D 聚焦地址栏。</summary>
    internal static void SendAltD()
    {
        keybd_event(VkMenu, 0, 0, IntPtr.Zero);
        Thread.Sleep(40);
        Tap((byte)'D');
        keybd_event(VkMenu, 0, KeyUp, IntPtr.Zero);
        Thread.Sleep(60);
    }

    internal static void SendEnter() => Tap(VkReturn);

    /// <summary>逐字符输入路径。只需支持盘符路径里的字符。</summary>
    internal static void TypeText(string text)
    {
        foreach (var ch in text)
        {
            if (ch == ':')
            {
                // ':' 需要 Shift + ';'
                keybd_event(0x10 /* VK_SHIFT */, 0, 0, IntPtr.Zero);
                Thread.Sleep(30);
                Tap(0xBA /* VK_OEM_1 = ';' */);
                keybd_event(0x10, 0, KeyUp, IntPtr.Zero);
                Thread.Sleep(30);
            }
            else if (ch == '\\')
            {
                Tap(0xDC /* VK_OEM_5 */);
            }
            else if (char.IsLetterOrDigit(ch))
            {
                Tap((byte)char.ToUpperInvariant(ch));
            }
        }
        Thread.Sleep(200);
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr hWnd, System.Text.StringBuilder buf, int max);

    internal static string ClassNameOf(IntPtr hwnd)
    {
        var sb = new System.Text.StringBuilder(256);
        return GetClassName(hwnd, sb, sb.Capacity) > 0 ? sb.ToString() : "?";
    }

    [ComImport]
    [Guid("6D5140C1-7436-11CE-8034-00AA006009FA")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IShellServiceProvider
    {
        [PreserveSig]
        int QueryService(ref Guid guidService, ref Guid riid, out IntPtr ppvObject);
    }

    [ComImport]
    [Guid("000214E2-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IShellBrowser
    {
        [PreserveSig] int GetWindow(out IntPtr phwnd);
        [PreserveSig] int ContextSensitiveHelp([MarshalAs(UnmanagedType.Bool)] bool fEnterMode);
        [PreserveSig] int InsertMenusSB(IntPtr a, IntPtr b);
        [PreserveSig] int SetMenuSB(IntPtr a, IntPtr b, IntPtr c);
        [PreserveSig] int RemoveMenusSB(IntPtr a);
        [PreserveSig] int SetStatusTextSB(IntPtr a);
        [PreserveSig] int EnableModelessSB([MarshalAs(UnmanagedType.Bool)] bool a);
        [PreserveSig] int TranslateAcceleratorSB(IntPtr a, ushort b);
        [PreserveSig] int BrowseObject(IntPtr a, uint b);
        [PreserveSig] int GetViewStateStream(uint a, out IntPtr b);
        [PreserveSig] int GetControlWindow(uint a, out IntPtr b);
        [PreserveSig] int SendControlMsg(uint a, uint b, IntPtr c, IntPtr d, out IntPtr e);
        [PreserveSig] int QueryActiveShellView([MarshalAs(UnmanagedType.Interface)] out IShellView v);
    }

    [ComImport]
    [Guid("000214E3-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IShellView
    {
        [PreserveSig] int GetWindow(out IntPtr phwnd);
        [PreserveSig] int ContextSensitiveHelp([MarshalAs(UnmanagedType.Bool)] bool fEnterMode);
    }
}
