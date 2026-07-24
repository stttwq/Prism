using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using Prism.Models;

namespace Prism.Controls;

/// <summary>
/// 动作面板（frontend-spec.md ActionPanel）。行高 44px；节标题与子菜单箭头。
/// </summary>
public partial class ActionPanel : UserControl
{
    private const double RowHeight = 44;
    private const int MaxVisibleRows = 10;

    private IReadOnlyList<ActionItem> _items = Array.Empty<ActionItem>();
    private bool _syncing;

    public ActionPanel()
    {
        InitializeComponent();
    }

    public IReadOnlyList<ActionItem> Items
    {
        get => _items;
        set
        {
            _items = value ?? Array.Empty<ActionItem>();
            _syncing = true;
            List.ItemsSource = _items;
            UpdateHeight();
            _syncing = false;
        }
    }

    public int SelectedIndex
    {
        get => List.SelectedIndex;
        set
        {
            if (List.SelectedIndex == value) return;
            _syncing = true;
            // 跳过节标题，落到可执行项。
            var idx = ClampToSelectable(value);
            List.SelectedIndex = idx;
            if (idx >= 0 && idx < List.Items.Count)
                List.ScrollIntoView(List.Items[idx]);
            _syncing = false;
        }
    }

    public event Action<int>? SelectedIndexChanged;
    public event Action<ActionItem>? ActionInvoked;

    public void MoveSelection(int delta)
    {
        if (_items.Count == 0) return;
        var i = SelectedIndex;
        if (i < 0) i = 0;
        var next = i;
        for (var step = 0; step < _items.Count; step++)
        {
            next = Math.Clamp(next + delta, 0, _items.Count - 1);
            if (!_items[next].IsSectionHeader) break;
            if (next == 0 || next == _items.Count - 1) break;
        }
        if (_items[next].IsSectionHeader)
        {
            // 整段都是标题时不选。
            return;
        }
        SelectedIndex = next;
        SelectedIndexChanged?.Invoke(SelectedIndex);
    }

    private int ClampToSelectable(int value)
    {
        if (_items.Count == 0) return -1;
        var idx = Math.Clamp(value, 0, _items.Count - 1);
        if (!_items[idx].IsSectionHeader) return idx;
        for (var i = idx + 1; i < _items.Count; i++)
            if (!_items[i].IsSectionHeader) return i;
        for (var i = idx - 1; i >= 0; i--)
            if (!_items[i].IsSectionHeader) return i;
        return -1;
    }

    private void UpdateHeight()
    {
        var rows = Math.Min(_items.Count, MaxVisibleRows);
        var h = rows * RowHeight;
        if (h <= 0)
        {
            Height = double.NaN;
            List.Height = double.NaN;
            MinHeight = 0;
        }
        else
        {
            Height = h;
            List.Height = h;
            MinHeight = h;
        }
    }

    private void OnSelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        // 用户点到节标题时弹回。
        if (List.SelectedItem is ActionItem ai && ai.IsSectionHeader)
        {
            _syncing = true;
            List.SelectedIndex = ClampToSelectable(List.SelectedIndex);
            _syncing = false;
        }
        SelectedIndexChanged?.Invoke(List.SelectedIndex);
    }

    private void OnDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (List.SelectedItem is ActionItem { IsSectionHeader: false } ai)
            ActionInvoked?.Invoke(ai);
    }

    private void OnItemPreviewMouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        // 单击也可执行（动作面板比结果列表更像菜单）。
        if (sender is ListBoxItem { DataContext: ActionItem { IsSectionHeader: false } ai })
        {
            ActionInvoked?.Invoke(ai);
            e.Handled = true;
        }
    }
}
