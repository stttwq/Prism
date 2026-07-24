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
    }

    private void OnCloseClick(object sender, RoutedEventArgs e) => Close();
}
