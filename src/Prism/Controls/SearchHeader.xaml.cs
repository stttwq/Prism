using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;

namespace Prism.Controls;

/// <summary>
/// 搜索输入区（frontend-spec.md SearchHeader）。
/// 文本变化与方向键/Enter/Esc/Ctrl+N 上抛给 SearchWindow 处理。
/// </summary>
public partial class SearchHeader : UserControl
{
    public SearchHeader()
    {
        InitializeComponent();
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

    /// <summary>方向键 / Enter / Esc / Ctrl+1..9 等按键上抛。</summary>
    public event Action<KeyEventArgs>? QueryKeyDown;

    public void FocusQuery()
    {
        // 空串时 SelectAll 无意义，但仍要确保键盘焦点进 TextBox，
        // 否则 Idle 态 Esc 进不了 PreviewKeyDown。
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

    private void OnQueryTextChanged(object sender, TextChangedEventArgs e)
    {
        UpdatePlaceholder();
        QueryChanged?.Invoke(QueryBox.Text);
    }

    private void OnQueryPreviewKeyDown(object sender, KeyEventArgs e)
    {
        // 导航/执行键交给外层；普通字符留给 TextBox 自己处理。
        switch (e.Key)
        {
            case Key.Up:
            case Key.Down:
            case Key.Enter:
            case Key.Escape:
            case Key.PageUp:
            case Key.PageDown:
                QueryKeyDown?.Invoke(e);
                break;
            default:
                // Ctrl+1..9 / Ctrl+Enter
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
