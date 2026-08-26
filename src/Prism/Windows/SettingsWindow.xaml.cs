using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;

namespace Prism.Windows;

/// <summary>设置窗口：常规 / 网页搜索 / 关于（frontend-spec SettingsWindow）。</summary>
public partial class SettingsWindow : Window
{
    private const int DwmwaUseImmersiveDarkMode = 20;        // Win10 2004+ / Win11
    private const int DwmwaUseImmersiveDarkModeLegacy = 19;  // Win10 1809–1909

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(
        IntPtr hwnd, int attr, ref int value, int size);

    public SettingsWindow(SettingsViewModel viewModel)
    {
        InitializeComponent();
        DataContext = viewModel;
        // 别名系统（2026-08-21 设想）：打开时拉取列表（失败静默，列表留空）。
        Loaded += async (_, _) => await viewModel.LoadAliasesAsync().ConfigureAwait(true);
        // 问题4：深色标题栏须在 HWND 创建后调用 DWM。设置窗短生命周期，
        // 开窗期间改系统主题属可忽略边角情况，不订阅 ThemeApplied。
        SourceInitialized += (_, _) => ApplyTitleBarTheme();
    }

    private void OnCloseClick(object sender, RoutedEventArgs e) => Close();

    /// <summary>按系统主题把 DWM 非客户区（标题栏/边框/系统按钮）切深浅色。</summary>
    private void ApplyTitleBarTheme()
    {
        var hwnd = new WindowInteropHelper(this).Handle;
        if (hwnd == IntPtr.Zero) return;
        var dark = ThemeWatcher.ReadSystemTheme() == AppTheme.Dark ? 1 : 0;
        // 先试 20（Win10 2004+/Win11），失败回退 19（Win10 1809–1909）。
        // 失败时 DwmSetWindowAttribute 返回非 0 且无副作用，两次调用安全。
        if (DwmSetWindowAttribute(hwnd, DwmwaUseImmersiveDarkMode, ref dark, sizeof(int)) != 0)
            DwmSetWindowAttribute(hwnd, DwmwaUseImmersiveDarkModeLegacy, ref dark, sizeof(int));
    }
}
