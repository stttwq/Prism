using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Prism.Controls;
using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 2026-08-25 用户报告回归锚定：覆盖安装后搜索 B，之后每次按键都报
/// "搜索失败：An item with the same key has already been added. Key: …ItemInfo"，直至重启。
///
/// 根因：结果装配层某次产出了同键（ContainerKey 相同）的重复行——同步算法
/// （FindBy 只查 i 之后）把上一轮已显示的同键实例再次插入，ListBox 出现同一
/// 实例两份；WPF Selector 的选中簿记以 Object.Equals 键控 ItemInfo 字典，
/// 重复实例让 Dictionary.Add 抛 ArgumentException，选中存储从此带毒，此后
/// 每次同步的 Insert/Move/Remove/Select 都复发，直至重启。
/// 修复 = ResultList 入口按 ContainerKey（辅以 record 值相等性）去重 +
/// broker 侧最终去重；此处在真实 WPF 渲染栈上验证重复行永远不会出现在
/// Items 集合里（无论装配层将来如何回归）。
/// </summary>
public sealed class ResultListDedupeTests
{
    [Fact]
    public void Same_Key_And_Equal_Value_Duplicates_Never_Reach_The_ListBox()
    {
        RunOnSta(() =>
        {
            var list = new ResultList();
            var window = new Window
            {
                Width = 800,
                Height = 400,
                WindowStyle = WindowStyle.None,
                ShowInTaskbar = false,
                Left = -10000,
                Top = -10000,
                Content = list,
            };
            window.Show();
            Pump();

            // 同键不同实例：kind+exec+title 相同、spans 值不同——模拟字面/拼音
            // 双通道对同一路径出行的装配回归。同步算法（FindBy 只查 i 之后）会把
            // 后到的同键行原样插入，若上一轮显示集合里已有该键，则**同一实例**
            // 被插两次——WPF Selector 的 ItemInfo 字典随即重复键炸裂，正是用户
            // 报告的触发形态。
            var dupA = Row("folder", "B", @"C:\B");
            var dupB = Row("folder", "B", @"C:\B", spans: [4, 1]);
            var distinct = Row("file", "note.txt", @"C:\note.txt");

            list.Items = new List<SearchResult> { dupA, distinct, dupB, dupA };
            Pump();

            var box = FindDescendant<ListBox>(list);
            Assert.NotNull(box);
            var rows = box!.Items.Cast<SearchResult>().ToList();

            // 同键行只留首次出现（第三、四项都被丢弃，同一实例绝不出现两次）。
            var keys = new HashSet<string>(rows.Select(r => r.ContainerKey));
            Assert.Equal(rows.Count, keys.Count);
            // 值唯一（record 值相等性含全部字段；等值记录必然同键，键去重已兜住，
            // 此断言防止将来 ContainerKey 与相等性脱钩后出现回归）。
            var values = new HashSet<SearchResult>(rows);
            Assert.Equal(rows.Count, values.Count);
            Assert.Equal(2, rows.Count);

            // 再来一轮不同查询：上一轮的同键残留（中毒行形态）同样被挡住。
            list.Items = new List<SearchResult> { dupB, distinct, dupA };
            Pump();
            var rows2 = box.Items.Cast<SearchResult>().ToList();
            Assert.Equal(2, rows2.Count);
            Assert.Equal(new HashSet<SearchResult>(rows2).Count, rows2.Count);

            window.Close();
        });
    }

    private static SearchResult Row(
        string kind, string title, string exec, string? rowKey = null, int[]? spans = null)
    {
        var row = new SearchResult(kind, title, exec, exec, spans ?? []);
        return rowKey is null ? row : row with { RowKey = rowKey };
    }


    /// <summary>把 Dispatcher 队列跑到 ContextIdle：等布局与容器生成落地。</summary>
    private static void Pump()
    {
        for (var i = 0; i < 4; i++)
        {
            var frame = new DispatcherFrame();
            Dispatcher.CurrentDispatcher.BeginInvoke(
                DispatcherPriority.ContextIdle,
                new Action(() => frame.Continue = false));
            Dispatcher.PushFrame(frame);
        }
    }

    /// <summary>WPF 控件必须在 STA 线程上构造；xunit 的工作线程是 MTA。</summary>
    private static void RunOnSta(Action action)
    {
        Exception? error = null;
        var thread = new Thread(() =>
        {
            try { action(); }
            catch (Exception ex) { error = ex; }
            finally { Dispatcher.CurrentDispatcher.InvokeShutdown(); }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.IsBackground = true;
        thread.Start();
        Assert.True(thread.Join(TimeSpan.FromSeconds(30)), "STA 线程超时");
        if (error is not null)
            throw new Xunit.Sdk.XunitException("STA 线程内失败：" + error);
    }

    private static T? FindDescendant<T>(DependencyObject root, string? name = null)
        where T : FrameworkElement
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
