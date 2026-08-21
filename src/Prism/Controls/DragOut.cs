using System.Windows;

namespace Prism.Controls;

/// <summary>
/// OLE 文件拖出共用件（2026-08-22 拖拽计划）：快照路径 → FileDropList →
/// 纯 Copy 语义 DoDragDrop。DoDragDrop 是阻塞式嵌套消息循环——期间 debounce
/// 定时器、generation 变更、pipe 响应照常派发，Items 可能整个被换掉，所以
/// 路径必须在进入前取好。目标程序异常时 OLE 抛 COMException，吞掉不外冒；
/// started/finished 回调必须配对（finally），窗口侧用它们置/清失焦闸守卫。
/// </summary>
internal static class FileDragOut
{
    /// <summary>paths 由调用方保证存在且非空（拖前快照与存在性过滤都是调用方职责）。</summary>
    public static void Start(
        FrameworkElement source,
        IReadOnlyList<string> paths,
        Action? started,
        Action? finished)
    {
        var files = new System.Collections.Specialized.StringCollection();
        foreach (var p in paths)
            files.Add(p);
        var data = new System.Windows.DataObject();
        data.SetFileDropList(files);
        // 只声明 Copy：搜索结果/暂存区不是文件所有者，不允许目标程序搬走原文件。
        started?.Invoke();
        try
        {
            System.Windows.DragDrop.DoDragDrop(source, data, System.Windows.DragDropEffects.Copy);
        }
        catch (System.Runtime.InteropServices.COMException)
        {
            // 目标程序行为异常属预期内，不上报到应用级未处理异常。
        }
        finally
        {
            finished?.Invoke();
        }
    }
}

/// <summary>
/// "点击 vs 拖动"阈值状态机：PreviewMouseLeftButtonDown 记录起点，位移超过
/// 系统最小拖拽距离才算拖。鼠标抬起/起拖后必须 <see cref="End"/> 作废。
/// </summary>
internal struct DragOutStart
{
    private bool _active;
    private System.Windows.Point _point;

    public bool IsActive => _active;

    public void Begin(System.Windows.Point point)
    {
        _point = point;
        _active = true;
    }

    public void End() => _active = false;

    /// <summary>起点有效且位移已超过系统最小拖拽距离。</summary>
    public readonly bool Exceeded(System.Windows.Point current) =>
        _active
        && (Math.Abs(current.X - _point.X) >= SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(current.Y - _point.Y) >= SystemParameters.MinimumVerticalDragDistance);
}
