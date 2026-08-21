using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Shapes;
using Prism.Models;

namespace Prism.Controls;

/// <summary>
/// 暂存区条带（2026-08-22 暂存区计划）：搜到文件扔进来（快捷键），需要时
/// 拖出去用。chips 由 code-behind 在 Changed 时整建——条目 ≤32，非热路径，
/// 无需 ResultList 那套容器复用机制。拖出不移除引用：暂存区是永久拿取口。
/// 阶段三：带标 chip 显角标；「存」= 存为工作集；「集」= 展开工作集行
/// （点击载入 / × 删除 / 整行拖拽整组拖出）。
/// </summary>
public partial class StagingStrip : UserControl
{
    private StagingArea? _staging;
    private bool _worksetsExpanded;

    /// <summary>OLE 拖出开始/结束：窗口侧失焦闸守卫（与 ResultList 同纪律）。</summary>
    public event Action? DragOutStarted;
    public event Action? DragOutFinished;

    /// <summary>点击 chip 打开文件。打开与错误提示归搜索窗（拥有 StatusMessage）。</summary>
    public event Action<string>? OpenRequested;

    /// <summary>「存」被点击：搜索窗负责起名/备注对话框与同名覆盖确认。</summary>
    public event Action? SaveWorksetRequested;

    public StagingStrip()
    {
        InitializeComponent();
        ClearButton.Click += (_, _) => _staging?.ClearUnmarked();
        SaveButton.Click += (_, _) => SaveWorksetRequested?.Invoke();
        WorksetsButton.Click += (_, _) =>
        {
            _worksetsExpanded = !_worksetsExpanded;
            Rebuild();
        };
    }

    public void Attach(StagingArea staging)
    {
        if (ReferenceEquals(_staging, staging)) return;
        if (_staging is not null) _staging.Changed -= Rebuild;
        _staging = staging;
        staging.Changed += Rebuild;
        Rebuild();
    }

    private void Rebuild()
    {
        if (_staging is null) return;
        ChipPanel.Children.Clear();
        Root.Visibility = _staging.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        CountText.Text = _staging.Count.ToString();
        CountText.ToolTip = "暂存区文件数（带点 = 属于某个工作集）";
        foreach (var item in _staging.Items)
            ChipPanel.Children.Add(BuildChip(item));

        // 工作集行：无工作集或暂存区空时不展示（入口随条带收起；召回仍可打名字）。
        var showWorksets = _worksetsExpanded && _staging.Worksets.Count > 0 && _staging.Count > 0;
        WorksetPanel.Visibility = showWorksets ? Visibility.Visible : Visibility.Collapsed;
        WorksetChipPanel.Children.Clear();
        if (showWorksets)
            foreach (var ws in _staging.Worksets)
                WorksetChipPanel.Children.Add(BuildWorksetChip(ws));
    }

