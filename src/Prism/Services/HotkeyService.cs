using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 全局快捷键服务。支持两种模式（frontend-spec.md 流程A）：
/// <list type="bullet">
/// <item><see cref="HotkeyMode.DoubleCtrl"/>：WH_KEYBOARD_LL 低级钩子，400ms 内双击 Ctrl。</item>
/// <item><see cref="HotkeyMode.Combo"/>：message-only 窗口 + RegisterHotKey（如 Alt+Space）。</item>
/// </list>
/// 调用 <see cref="Apply"/> 切换模式；<see cref="Triggered"/> 在 UI 线程触发。
/// </summary>
public sealed class HotkeyService : IDisposable
{
    // ── Win32 ──────────────────────────────────────────────────────────────
    private const int WH_KEYBOARD_LL = 13;
    private const int WM_KEYDOWN = 0x0100;
    private const int WM_SYSKEYDOWN = 0x0104;
    private const int WM_HOTKEY = 0x0312;
    // 低级钩子里左右 Ctrl 分别是 0xA2/0xA3，很少报通用 0x11；三者都认。
    private const int VK_CONTROL = 0x11;
    private const int VK_LCONTROL = 0xA2;
    private const int VK_RCONTROL = 0xA3;
    private const int HOTKEY_ID = 0xBEEF;

