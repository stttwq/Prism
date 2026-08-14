using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Prism.Controls;
using Prism.Models;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 9 行（8 条结果 + "展示更多"）恰好等于可视上限时不允许滚动。
///
/// 缺陷现象：方向键下滑选到第 9 行时第一行被顶出视口，末尾露出一整行 62px 的白方框。
/// 机制是 ListBox 按项滚动（CanContentScroll=True）+ UseLayoutRounding 把每行 62 DIP
/// 向上贴到整数设备像素，于是 9 行内容在非整数缩放下比 9*62 的视口高 1~2px，
/// ScrollIntoView 便"整滚一项"。
///
/// 断言 ExtentHeight==ViewportHeight / ScrollableHeight==0 直接钉住这个前提；
/// 只断言 VerticalOffset==0 不够——那只说明本次没滚，不说明滚不动。
/// 本测试在当前显示器的真实 DPI 下跑；100% 缩放的机器上它天然通过，
/// 1.25x/1.5x 的机器上它在修复前失败。
/// </summary>
public sealed class ResultListScrollTests
{
    [Fact]
    public void Nine_Rows_Cannot_Scroll()
    {
        var failures = new List<string>();
        RunOnSta(() =>
        {
            var list = new ResultList();
            var window = NewHost(list);
            window.Show();
            Pump();

            list.Items = Rows(resultCount: 8, withMore: true);
            Pump();

            var box = FindDescendant<ListBox>(list);
            Assert.NotNull(box);
            var sv = FindDescendant<ScrollViewer>(box!);
            Assert.NotNull(sv);

            // 逐行下滑到最后一行（"展示更多"），与用户按方向键的路径一致。
            for (var i = 0; i < 9; i++)
            {
                list.SelectedIndex = i;
                Pump();
            }

            if (sv!.ViewportHeight < 9)
                failures.Add($"视口只装得下 {sv.ViewportHeight} 项（应为 9）");
            if (sv.ScrollableHeight > 0.01)
                failures.Add($"9 行仍可滚动 {sv.ScrollableHeight:F2}（会露出白方框）");
            if (sv.VerticalOffset > 0.01)
                failures.Add($"下滑到末行后视口已滚动 offset={sv.VerticalOffset:F2}");

            // 第一行必须还在原位：白方框缺陷的可见特征就是首行上浮消失。
            if (box!.ItemContainerGenerator.ContainerFromIndex(0) is not ListBoxItem)
                failures.Add("首行容器不存在（已被滚出视口虚拟化掉）");

            window.Close();
        });

        Assert.Empty(failures);
    }

    [Fact]
    public void Ten_Rows_Still_Scroll()
    {
        // 反向守卫：真正超过可视上限时滚动不能被一起关掉。
        var scrollable = 0.0;
        RunOnSta(() =>
        {
            var list = new ResultList();
            var window = NewHost(list);
            window.Show();
            Pump();

            list.Items = Rows(resultCount: 19, withMore: true);
            Pump();

            var sv = FindDescendant<ScrollViewer>(FindDescendant<ListBox>(list)!);
            scrollable = sv!.ScrollableHeight;

            window.Close();
        });

        Assert.True(scrollable > 0, "20 行时应当可以滚动");
    }

    private static Window NewHost(ResultList list)
    {
        var window = new Window
        {
            Width = 660,
            SizeToContent = SizeToContent.Height,
            WindowStyle = WindowStyle.None,
            ShowInTaskbar = false,
            Left = -10000,
            Top = -10000,
            Content = list,
            // 必须与 SearchWindow.xaml 一致：正是 UseLayoutRounding 把每行 62 DIP
            // 向上贴到整数设备像素（1.25x 下 62→62.4），制造出那 1 项的溢出。
            // 漏掉这一项测试会在有缺陷的代码上假绿。
            UseLayoutRounding = true,
        };
        // 必须挂真实主题字典：默认 ListBoxItem 模板自带 padding，行高会变成 66，
        // 那样测的就不是产品的行几何了。挂在 Window 而不是 Application 上——
        // Application 是进程单例且绑在别的 dispatcher 线程，跨 STA 测试线程访问会炸。
        foreach (var src in new[] { "Themes/Tokens.Light.xaml", "Themes/Styles.xaml" })
        {
            window.Resources.MergedDictionaries.Add(new ResourceDictionary
            {
                Source = new Uri($"pack://application:,,,/Prism;component/{src}", UriKind.Absolute),
            });
        }
        return window;
    }

    private static List<SearchResult> Rows(int resultCount, bool withMore)
    {
        var rows = new List<SearchResult>();
        for (var i = 0; i < resultCount; i++)
        {
            var path = $@"D:\个人\系统工具\Wub{i}.exe";
            rows.Add(new SearchResult("file", $"Wub{i}.exe", path, path, [0, 3])
            {
                Target = new ActionTarget("file", path),
                RowKey = $"file:{i}",
            });
        }
        if (withMore)
            rows.Add(SearchResult.More("wub"));
        return rows;
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
