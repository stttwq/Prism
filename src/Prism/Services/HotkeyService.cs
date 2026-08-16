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
    private IntPtr _hook;
    private HwndSource? _msgWindow;
    private HotkeyMode _mode;

    // Double-Ctrl detection state
    private long _lastCtrlUpMs = -1;
    private bool _ctrlDown;
    private bool _otherKeyPressed;
    private const int DoubleClickMs = 400;
    private const int MaxHoldMs = 300;
    private long _ctrlDownMs;

    /// <summary>呼出事件，在 UI 线程触发。</summary>
    public event Action? Triggered;

    // ── Public API ─────────────────────────────────────────────────────────

    /// <summary>按 <paramref name="settings"/> 安装快捷键；可重复调用以切换模式。</summary>
    public void Apply(Settings settings)
    {
        Uninstall();
        _mode = settings.HotkeyMode;
        if (_mode == HotkeyMode.DoubleCtrl)
            InstallLowLevelHook();
        else
            InstallRegisterHotKey(settings.ComboHotkey);
    }

    public void Dispose() => Uninstall();

    // ── Low-level hook (DoubleCtrl) ────────────────────────────────────────

    private void InstallLowLevelHook()
    {
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
    }

    private IntPtr HookCallback(int code, IntPtr w, IntPtr l)
    {
        if (code >= 0)
        {
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
        if (_hook != IntPtr.Zero) { UnhookWindowsHookEx(_hook); _hook = IntPtr.Zero; }
        if (_msgWindow is not null)
        {
            UnregisterHotKey(_msgWindow.Handle, HOTKEY_ID);
            _msgWindow.Dispose();
            _msgWindow = null;
        }
        _hookProc = null;
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
