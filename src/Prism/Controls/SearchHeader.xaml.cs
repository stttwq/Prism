using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
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
        Placeholder.Text = "搜索应用和文件";
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
