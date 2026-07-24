using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;
using Prism.Models;
using Prism.Services;

namespace Prism.Controls;

/// <summary>
/// 结果列表（frontend-spec.md ResultList）。
/// 虚拟化 ListBox；标题默认绑定 + MatchSpans 染蓝；图标经 IconCache 异步加载。
/// 高度按条数计算（最多 9 行），避免放在 StackPanel 里高度塌成 0。
/// </summary>
public partial class ResultList : UserControl
{
    private const double RowHeight = 62;
    private const int MaxVisibleRows = 9;
    private const double StatusRowHeight = 36;

    private IconCache? _icons;
    private IReadOnlyList<SearchResult> _items = Array.Empty<SearchResult>();
    private bool _syncing;
    private ScrollViewer? _scrollViewer;
    private bool _scrollHooked;

    public ResultList()
    {
        InitializeComponent();
        List.ItemContainerGenerator.StatusChanged += OnGeneratorStatusChanged;
        List.Loaded += (_, _) =>
        {
            HookScrollViewer();
            DecorateVisibleItems();
        };
    }

    public void SetIconCache(IconCache cache) => _icons = cache;

    /// <summary>结果集合。</summary>
    public IReadOnlyList<SearchResult> Items
    {
        get => _items;
        set
        {
            _items = value ?? Array.Empty<SearchResult>();
            _syncing = true;
            List.ItemsSource = null; // 强制刷新容器
            List.ItemsSource = _items;
            UpdateListHeight();
            _syncing = false;
            Dispatcher.BeginInvoke(
                System.Windows.Threading.DispatcherPriority.Loaded,
                () =>
                {
                    HookScrollViewer();
                    DecorateVisibleItems();
                });
        }
    }

    /// <summary>当前选中下标；-1 表示无选中。</summary>
    public int SelectedIndex
    {
        get => List.SelectedIndex;
        set
        {
            if (List.SelectedIndex == value) return;
            _syncing = true;
            List.SelectedIndex = value;
            if (value >= 0 && value < List.Items.Count)
                List.ScrollIntoView(List.Items[value]);
            _syncing = false;
        }
    }

    /// <summary>列表区状态提示（无结果 / 索引中 / 错误）。</summary>
    public string StatusMessage
    {
        get => StatusText.Text;
        set
        {
            StatusText.Text = value ?? "";
            var show = !string.IsNullOrEmpty(value);
            StatusText.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
            UpdateListHeight();
        }
    }

    /// <summary>选中项变化（用户点击）。</summary>
    public event Action<int>? SelectedIndexChanged;

    /// <summary>双击某项。</summary>
    public event Action<SearchResult>? ItemInvoked;

    private void UpdateListHeight()
    {
        var rows = Math.Min(_items.Count, MaxVisibleRows);
        var h = rows * RowHeight;
        if (!string.IsNullOrEmpty(StatusText.Text) && rows == 0)
            h = StatusRowHeight;
        // 有结果又有状态时，在列表下留一点状态行空间。
        if (!string.IsNullOrEmpty(StatusText.Text) && rows > 0)
            h += StatusRowHeight;

        if (h <= 0)
        {
            Height = double.NaN;
            List.Height = double.NaN;
            MinHeight = 0;
        }
        else
        {
            Height = h;
            List.Height = Math.Min(_items.Count, MaxVisibleRows) * RowHeight;
            MinHeight = h;
        }
    }

