using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using Prism.Models;

namespace Prism.Controls;

/// <summary>
/// 搜索输入区（frontend-spec.md SearchHeader）。
/// Idle/Results：占位"搜索应用和文件"；Actions：左侧"动作"+竖线，输入可过滤动作。
/// 文本变化与方向键/Enter/Esc/←/→/Ctrl+N 上抛给 SearchWindow 处理。
/// </summary>
public partial class SearchHeader : UserControl
{
    public SearchHeader()
    {
        InitializeComponent();
        SetMode(PanelMode.Idle);
    }

    /// <summary>当前输入文字。</summary>
    public string Query
    {
        get => QueryBox.Text;
        set
        {
            if (QueryBox.Text == value) return;
            QueryBox.Text = value;
            QueryBox.CaretIndex = value?.Length ?? 0;
            UpdatePlaceholder();
        }
    }

    /// <summary>输入文字变化。</summary>
    public event Action<string>? QueryChanged;

    /// <summary>方向键 / Enter / Esc / ←/→ / Ctrl+1..9 等按键上抛。</summary>
    public event Action<KeyEventArgs>? QueryKeyDown;

    /// <summary>范围标签被点击（与 Ctrl+G 等价）。</summary>
    public event Action? ScopeToggleRequested;

    /// <summary>刷新范围标签：不可见时隐藏，可见时显示当前目录或全局。</summary>
    public void SetScope(bool visible, string label, string tooltip)
    {
        var wasVisible = ScopeChipColumn.Width != new GridLength(0);
        ScopeChip.Content = label;
        ScopeChip.ToolTip = string.IsNullOrEmpty(tooltip) ? null : tooltip;
        // 无障碍：屏幕阅读器读到的是完整范围说明，而不是截断的目录名。
        System.Windows.Automation.AutomationProperties.SetName(
            ScopeChip,
            string.IsNullOrEmpty(tooltip) ? label : $"{label}。{tooltip}");

        if (!visible)
        {
            ScopeChipColumn.Width = new GridLength(0);
            ScopeChip.Visibility = Visibility.Collapsed;
            return;
        }

        // 列宽恢复自适应，让 chip 有空间显示。
        ScopeChipColumn.Width = GridLength.Auto;
        ScopeChip.Visibility = Visibility.Visible;

        // 隐藏→可见的跃迁做一次淡入 + 左滑入场（120ms）。
        if (!wasVisible)
        {
            var ease = new CubicEase { EasingMode = EasingMode.EaseOut };
            var ms = System.TimeSpan.FromMilliseconds(120);
            ScopeChip.BeginAnimation(
                OpacityProperty,
                new DoubleAnimation(0, 1, ms) { EasingFunction = ease });
            if (ScopeChip.RenderTransform is not TranslateTransform slide)
            {
                slide = new TranslateTransform();
                ScopeChip.RenderTransform = slide;
            }
            slide.BeginAnimation(
                TranslateTransform.XProperty,
                new DoubleAnimation(-8, 0, ms) { EasingFunction = ease });
        }
    }

    private void OnScopeChipClick(object sender, RoutedEventArgs e) =>
        ScopeToggleRequested?.Invoke();

    public void FocusQuery()
    {
        QueryBox.Focusable = true;
        QueryBox.Focus();
        Keyboard.Focus(QueryBox);
        if (!string.IsNullOrEmpty(QueryBox.Text))
            QueryBox.SelectAll();
        else
            QueryBox.CaretIndex = 0;
    }

    public void ClearQuery()
    {
        QueryBox.Text = "";
        UpdatePlaceholder();
    }

    /// <summary>按面板模式切换"动作"标签与占位文案。</summary>
    public void SetMode(PanelMode mode)
    {
        var actions = mode == PanelMode.Actions;
        ModeLabel.Visibility = actions ? Visibility.Visible : Visibility.Collapsed;
        ModeDivider.Visibility = actions ? Visibility.Visible : Visibility.Collapsed;
        // 审计 U3：动作模式下输入框筛选的是动作列表，占位文案必须跟着换，
        // 否则用户会以为还能在这里搜文件（此前恒为"搜索应用和文件"）。
        Placeholder.Text = actions ? "输入以筛选动作" : "搜索应用和文件";
        UpdatePlaceholder();
    }

    private void OnQueryTextChanged(object sender, TextChangedEventArgs e)
    {
        UpdatePlaceholder();
        QueryChanged?.Invoke(QueryBox.Text);
    }

    private void OnQueryPreviewKeyDown(object sender, KeyEventArgs e)
    {
        switch (e.Key)
        {
            case Key.Up:
            case Key.Down:
            case Key.Enter:
            case Key.Escape:
            case Key.Left:
            case Key.Right:
            case Key.PageUp:
            case Key.PageDown:
                QueryKeyDown?.Invoke(e);
                break;
            default:
                if (Keyboard.Modifiers.HasFlag(ModifierKeys.Control))
                    QueryKeyDown?.Invoke(e);
                break;
        }
    }

    private void UpdatePlaceholder()
    {
        Placeholder.Visibility = string.IsNullOrEmpty(QueryBox.Text)
            ? Visibility.Visible
            : Visibility.Collapsed;
    }
}