    [DllImport("user32.dll")] private static extern IntPtr SetWindowsHookEx(int id, LowLevelKeyboardProc cb, IntPtr hMod, uint tid);
    [DllImport("user32.dll")] private static extern bool UnhookWindowsHookEx(IntPtr hook);
    [DllImport("user32.dll")] private static extern IntPtr CallNextHookEx(IntPtr hook, int code, IntPtr w, IntPtr l);
    [DllImport("kernel32.dll")] private static extern IntPtr GetModuleHandle(string? mod);
    [DllImport("user32.dll")] private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint mod, uint vk);
    [DllImport("user32.dll")] private static extern bool UnregisterHotKey(IntPtr hwnd, int id);

    private delegate IntPtr LowLevelKeyboardProc(int code, IntPtr w, IntPtr l);

    [StructLayout(LayoutKind.Sequential)]
    private struct KBDLLHOOKSTRUCT { public uint vkCode, scanCode, flags, time; public IntPtr extra; }

    // ── State ──────────────────────────────────────────────────────────────
    private LowLevelKeyboardProc? _hookProc; // keep-alive: GC must not collect the delegate
    internal IntPtr _hook;
    private HwndSource? _msgWindow;
    private HotkeyMode _mode;

    // Double-Ctrl detection state
    private long _lastCtrlUpMs = -1;
    internal volatile int _callbackCount;
    private bool _ctrlDown;
    private bool _otherKeyPressed;
    private const int DoubleClickMs = 400;
    private const int MaxHoldMs = 300;
    private long _ctrlDownMs;

    /// <summary>
    /// WH_KEYBOARD_LL 的回调由系统投递到「安装钩子的那个线程」的消息队列，
    /// 该线程必须有消息循环，否则每次按键都要等 LowLevelHooksTimeout（默认 ~300ms）
    /// 超时，表现为全局输入卡顿 + 双击 Ctrl 完全失效。
    /// 因此钩子跑在专用泵线程上：既不能装在线程池线程（无消息循环），
    /// 也不装在 UI 线程（与 WPF 布局/渲染/搜索抢同一个泵，忙时回调超时会被系统摘除）。
    /// </summary>
    private System.Threading.Thread? _hookThread;
    private System.Windows.Threading.Dispatcher? _hookDispatcher;

    /// <summary>
    /// AUDIT-2026-08-18 C-D3: 低级键盘钩子被系统超时摘除后无自检重装，
    /// 60s 定时器兜底重装（幂等：UnhookEx + SetWindowsHookEx）。
    /// </summary>
    private System.Threading.Timer? _hookRefreshTimer;
    private const int HookRefreshIntervalMs = 60_000;
    private volatile bool _disposed;

    /// <summary>呼出事件，在 UI 线程触发。</summary>
    public event Action? Triggered;

    // ── Public API ─────────────────────────────────────────────────────────

    /// <summary>按 <paramref name="settings"/> 安装快捷键；可重复调用以切换模式。</summary>
    public void Apply(Settings settings)
    {
        Uninstall();
        _mode = settings.HotkeyMode;
        if (_mode == HotkeyMode.DoubleCtrl)
        {
            InstallLowLevelHook();
            StartHookRefresh();
        }
        else
            InstallRegisterHotKey(settings.ComboHotkey);
    }

    public void Dispose()
    {
        _disposed = true;
        Uninstall();
        _hookDispatcher?.InvokeShutdown();
        _hookDispatcher = null;
        _hookThread = null;
    }

    /// <summary>
    /// 幂等重装低级键盘钩子，仅 DoubleCtrl 模式有效。呼出/隐藏时调用，
    /// 系统摘钩后无需等 60s 定时器。实际安装被投递到钩子线程。
    /// </summary>
    public void RefreshHook()
    {
        if (_mode == HotkeyMode.DoubleCtrl) InstallLowLevelHook();
    }

    private void StartHookRefresh()
    {
        _hookRefreshTimer?.Dispose();
        // 兜底重装必须回到钩子线程执行（线程池线程装的钩子收不到回调）。
        _hookRefreshTimer = new System.Threading.Timer(
            _ => InstallLowLevelHook(), null, HookRefreshIntervalMs, HookRefreshIntervalMs);
    }

    // ── Low-level hook (DoubleCtrl) ────────────────────────────────────────

    /// <summary>
    /// AUDIT-2026-08-18 C-D3: 幂等（重）装低级键盘钩子。系统在回调超时
    /// （LowLevelHooksTimeout，默认 ~300ms）后会静默 unhook，用户无感知，
    /// 所以 60s 周期重装兜底。始终在专用钩子线程上执行。
    /// </summary>
    internal void InstallLowLevelHook()
    {
        // Dispose 与仍在飞的 timer 回调有竞争：不拦住的话回调会重建一个没人再摘的钩子。
        if (_disposed) return;
        EnsureHookThread();
        _hookDispatcher?.InvokeAsync(() =>
        {
            if (_hook != IntPtr.Zero)
            {
                UnhookWindowsHookEx(_hook);
                _hook = IntPtr.Zero;
            }
            _hookProc = HookCallback;
            // 低级键盘钩子的 hMod 传当前 exe 的模块句柄即可（GetModuleHandle(null)），
            // 省去 Process/MainModule 的创建与释放，避免每次 Apply 泄漏 Process 对象。
            _hook = SetWindowsHookEx(WH_KEYBOARD_LL, _hookProc, GetModuleHandle(null), 0);
            if (_hook == IntPtr.Zero)
            {
                // 双击 Ctrl 本就是兜底模式，无可再降级——但绝不能静默死亡：
                // 至少留下可诊断的痕迹（DebugView / 调试器输出）。
                System.Diagnostics.Debug.WriteLine(
                    "[Prism] 低级键盘钩子安装失败，双击 Ctrl 呼出将不可用（Apply 重新应用可恢复）");
            }
        });
    }

    /// <summary>钩子专用泵线程：只跑 Dispatcher 循环，回调不与 WPF UI 抢同一个消息泵。</summary>
    private void EnsureHookThread()
    {
        if (_hookDispatcher is not null) return;
        using var ready = new System.Threading.ManualResetEventSlim(false);
        _hookThread = new System.Threading.Thread(() =>
        {
            _hookDispatcher = System.Windows.Threading.Dispatcher.CurrentDispatcher;
            ready.Set();
            System.Windows.Threading.Dispatcher.Run();
        })
        {
            IsBackground = true,
            Name = "PrismHotkeyHook",
        };
        _hookThread.SetApartmentState(System.Threading.ApartmentState.STA);
        _hookThread.Start();
        ready.Wait();
    }

    private IntPtr HookCallback(int code, IntPtr w, IntPtr l)
    {
        if (code >= 0)
        {
            // 回调计数：钩子若装在没有消息泵的线程上，句柄非零但这里一次都不会执行。
            // HotkeyHookThreadTests 靠它证伪「装错线程」，别的地方不读。
            _callbackCount++;
            var kb = Marshal.PtrToStructure<KBDLLHOOKSTRUCT>(l);
            // 低级钩子里左右 Ctrl 是 0xA2/0xA3，通用 0x11 很少出现。
            bool isCtrl = kb.vkCode is VK_CONTROL or VK_LCONTROL or VK_RCONTROL;
            bool isDown = w == WM_KEYDOWN || w == WM_SYSKEYDOWN;

            if (isDown)
            {
                if (isCtrl)
                {
                    if (!_ctrlDown)
                    {
                        _ctrlDown = true;
                        _otherKeyPressed = false;
                        _ctrlDownMs = Environment.TickCount64;
                    }
                }
                else
                {
                    // 其他键打断双击序列。
                    _otherKeyPressed = true;
                    _lastCtrlUpMs = -1;
                }
            }
            else // key-up
            {
                if (isCtrl && _ctrlDown)
                {
                    _ctrlDown = false;
                    long holdMs = Environment.TickCount64 - _ctrlDownMs;

                    if (!_otherKeyPressed && holdMs <= MaxHoldMs)
                    {
                        long now = Environment.TickCount64;
                        if (_lastCtrlUpMs >= 0 && now - _lastCtrlUpMs <= DoubleClickMs)
                        {
                            _lastCtrlUpMs = -1;
                            Application.Current?.Dispatcher.InvokeAsync(() => Triggered?.Invoke());
                        }
                        else
                        {
                            _lastCtrlUpMs = now;
                        }
                    }
                    else
                    {
                        _lastCtrlUpMs = -1;
                    }
                }
            }
        }
        return CallNextHookEx(_hook, code, w, l);
    }

    // ── RegisterHotKey (Combo) ─────────────────────────────────────────────

    private void InstallRegisterHotKey(string combo)
    {
        var (mod, vk) = ParseCombo(combo);
        if (vk == 0)
        {
            // 组合键无法解析（拼写错误/缺主键）——降级到双击 Ctrl，保证仍能呼出。
            _mode = HotkeyMode.DoubleCtrl;
            InstallLowLevelHook();
            return;
        }

        // message-only 窗口：不显示、不在任务栏，仅接收 WM_HOTKEY。
        _msgWindow = new HwndSource(new HwndSourceParameters("PrismHotkeyMsg")
        {
            ParentWindow = new IntPtr(-3), // HWND_MESSAGE
            Width = 0, Height = 0,
        });
        _msgWindow.AddHook(MsgWndProc);

        // 组合键可能被其它程序占用导致注册失败——降级到双击 Ctrl（implement.md：各模块可单独降级）。
        if (!RegisterHotKey(_msgWindow.Handle, HOTKEY_ID, mod, vk))
        {
            _msgWindow.RemoveHook(MsgWndProc);
            _msgWindow.Dispose();
            _msgWindow = null;
            _mode = HotkeyMode.DoubleCtrl;
            InstallLowLevelHook();
        }
    }

    private IntPtr MsgWndProc(IntPtr hwnd, int msg, IntPtr w, IntPtr l, ref bool handled)
    {
        if (msg == WM_HOTKEY && w.ToInt32() == HOTKEY_ID)
        {
            Triggered?.Invoke();
            handled = true;
        }
        return IntPtr.Zero;
    }

    // ── Cleanup ────────────────────────────────────────────────────────────

    private void Uninstall()
    {
        _hookRefreshTimer?.Dispose();
        _hookRefreshTimer = null;
        // 摘钩必须回到装钩的线程上执行，否则句柄留在系统里继续吃按键。
        if (_hook != IntPtr.Zero && _hookDispatcher is not null)
            _hookDispatcher.Invoke(() =>
            {
                if (_hook != IntPtr.Zero) { UnhookWindowsHookEx(_hook); _hook = IntPtr.Zero; }
                _hookProc = null;
            });
        if (_msgWindow is not null)
        {
            UnregisterHotKey(_msgWindow.Handle, HOTKEY_ID);
            _msgWindow.Dispose();
            _msgWindow = null;
        }
        _lastCtrlUpMs = -1;
        _ctrlDown = false;
    }

    // ── Combo parser ───────────────────────────────────────────────────────

    /// <summary>
    /// 把 "Alt+Space"、"Ctrl+Shift+F1"、"Alt+D1" 等字符串解析为 (modifiers, vk)。
    /// 主键优先按 <see cref="System.Windows.Input.Key"/> 枚举名解析（与 HotkeyRecorderBox 输出一致）；
    /// 另接受单位数字 0-9 与常见别名。不认识的组合返回 (0, 0)。
    /// </summary>
    private static (uint mod, uint vk) ParseCombo(string combo)
    {
        uint mod = 0, vk = 0;
        foreach (var part in combo.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            switch (part.ToUpperInvariant())
            {
                case "ALT":   mod |= 0x0001; break;
                case "CTRL":
                case "CONTROL": mod |= 0x0002; break;
                case "SHIFT": mod |= 0x0004; break;
                case "WIN":
                case "WINDOWS":
                case "LWIN":
                case "RWIN":  mod |= 0x0008; break;
                default:
                    if (part.Length == 1 && part[0] is >= '0' and <= '9')
                    {
                        // "Alt+1" → Key.D1
                        if (Enum.TryParse<System.Windows.Input.Key>("D" + part, true, out var digitKey))
                            vk = (uint)System.Windows.Input.KeyInterop.VirtualKeyFromKey(digitKey);
                    }
                    else if (Enum.TryParse<System.Windows.Input.Key>(part, true, out var key))
                    {
                        vk = (uint)System.Windows.Input.KeyInterop.VirtualKeyFromKey(key);
                    }
                    break;
            }
        }
        return (mod, vk);
    }
}
