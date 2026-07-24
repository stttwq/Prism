using System.Windows;
using Prism.ViewModels;

namespace Prism.Windows;

/// <summary>设置窗口（步骤 7 最小实现）。</summary>
public partial class SettingsWindow : Window
{
    public SettingsWindow(SettingsViewModel viewModel)
    {
        InitializeComponent();
        DataContext = viewModel;
    }
}