    private Border BuildChip(StagingItem item)
    {
        var displayName = System.IO.Path.GetFileName(item.Path);
        if (string.IsNullOrEmpty(displayName)) displayName = item.Path;

        var panel = new StackPanel { Orientation = System.Windows.Controls.Orientation.Horizontal };
        if (item.Workset is not null)
        {
            // 角标是真相：带点 = 属于工作集（LRU 挤不掉），无点 = 临时文件。
            var dot = new Ellipse { Width = 5, Height = 5, Margin = new Thickness(0, 0, 5, 0) };
            dot.SetResourceReference(Shape.FillProperty, "TextMatch");
            dot.VerticalAlignment = VerticalAlignment.Center;
            panel.Children.Add(dot);
        }
        var text = new TextBlock
        {
            Text = displayName,
            FontSize = 12,
            FontFamily = (System.Windows.Media.FontFamily)Application.Current.FindResource("AppFontFamily"),
            MaxWidth = 110,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
        };
        text.SetResourceReference(TextBlock.ForegroundProperty, "TextQuery");
        panel.Children.Add(text);
        var close = new TextBlock
        {
            Text = "\u2715",
            FontSize = 10,
            Margin = new Thickness(6, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Cursor = System.Windows.Input.Cursors.Hand,
            ToolTip = "移除（属于工作集时同步从工作集移除）",
        };
        close.SetResourceReference(TextBlock.ForegroundProperty, "TextSubtitle");
        close.MouseLeftButtonUp += (_, e) =>
        {
            e.Handled = true; // 不触发 chip 的"点击打开"
            _dragStart.End();
            _staging?.Remove(item);
        };
        panel.Children.Add(close);

        var chip = new Border
        {
            Child = panel,
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(8, 4, 8, 4),
            Margin = new Thickness(0, 0, 6, 0),
            ToolTip = item.Path,
            Cursor = System.Windows.Input.Cursors.Hand,
        };
        chip.SetResourceReference(Border.BackgroundProperty, "BgItemSelected");
        chip.SetResourceReference(Border.BorderBrushProperty, "Divider");
        chip.BorderThickness = new Thickness(1);

        chip.PreviewMouseLeftButtonDown += (_, e) =>
        {
            _dragStart.Begin(e.GetPosition(chip));
            _openOnRelease = true;
            // 不置 Handled：不影响后续事件。
        };
        chip.MouseLeftButtonUp += (_, e) =>
        {
            _dragStart.End();
            // 本次按下期间起过拖（拖回自身取消也算）：松手不再当"点击打开"。
            if (!_openOnRelease) return;
            _openOnRelease = false;
            OpenRequested?.Invoke(item.Path);
        };
        chip.MouseMove += (_, e) =>
        {
            if (e.LeftButton != MouseButtonState.Pressed) return;
            if (!_dragStart.Exceeded(e.GetPosition(chip))) return;
            _dragStart.End();
            _openOnRelease = false;
            // 快照路径 + 存在性过滤（原文件被删是路径引用的固有限制：不拖）。
            if (System.IO.File.Exists(item.Path) || System.IO.Directory.Exists(item.Path))
                FileDragOut.Start(chip, [item.Path], DragOutStarted, DragOutFinished);
        };
        return chip;
    }

    /// <summary>工作集 chip：点击=载入（保留规则见 StagingPolicy.LoadWorkset），
    /// ×=删除记录（条目改未标记），拖拽=整组 FileDropList（拖前过滤不存在路径）。</summary>
    private Border BuildWorksetChip(WorksetEntry ws)
    {
        var panel = new StackPanel { Orientation = System.Windows.Controls.Orientation.Horizontal };
        var name = new TextBlock
        {
            Text = ws.Name,
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            FontFamily = (System.Windows.Media.FontFamily)Application.Current.FindResource("AppFontFamily"),
            MaxWidth = 120,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
        };
        name.SetResourceReference(TextBlock.ForegroundProperty, "TextMatch");
        var count = new TextBlock
        {
            Text = $" {ws.Paths.Count}",
            FontSize = 11,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 6, 0),
        };
        count.SetResourceReference(TextBlock.ForegroundProperty, "TextSubtitle");
        var close = new TextBlock
        {
            Text = "\u2715",
            FontSize = 10,
            Margin = new Thickness(4, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Cursor = System.Windows.Input.Cursors.Hand,
            ToolTip = "删除工作集记录（暂存区条目改为未标记）",
        };
        close.SetResourceReference(TextBlock.ForegroundProperty, "TextSubtitle");
        close.MouseLeftButtonUp += (_, e) =>
        {
            e.Handled = true;
            _wsDragStart.End();
            _staging?.DeleteWorkset(ws.Name);
        };
        panel.Children.Add(name);
        panel.Children.Add(count);
        panel.Children.Add(close);

        // "点开看一眼"：tooltip 列备注与文件清单。
        var listing = string.Join("\n", ws.Paths.Take(8));
        if (ws.Paths.Count > 8) listing += $"\n… 共 {ws.Paths.Count} 个";
        var tip = string.IsNullOrWhiteSpace(ws.Note) ? listing : $"{ws.Note}\n{listing}";

        var chip = new Border
        {
            Child = panel,
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(8, 4, 8, 4),
            Margin = new Thickness(0, 0, 6, 0),
            ToolTip = tip,
            Cursor = System.Windows.Input.Cursors.Hand,
        };
        chip.SetResourceReference(Border.BackgroundProperty, "BgItemSelected");
        chip.SetResourceReference(Border.BorderBrushProperty, "Divider");
        chip.BorderThickness = new Thickness(1);

        chip.PreviewMouseLeftButtonDown += (_, e) =>
        {
            _wsDragStart.Begin(e.GetPosition(chip));
            _wsOpenOnRelease = true;
        };
        chip.MouseLeftButtonUp += (_, e) =>
        {
            _wsDragStart.End();
            // 与文件 chip 同纪律：本次按下起过拖（含拖回自身取消）就不再当点击。
            if (!_wsOpenOnRelease) return;
            _wsOpenOnRelease = false;
            if (_staging is not null && _staging.LoadWorkset(ws.Name) && _vmStatusFeedback is not null)
                _vmStatusFeedback($"已载入工作集「{ws.Name}」");
        };
        chip.MouseMove += (_, e) =>
        {
            if (e.LeftButton != MouseButtonState.Pressed) return;
            if (!_wsDragStart.Exceeded(e.GetPosition(chip))) return;
            _wsDragStart.End();
            _wsOpenOnRelease = false;
            var existing = ws.Paths
                .Where(p => System.IO.File.Exists(p) || System.IO.Directory.Exists(p))
                .ToList();
            if (existing.Count > 0)
                FileDragOut.Start(chip, existing, DragOutStarted, DragOutFinished);
        };
        return chip;
    }

    /// <summary>状态反馈通道（SearchWindow 注入，写 AppState.StatusMessage）。</summary>
    private Action<string>? _vmStatusFeedback;

    public void SetStatusFeedback(Action<string> feedback) => _vmStatusFeedback = feedback;

    private DragOutStart _dragStart;
    private DragOutStart _wsDragStart;
    /// <summary>本次按下尚未起过拖（松手才允许"点击打开"；起拖即置否）。</summary>
    private bool _openOnRelease;
    private bool _wsOpenOnRelease;
}