    private void OnSelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncing) return;
        SelectedIndexChanged?.Invoke(List.SelectedIndex);
        DecorateVisibleItems();
    }

    private void OnDoubleClick(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        if (List.SelectedItem is SearchResult r)
            ItemInvoked?.Invoke(r);
    }

    private void OnGeneratorStatusChanged(object? sender, EventArgs e)
    {
        if (List.ItemContainerGenerator.Status ==
            System.Windows.Controls.Primitives.GeneratorStatus.ContainersGenerated)
            DecorateVisibleItems();
    }

    private void HookScrollViewer()
    {
        if (_scrollHooked) return;
        _scrollViewer = FindDescendant<ScrollViewer>(List, null);
        if (_scrollViewer is null) return;
        _scrollViewer.ScrollChanged += (_, _) => DecorateVisibleItems();
        _scrollHooked = true;
    }

    private void DecorateVisibleItems()
    {
        HookScrollViewer();

        for (var i = 0; i < List.Items.Count; i++)
        {
            if (List.ItemContainerGenerator.ContainerFromIndex(i) is not ListBoxItem container)
                continue;
            if (List.Items[i] is not SearchResult item)
                continue;

            var titleBlock = FindDescendant<TextBlock>(container, "TitleBlock");
            var hotkey = FindDescendant<TextBlock>(container, "HotkeyHint");
            var icon = FindDescendant<Image>(container, "IconImage");

            if (titleBlock is not null)
                ApplyMatchSpans(titleBlock, item.Title, item.MatchSpans);

            if (hotkey is not null)
            {
                if (i < 9 && item.Kind != "more")
                {
                    hotkey.Text = $"Ctrl+{i + 1}";
                    hotkey.Visibility = Visibility.Visible;
                }
                else
                {
                    hotkey.Text = "";
                    hotkey.Visibility = Visibility.Collapsed;
                }
            }

            if (icon is null || _icons is null) continue;

            if (item.Kind == "more" || string.IsNullOrEmpty(item.ExecuteId))
            {
                icon.Tag = null;
                icon.Source = null;
            }
            else
            {
                var path = item.ExecuteId;
                if (!Equals(icon.Tag as string, path))
                {
                    icon.Tag = path;
                    icon.Source = null;
                    _ = LoadIconAsync(path, icon);
                }
            }
        }
    }

    private async Task LoadIconAsync(string path, Image target)
    {
        if (_icons is null) return;
        try
        {
            var src = await _icons.GetAsync(path).ConfigureAwait(true);
            if (!Equals(target.Tag as string, path)) return;
            target.Source = src;
        }
        catch
        {
            // 图标失败不影响搜索。
        }
    }

    private static void ApplyMatchSpans(TextBlock block, string title, int[] spans)
    {
        block.Inlines.Clear();
        if (string.IsNullOrEmpty(title))
            return;

        var matchBrush = TryFindBrush(block, "TextMatch")
            ?? new SolidColorBrush(Color.FromRgb(0x1E, 0x7A, 0xD4));
        var normalBrush = TryFindBrush(block, "TextTitle")
            ?? new SolidColorBrush(Color.FromRgb(0x30, 0x32, 0x37));

        if (spans is not { Length: >= 2 })
        {
            block.Inlines.Add(new Run(title) { Foreground = normalBrush });
            return;
        }

        var ranges = new List<(int start, int len)>();
        for (var i = 0; i + 1 < spans.Length; i += 2)
        {
            var s = Math.Clamp(spans[i], 0, title.Length);
            var l = Math.Max(0, Math.Min(spans[i + 1], title.Length - s));
            if (l > 0) ranges.Add((s, l));
        }
        ranges.Sort((a, b) => a.start.CompareTo(b.start));

        var cursor = 0;
        foreach (var (start, len) in ranges)
        {
            if (start > cursor)
                block.Inlines.Add(new Run(title[cursor..start]) { Foreground = normalBrush });
            var end = Math.Min(title.Length, start + len);
            if (end > start)
                block.Inlines.Add(new Run(title[start..end]) { Foreground = matchBrush });
            cursor = Math.Max(cursor, end);
        }
        if (cursor < title.Length)
            block.Inlines.Add(new Run(title[cursor..]) { Foreground = normalBrush });
    }

    private static Brush? TryFindBrush(FrameworkElement el, string key)
        => el.TryFindResource(key) as Brush;

    private static T? FindDescendant<T>(DependencyObject root, string? name) where T : FrameworkElement
    {
        var count = VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < count; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T fe && (name is null || fe.Name == name))
                return fe;
            var nested = FindDescendant<T>(child, name);
            if (nested is not null) return nested;
        }
        return null;
    }
}
