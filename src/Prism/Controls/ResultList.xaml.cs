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
/// "more" 行使用蓝底双箭头装饰图标。
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
    private Brush? _matchBrush;
    private Brush? _normalBrush;
    private ImageSource? _moreIcon;

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

    /// <summary>主题切换后丢弃缓存画刷，下次装饰时重新取。</summary>
    public void InvalidateThemeBrushes()
    {
        _matchBrush = null;
        _normalBrush = null;
        _moreIcon = null;
        DecorateVisibleItems();
    }

    public IReadOnlyList<SearchResult> Items
    {
        get => _items;
        set
        {
            var next = value ?? Array.Empty<SearchResult>();
            if (ReferenceEquals(_items, next))
            {
                UpdateListHeight();
                return;
            }
            _items = next;
            _syncing = true;
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

    public event Action<int>? SelectedIndexChanged;
    public event Action<SearchResult>? ItemInvoked;

    private void UpdateListHeight()
    {
        var rows = Math.Min(_items.Count, MaxVisibleRows);
        var h = rows * RowHeight;
        if (!string.IsNullOrEmpty(StatusText.Text) && rows == 0)
            h = StatusRowHeight;
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

    private void OnPreviewMouseLeftButtonUp(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        var source = e.OriginalSource as DependencyObject;
        if (source is null) return;
        if (ItemsControl.ContainerFromElement(List, source) is not ListBoxItem container) return;
        if (container.DataContext is SearchResult { Kind: "more" } result)
            ItemInvoked?.Invoke(result);
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

            if (icon is null) continue;

            if (item.Kind == "more")
            {
                icon.Tag = "more";
                icon.Source = MoreIcon();
                continue;
            }

            if (item.Kind is "web" || string.IsNullOrEmpty(item.ExecuteId) || _icons is null)
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

    private ImageSource MoreIcon()
    {
        if (_moreIcon is not null) return _moreIcon;

        // 蓝底圆角方块 + 白色双箭头（用 DrawingImage，避免位图依赖）。
        var blue = TryFindBrush(this, "TextMatch")
            ?? Freeze(new SolidColorBrush(Color.FromRgb(0x1E, 0x7A, 0xD4)));
        var white = Freeze(new SolidColorBrush(Colors.White));

        var group = new DrawingGroup();
        using (var ctx = group.Open())
        {
            ctx.DrawRoundedRectangle(blue, null, new Rect(0, 0, 32, 32), 6, 6);
            // 简易双箭头：两条折线
            var pen = new Pen(white, 2.2) { StartLineCap = PenLineCap.Round, EndLineCap = PenLineCap.Round, LineJoin = PenLineJoin.Round };
            if (pen.CanFreeze) pen.Freeze();
            var geo1 = new PathGeometry(new[]
            {
                new PathFigure(new Point(10, 12), new[] { new LineSegment(new Point(16, 8), true), new LineSegment(new Point(22, 12), true) }, false),
            });
            var geo2 = new PathGeometry(new[]
            {
                new PathFigure(new Point(10, 20), new[] { new LineSegment(new Point(16, 24), true), new LineSegment(new Point(22, 20), true) }, false),
            });
            ctx.DrawGeometry(null, pen, geo1);
            ctx.DrawGeometry(null, pen, geo2);
        }
        if (group.CanFreeze) group.Freeze();
        var img = new DrawingImage(group);
        if (img.CanFreeze) img.Freeze();
        _moreIcon = img;
        return img;
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

    private Brush MatchBrush(FrameworkElement el) =>
        _matchBrush ??= TryFindBrush(el, "TextMatch")
            ?? Freeze(new SolidColorBrush(Color.FromRgb(0x1E, 0x7A, 0xD4)));

    private Brush NormalBrush(FrameworkElement el) =>
        _normalBrush ??= TryFindBrush(el, "TextTitle")
            ?? Freeze(new SolidColorBrush(Color.FromRgb(0x30, 0x32, 0x37)));

    private static Brush Freeze(SolidColorBrush b)
    {
        if (b.CanFreeze) b.Freeze();
        return b;
    }

    private void ApplyMatchSpans(TextBlock block, string title, int[] spans)
    {
        block.Inlines.Clear();
        if (string.IsNullOrEmpty(title))
            return;

        var matchBrush = MatchBrush(block);
        var normalBrush = NormalBrush(block);

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
