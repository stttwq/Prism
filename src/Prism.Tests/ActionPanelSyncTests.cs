using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Prism.Controls;
using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 审计批次 3 A2：动作面板打字过滤不再整表重建。
///
/// 缺陷机制与 ResultList 当年一致：过滤时整体替换 ItemsSource，ListBox 丢弃全部
/// ListBoxItem 容器再重建 → 行闪烁 + 滚动/选中丢失。修复后按 RowKey（ActionItem.Id）
/// 原地增删移，存活行的容器必须是同一个实例。
///
/// 断言"容器同一实例"而不是"没看到闪烁"：单测拿不到渲染帧，而容器重建正是
/// 闪烁的直接成因，也是回归会先破掉的地方（与 WebRowIdentityTests 同一思路）。
/// </summary>
public sealed class ActionPanelSyncTests
{
    [Fact]
    public void Filtering_Reuses_Row_Containers_For_Surviving_Actions()
    {
        var failures = new List<string>();
        RunOnSta(() =>
        {
            var panel = new ActionPanel();
            var window = NewHost(panel);
            window.Show();
            Pump();

            panel.Items = All();
            Pump();

            var box = FindDescendant<ListBox>(panel);
            Assert.NotNull(box);

            var sourceBefore = box!.ItemsSource;
            var containersBefore = Containers(box);
            if (containersBefore.Count != 5)
                failures.Add($"过滤前应有 5 个行容器，实为 {containersBefore.Count}");

            // 只记录 Reset：整表重建的唯一签名。Move/Remove/Insert 都是原地同步。
            var resets = 0;
            ((System.Collections.Specialized.INotifyCollectionChanged)sourceBefore!).CollectionChanged +=
                (_, e) =>
                {
                    if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Reset)
                        resets++;
                };

            // 打字过滤："复制" 只留下两条，copy_path 从索引 2 移到索引 1。
            panel.Items = Filtered();
            Pump();

            if (!ReferenceEquals(sourceBefore, box.ItemsSource))
                failures.Add("ItemsSource 被整体替换（ListBox 会丢弃全部行容器 = 闪烁）");
            if (resets != 0)
                failures.Add($"过滤触发了 {resets} 次 Reset 通知（等价于整表重建）");

            // 容器回收模式下同一项的容器实例可以换，但不允许**新建**容器：
            // 过滤后在场的容器必须全部来自过滤前那一批。
            var containersAfter = Containers(box);
            foreach (var container in containersAfter)
            {
                if (!containersBefore.Any(c => ReferenceEquals(c, container)))
                    failures.Add("过滤后出现了新建的行容器（存活行没有复用容器）");
            }

            if (box.Items.Count != 2)
                failures.Add($"过滤后行数应为 2，实为 {box.Items.Count}");

            // 反向守卫：被过滤掉的行必须真的没了，不能只是没更新。
            if (box.Items.Cast<ActionItem>().Any(a => a.Id == "open_folder"))
                failures.Add("被过滤掉的 open_folder 仍在列表中");

            window.Close();
        });

        Assert.Empty(failures);
    }

    /// <summary>过滤回全量后行数与顺序恢复，且新增行插在正确位置。</summary>
    [Fact]
    public void Clearing_Filter_Restores_All_Rows_In_Order()
    {
        var ids = new List<string>();
        RunOnSta(() =>
        {
            var panel = new ActionPanel();
            var window = NewHost(panel);
            window.Show();
            Pump();

            panel.Items = All();
            Pump();
            panel.Items = Filtered();
            Pump();
            panel.Items = All();
            Pump();

            var box = FindDescendant<ListBox>(panel)!;
            foreach (var item in box.Items)
                ids.Add(((ActionItem)item).Id);

            window.Close();
        });

        Assert.Equal(All().Select(a => a.Id), ids);
    }

    /// <summary>当前在场的行容器（顺序即行序）。</summary>
    private static List<ListBoxItem> Containers(ListBox box)
    {
        var containers = new List<ListBoxItem>();
        for (var i = 0; i < box.Items.Count; i++)
        {
            if (box.ItemContainerGenerator.ContainerFromIndex(i) is ListBoxItem row)
                containers.Add(row);
        }
        return containers;
    }

    /// <summary>输入"复制"后的过滤结果：copy 留在 0，copy_path 从 2 移到 1。</summary>
    private static List<ActionItem> Filtered() =>
        All().Where(a => a.Label.Contains("复制", StringComparison.Ordinal)).ToList();

    private static List<ActionItem> All() =>
    [
        new("open_folder", "打开所在文件夹", "", false, false),
        new("copy", "复制", "", false, false),
        new("copy_path", "复制路径", "", false, false),
        new("properties", "属性", "", false, false),
        new("rename", "重命名", "", false, false),
    ];

    private static Window NewHost(ActionPanel panel)
    {
        var window = new Window
        {
            Width = 660,
            SizeToContent = SizeToContent.Height,
            WindowStyle = WindowStyle.None,
            ShowInTaskbar = false,
            Left = -10000,
            Top = -10000,
            Content = panel,
            UseLayoutRounding = true,
        };
        // 挂真实主题字典（同 ResultListScrollTests）：默认模板的 padding 会改行几何。
        foreach (var src in new[] { "Themes/Tokens.Light.xaml", "Themes/Styles.xaml" })
        {
            window.Resources.MergedDictionaries.Add(new ResourceDictionary
            {
                Source = new Uri($"pack://application:,,,/Prism;component/{src}", UriKind.Absolute),
            });
        }
        return window;
    }

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
