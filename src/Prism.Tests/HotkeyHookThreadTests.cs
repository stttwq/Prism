using System.Runtime.InteropServices;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// WH_KEYBOARD_LL 装在哪个线程上的实机探针。
///
/// The regression this exists to catch: SetWindowsHookEx(WH_KEYBOARD_LL) binds the hook to the
/// message queue of the calling thread, and the system delivers callbacks by dispatching to that
/// queue. Install it from a thread pool thread (no message loop) and the hook handle is non-zero,
/// the install "succeeds", and nothing ever fires — while every keystroke system-wide stalls until
/// LowLevelHooksTimeout (~300ms) expires. Asserting on a non-zero hook handle cannot see that;
/// only injecting real Ctrl taps and watching the callback run can.
///
/// Unlike a pure unit test these inject real keystrokes, so they are [Fact(Skip)] like the
/// WindowActivatorLiveTests probes. A Ctrl tap steals no foreground, but it does perturb the
/// WPF layout tests running in parallel (measured: 1 red in 10 full-suite runs, 0 in 8 runs
/// without this file), and a tray-resident Prism on the same desktop pops its search window.
/// Run manually:
///
/// <code>
/// dotnet test src/Prism.Tests/Prism.Tests.csproj -c Release ^
///   --filter "FullyQualifiedName~HotkeyHookThreadTests"
/// </code>
///
/// Both probes were confirmed red against the pre-fix install-on-calling-thread code and green
/// after, so their green is not free.
/// </summary>
public sealed class HotkeyHookThreadTests
{
    [Fact(Skip = "实机探针：注入 Ctrl 按键，会扰动并行的 WPF 布局测试。手动去掉 Skip 后运行。")]
    public void DoubleCtrlFiresAfterTheHookIsInstalled()
    {
        Assert.True(FiresOnInjectedDoubleCtrl(refreshFirst: false), "hook installed by Apply never fired");
    }

    /// <summary>
    /// 定时兜底重装后仍必须能触发。
    ///
    /// This is the exact path that broke: the 60s refresh timer runs on a thread pool thread, and
    /// re-installing there moved a working hook onto a queue nobody pumps. The user saw
    /// double-Ctrl work right after a tray click, then die about a minute later.
    /// </summary>
    [Fact(Skip = "实机探针：注入 Ctrl 按键，会扰动并行的 WPF 布局测试。手动去掉 Skip 后运行。")]
    public void DoubleCtrlStillFiresAfterAThreadPoolTriggeredRefresh()
    {
        Assert.True(FiresOnInjectedDoubleCtrl(refreshFirst: true), "hook died after a refresh off the UI thread");
    }

    private static bool FiresOnInjectedDoubleCtrl(bool refreshFirst)
    {
        using var hotkey = new HotkeyService();
        hotkey.Apply(new Settings { HotkeyMode = HotkeyMode.DoubleCtrl });

        // Apply hands the install to the hook thread asynchronously.
        for (var i = 0; i < 100 && hotkey._hook == IntPtr.Zero; i++) Thread.Sleep(20);
        Assert.NotEqual(IntPtr.Zero, hotkey._hook);

        if (refreshFirst)
        {
            // Reproduce the timer's calling context exactly: a thread pool thread.
            var refreshed = new ManualResetEventSlim(false);
            ThreadPool.QueueUserWorkItem(_ => { hotkey.InstallLowLevelHook(); refreshed.Set(); });
            Assert.True(refreshed.Wait(TimeSpan.FromSeconds(5)), "refresh call never returned");
            Thread.Sleep(200); // let the hook thread process the re-install
        }

        // Triggered is raised through Application.Current.Dispatcher, and there is no Application in
        // the test host, so that path silently drops the event. _callbackCount is the decisive
        // observation instead: it can only move if HookCallback actually ran, which can only happen
        // if the hook sits on a thread that pumps messages.
        var before = hotkey._callbackCount;
        TapCtrl();
        Thread.Sleep(60);
        TapCtrl();

        for (var i = 0; i < 50; i++)
        {
            if (hotkey._callbackCount > before) return true;
            Thread.Sleep(20);
        }
        return false;
    }

    private static void TapCtrl()
    {
        // Inject from a thread that does not own the hook. Injecting from the installing thread lets
        // the kernel dispatch the callback inline during the keybd_event syscall, so a hook on an
        // unpumped thread still fires — the probe goes green for the wrong reason and stops being
        // able to see the bug.
        var t = new Thread(() =>
        {
            keybd_event(VK_CONTROL, 0, 0, IntPtr.Zero);
            Thread.Sleep(20); // under MaxHoldMs (300ms), or the tap is discarded as a hold
            keybd_event(VK_CONTROL, 0, KEYEVENTF_KEYUP, IntPtr.Zero);
        }) { IsBackground = true };
        t.Start();
        Assert.True(t.Join(TimeSpan.FromSeconds(5)), "注入线程超时");
    }

    private const byte VK_CONTROL = 0x11;
    private const uint KEYEVENTF_KEYUP = 0x0002;

    // keybd_event over SendInput: SendInput's INPUT struct is 40 bytes on x64 because its union is
    // sized by MOUSEINPUT, and a keyboard-only union declaration silently computes 32 — SendInput
    // then rejects the call and returns 0. keybd_event forwards to the same injection path with no
    // struct to get wrong.
    [DllImport("user32.dll")]
    private static extern void keybd_event(byte vk, byte scan, uint flags, IntPtr extra);
}
