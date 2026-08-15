using System.Collections.ObjectModel;
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
    private WebIconProvider? _webIcons;
    private IReadOnlyList<SearchResult> _items = Array.Empty<SearchResult>();
    private readonly ObservableCollection<SearchResult> _displayItems = [];
    private bool _syncing;
    private ScrollViewer? _scrollViewer;
    private bool _scrollHooked;
    private Brush? _matchBrush;
    private Brush? _normalBrush;
    private ImageSource? _moreIcon;

    public ResultList()
    {
        InitializeComponent();
        List.ItemsSource = _displayItems;
        List.ItemContainerGenerator.StatusChanged += OnGeneratorStatusChanged;
        List.Loaded += (_, _) =>
        {
            HookScrollViewer();
            // 挂进可视树后才拿得到真实 DPI；此前 UpdateListHeight 只能按 1.0 算行高。
            UpdateListHeight();
            DecorateVisibleItems();
        };
    }

    public void SetIconCache(IconCache cache) => _icons = cache;
    public void SetWebIconProvider(WebIconProvider provider) => _webIcons = provider;

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
            if (ReferenceEquals(_items, next)) return;

            var countChanged = _items.Count != next.Count;
            _items = next;
            _syncing = true;
            try
            {
                SynchronizeDisplayItems(next);
            }
            finally
            {
                _syncing = false;
            }

            // 立即装饰一次（不在 _syncing 内），让回收的容器在本次同步后就显示正确文字——
            // 不能只靠下面的 BeginInvoke，否则 DataContext 已变但 Inlines 仍是旧内容的窗口里
            // 用户会看到残留文字叠加。
            DecorateVisibleItems();

            if (countChanged)
                UpdateListHeight();
            Dispatcher.BeginInvoke(
                System.Windows.Threading.DispatcherPriority.Loaded,
                () =>
                {
                    HookScrollViewer();
                    DecorateVisibleItems();
                });
        }
    }

    /// <summary>
    /// 只增删移动，不替换同键项。替换会触发 Replace 通知让 ListBox 重建行容器，
    /// 新容器在下次装饰前 Image 为空——每按一键一帧空白，正是网页图标闪烁的来源。
    /// Title/Subtitle/MatchSpans 全部由 DecorateVisibleItems 从 _items 原地重绘。
    ///
    /// 代价：_displayItems[i] 可能是比 _items[i] 更旧的实例。行模板刻意不绑定任何字段，
    /// 所有事件处理都按索引从 _items 取当前项（ItemAt）。给模板加 {Binding} 前须重新审视这里。
    /// </summary>
    private void SynchronizeDisplayItems(IReadOnlyList<SearchResult> next)
    {
        for (var i = 0; i < next.Count; i++)
        {
            var desired = next[i];
            if (i < _displayItems.Count && HasSameKey(_displayItems[i], desired))
                continue;

            var existingIndex = FindByKey(desired, i + 1);
            if (existingIndex >= 0)
                _displayItems.Move(existingIndex, i);
            else
                _displayItems.Insert(i, desired);
        }

        while (_displayItems.Count > next.Count)
            _displayItems.RemoveAt(_displayItems.Count - 1);
    }

    private int FindByKey(SearchResult desired, int startIndex)
    {
        for (var i = startIndex; i < _displayItems.Count; i++)
        {
            if (HasSameKey(_displayItems[i], desired))
                return i;
        }
        return -1;
    }

    private static bool HasSameKey(SearchResult left, SearchResult right) =>
        string.Equals(left.ContainerKey, right.ContainerKey, StringComparison.Ordinal);

    /// <summary>按索引取当前项；容器 DataContext 可能是旧实例，不可用于执行。</summary>
    private SearchResult? ItemAt(int index) =>
        index >= 0 && index < _items.Count ? _items[index] : null;

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
            var next = value ?? "";
            if (string.Equals(StatusText.Text, next, StringComparison.Ordinal)) return;

            StatusText.Text = next;
            var show = !string.IsNullOrEmpty(next);
            StatusText.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
            UpdateListHeight();
        }
    }

    public event Action<int>? SelectedIndexChanged;
    public event Action<SearchResult>? ItemInvoked;
    public event Action<SearchResult>? ContextMenuRequested;

    private void UpdateListHeight()
    {
        var rows = Math.Min(_items.Count, MaxVisibleRows);
        var h = rows * SnappedRowHeight();

        // 行数没超过可视上限时必须彻底禁掉滚动，不能只依赖"高度刚好装得下"。
        ScrollViewer.SetVerticalScrollBarVisibility(
            List,
            _items.Count > MaxVisibleRows
                ? ScrollBarVisibility.Auto
                : ScrollBarVisibility.Disabled);

        if (h <= 0)
        {
            // No result rows: let the StackPanel auto-size to StatusText (if any).
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

    /// <summary>
    /// 单行占用的实际布局高度：UseLayoutRounding 会把每行 62 DIP 向上贴到整数设备像素，
    /// 所以非整数缩放下真实行高比 62 略大。
    ///
    /// 必须按贴齐后的值分配高度，否则 9 行内容会比 9*62 的视口高出 1~2px。
    /// ListBox 是按项滚动的（CanContentScroll=True）：视口装不下第 9 项就报 ViewportHeight=8、
    /// ScrollableHeight=1，方向键选到第 9 行触发 ScrollIntoView 时整滚一项——
    /// 第一行被顶出视口，末尾露出一整行 62px 的白方框。
    /// 单纯把滚动条设成 Disabled 拦不住这条路径（ScrollIntoView 仍会改 offset），
    /// 唯一可靠的办法是让视口真的装得下。
    /// </summary>
    private double SnappedRowHeight()
    {
        var scale = 1.0;
        if (PresentationSource.FromVisual(this) is not null)
        {
            var dpi = VisualTreeHelper.GetDpi(this);
            if (dpi.DpiScaleY > 0) scale = dpi.DpiScaleY;
        }
        if (Math.Abs(scale - 1.0) < 0.0001) return RowHeight;
        return Math.Ceiling(RowHeight * scale) / scale;
    }

    protected override void OnDpiChanged(DpiScale oldDpi, DpiScale newDpi)
    {
        base.OnDpiChanged(oldDpi, newDpi);
        UpdateListHeight();
        // DPI 变化后图标身份键带新尺寸，重装饰会触发按新尺寸重载。
        DecorateVisibleItems();
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
        // Resolve from _items, not container.DataContext: same-key rows are never replaced in
        // _displayItems, so a container can still hold an earlier instance of that row.
        var index = List.ItemContainerGenerator.IndexFromContainer(container);
        if (ItemAt(index) is { Kind: "more" } result)
            ItemInvoked?.Invoke(result);
    }

    private void OnMouseRightButtonUp(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        var source = e.OriginalSource as DependencyObject;
        if (source is null) return;
        if (ItemsControl.ContainerFromElement(List, source) is not ListBoxItem container) return;

        var index = List.ItemContainerGenerator.IndexFromContainer(container);
        if (ItemAt(index) is not { } result) return;
        if (result.Kind is not ("app" or "file" or "folder") || string.IsNullOrEmpty(result.ExecuteId))
            return;

        List.SelectedIndex = index;
        ContextMenuRequested?.Invoke(result);
        e.Handled = true;
    }

    private void OnDoubleClick(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        // 从点击位置解析实际行，不用 List.SelectedIndex：双击的第一次点击会经
        // PreviewMouseLeftButtonUp 触发 ShowMoreAsync，其异步搜索可能在 DoubleClick
        // 触发前就把 SelectedIndex 重置为 0，导致此处拿到的是第一行而非双击的行。
        var source = e.OriginalSource as DependencyObject;
        if (source is null) return;
        if (ItemsControl.ContainerFromElement(List, source) is not ListBoxItem container) return;
        var index = List.ItemContainerGenerator.IndexFromContainer(container);
        if (ItemAt(index) is { } r)
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
        if (_syncing) return;
        HookScrollViewer();

        for (var i = 0; i < List.Items.Count; i++)
        {
            if (List.ItemContainerGenerator.ContainerFromIndex(i) is not ListBoxItem container)
                continue;
            if (i >= _items.Count)
                continue;
            var item = _items[i];

            var titleBlock = FindDescendant<TextBlock>(container, "TitleBlock");
            var subtitleBlock = FindDescendant<TextBlock>(container, "SubtitleBlock");
            var hotkey = FindDescendant<TextBlock>(container, "HotkeyHint");
            var icon = FindDescendant<Image>(container, "IconImage");

            if (titleBlock is not null)
            {
                // 脏检查：SearchResult 是不可变 record，(标题, 高亮区间) 引用都相同即内容相同。
                // 每次按键周期本方法会执行 2-4 遍、滚动事件也全量重刷——跳过未变行可省掉
                // Inline 集合重建与文本布局重算（逐键与滚动的主要掉帧来源之一）。
                var applied = titleBlock.Tag as AppliedTitle;
                if (applied is null
                    || !ReferenceEquals(applied.Title, item.Title)
                    || !ReferenceEquals(applied.Spans, item.MatchSpans))
                {
                    ApplyMatchSpans(titleBlock, item.Title, item.MatchSpans);
                    titleBlock.Tag = new AppliedTitle { Title = item.Title, Spans = item.MatchSpans };
                }
            }

            if (subtitleBlock is not null
                && !string.Equals(subtitleBlock.Text, item.Subtitle, StringComparison.Ordinal))
                subtitleBlock.Text = item.Subtitle;

            if (hotkey is not null)
            {
                if (i < 9 && item.Kind != "more")
                {
                    var hint = $"Ctrl+{i + 1}";
                    if (!string.Equals(hotkey.Text, hint, StringComparison.Ordinal))
                        hotkey.Text = hint;
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

            // "window" carries an enumeration token, not a path — asking the shell for an
            // icon from it would just fail per row.
            if (item.Kind == "web")
            {
                if (_webIcons is not null)
                {
                    // Key on the engine, not the URL: the URL carries the query, so keying on
                    // it would reassign Source on every keystroke — a visible icon flicker.
                    var webKey = "web:" + _webIcons.IconKey(item.ExecuteId);
                    if (!Equals(icon.Tag as string, webKey))
                    {
                        icon.Tag = webKey;
                        icon.Source = _webIcons.GetIcon(item.ExecuteId);
                    }
                }
                else
                {
                    icon.Tag = null;
                    icon.Source = null;
                }
            }
            else if (item.Kind == "window"
                || string.IsNullOrEmpty(item.ExecuteId)
                || _icons is null)
            {
                icon.Tag = null;
                icon.Source = null;
            }
            else
            {
                var path = item.ExecuteId;
                var identity = path + "@" + IconPixelSize();
                if (!Equals(icon.Tag as string, identity))
                {
                    icon.Tag = identity;
                    icon.Source = null;
                    _ = LoadIconAsync(path, IconPixelSize(), identity, icon);
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

    private async Task LoadIconAsync(string path, int pixelSize, string identity, Image target)
    {
        if (_icons is null) return;
        try
        {
            var src = await _icons.GetAsync(path, pixelSize).ConfigureAwait(true);
            if (!Equals(target.Tag as string, identity)) return;
            target.Source = src;
        }
        catch
        {
            // 图标失败不影响搜索。
        }
    }

    /// <summary>
    /// 图标源应取的物理像素：32 DIP × 当前 DPI 缩放向上取整。
    /// 非 100% 缩放下仍取 32px 源会被拉伸发虚——这是与系统资源管理器观感的主要差距。
    /// </summary>
    private int IconPixelSize()
    {
        var scale = 1.0;
        if (PresentationSource.FromVisual(this) is not null)
        {
            var dpi = VisualTreeHelper.GetDpi(this);
            if (dpi.DpiScaleX > 0) scale = dpi.DpiScaleX;
        }
        return Math.Max(32, (int)Math.Ceiling(32 * scale));
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

    /// <summary>TitleBlock.Tag 的脏检查载体：记录上次装饰用的标题与高亮区间实例。</summary>
    private sealed class AppliedTitle
    {
        public required string Title { get; init; }
        public required int[] Spans { get; init; }
    }
}
