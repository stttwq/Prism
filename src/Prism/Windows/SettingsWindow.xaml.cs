using System.Windows;
using Prism.ViewModels;

namespace Prism.Windows;

/// <summary>设置窗口：常规 / 网页搜索 / 关于（frontend-spec SettingsWindow）。</summary>
public partial class SettingsWindow : Window
{
    public SettingsWindow(SettingsViewModel viewModel)
    {
        InitializeComponent();
        DataContext = viewModel;
        // 别名系统（2026-08-21 设想）：打开时拉取列表（失败静默，列表留空）。
        Loaded += async (_, _) => await viewModel.LoadAliasesAsync().ConfigureAwait(true);
    }

    private void OnCloseClick(object sender, RoutedEventArgs e) => Close();
}
