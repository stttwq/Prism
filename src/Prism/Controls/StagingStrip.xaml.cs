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
/// </summary>
public partial class StagingStrip : UserControl
{
    private StagingArea? _staging;

    /// <summary>OLE 拖出开始/结束：窗口侧失焦闸守卫（与 ResultList 同纪律）。</summary>
    public event Action? DragOutStarted;
    public event Action? DragOutFinished;

    /// <summary>点击 chip 打开文件。打开与错误提示归搜索窗（拥有 StatusMessage）。</summary>
    public event Action<string>? OpenRequested;

    public StagingStrip()
    {
        InitializeComponent();
        ClearButton.Click += (_, _) => _staging?.ClearUnmarked();
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
        foreach (var item in _staging.Items)
            ChipPanel.Children.Add(BuildChip(item));
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
            ToolTip = "移除",
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
        chip.SetResourceReference(Border.BackgroundProperty, "BgWindow");
        chip.SetResourceReference(Border.BorderBrushProperty, "Divider");
        chip.BorderThickness = new Thickness(1);

        chip.PreviewMouseLeftButtonDown += (_, e) =>
        {
            _dragStart.Begin(e.GetPosition(chip));
            // 不置 Handled：不影响后续事件。
        };
        chip.MouseLeftButtonUp += (_, e) =>
        {
            _dragStart.End();
            OpenRequested?.Invoke(item.Path);
        };
        chip.MouseMove += (_, e) =>
        {
            if (e.LeftButton != MouseButtonState.Pressed) return;
            if (!_dragStart.Exceeded(e.GetPosition(chip))) return;
            _dragStart.End();
            // 快照路径 + 存在性过滤（原文件被删是路径引用的固有限制：不拖）。
            if (System.IO.File.Exists(item.Path) || System.IO.Directory.Exists(item.Path))
                FileDragOut.Start(chip, [item.Path], DragOutStarted, DragOutFinished);
        };
        return chip;
    }

    private DragOutStart _dragStart;
}
