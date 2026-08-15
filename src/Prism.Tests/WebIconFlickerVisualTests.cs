using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;
using Prism.Controls;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 真实 WPF 渲染树上的闪烁验证。
///
/// 上面的键值测试只证明"身份稳定"，不证明"没有闪烁"——闪烁的直接机制是
/// ListBox 重建行容器后 Image.Source 短暂为空。这里在 STA 线程上真的把 ResultList
/// 挂进窗口、逐键推入网页结果，然后断言：
/// - 行 0 的 ListBoxItem 与 Image 是同一个实例（容器没被重建）；
/// - 每次布局完成后 Image.Source 都非空（没有空白帧）；
/// - Source 引用始终是同一个冻结图标（没有重新赋值）。
///
/// 修复前这里会失败：Title/Subtitle 逐键变化使旧的内容型身份判定删旧插新，
/// 容器与 Image 都是新实例，Source 从 null 起步。
/// </summary>
public sealed class WebIconFlickerVisualTests
{
    [Fact]
    public void Web_Row_Container_And_Icon_Survive_Keystrokes()
    {
        var failures = new List<string>();
        RunOnSta(() =>
        {
            var list = new ResultList();
            list.SetWebIconProvider(new WebIconProvider());
            var window = new Window
            {
                Width = 800,
                Height = 400,
                // 不真的显示给用户：移出屏幕、无边框，只为触发真实布局与容器生成。
                WindowStyle = WindowStyle.None,
                ShowInTaskbar = false,
                Left = -10000,
                Top = -10000,
                Content = list,
            };
            window.Show();
            Pump();

            ListBoxItem? firstContainer = null;
            Image? firstImage = null;
            ImageSource? firstSource = null;

            foreach (var terms in new[] { "w", "we", "wea", "weat", "weath" })
            {
                list.Items = WebRows(terms, suggestionCount: 3);
                Pump();

                var box = FindDescendant<ListBox>(list);
                Assert.NotNull(box);
                var container = box!.ItemContainerGenerator.ContainerFromIndex(0) as ListBoxItem;
                if (container is null)
                {
                    failures.Add($"[{terms}] 行 0 没有容器");
                    continue;
                }
                var image = FindDescendant<Image>(container, "IconImage");
                if (image is null)
                {
                    failures.Add($"[{terms}] 行 0 没有 IconImage");
                    continue;
                }

                if (image.Source is null)
                    failures.Add($"[{terms}] 布局完成后图标为空（= 一帧空白）");

                if (firstContainer is null)
                {
                    firstContainer = container;
                    firstImage = image;
                    firstSource = image.Source;
                }
                else
                {
                    if (!ReferenceEquals(firstContainer, container))
                        failures.Add($"[{terms}] 行容器被重建");
                    if (!ReferenceEquals(firstImage, image))
                        failures.Add($"[{terms}] Image 元素被重建");
                    if (!ReferenceEquals(firstSource, image.Source))
                        failures.Add($"[{terms}] Image.Source 被重新赋值");
                }

                // 文本确实在变，说明这一轮不是空转。
                var title = FindDescendant<TextBlock>(container, "TitleBlock");
                Assert.NotNull(title);
                Assert.Contains(terms, new System.Windows.Documents.TextRange(
                    title!.ContentStart, title.ContentEnd).Text, StringComparison.Ordinal);
            }

            window.Close();
        });

        Assert.Empty(failures);
    }

    /// <summary>
    /// 标题装饰脏检查：同一批 SearchResult 实例重复装饰（每次按键周期会跑 2-4 遍）
    /// 不应重建 Inlines；换了新实例（哪怕内容相同）必须重新装饰。
    /// </summary>
    [Fact]
    public void Title_Decoration_Skips_Unchanged_Rows_And_Reapplies_New_Item_Instances()
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

            list.Items = new[]
            {
                FileRow("alpha.txt", [0, 5]),
                FileRow("beta.txt", [0, 4]),
            };
            Pump();

            var box = FindDescendant<ListBox>(list);
            Assert.NotNull(box);
            var container = box!.ItemContainerGenerator.ContainerFromIndex(0) as ListBoxItem;
            Assert.NotNull(container);
            var title = FindDescendant<TextBlock>(container, "TitleBlock");
            Assert.NotNull(title);
            var runAfterFirstDecorate = title!.Inlines.FirstInline;
            Assert.NotNull(runAfterFirstDecorate);

            // 同一批实例再次触发装饰（选中变化路径）：Inlines 不应重建。
            box.SelectedIndex = 1;
            Pump();
            Assert.Same(runAfterFirstDecorate, title.Inlines.FirstInline);

            // 同键新实例：容器不重建，但内容必须重新装饰。
            list.Items = new[]
            {
                FileRow("alpha.txt", [0, 5]),
                FileRow("beta.txt", [0, 4]),
            };
            Pump();

            var sameContainer = box.ItemContainerGenerator.ContainerFromIndex(0) as ListBoxItem;
            Assert.NotNull(sameContainer);
            Assert.Same(container, sameContainer);
            var sameTitle = FindDescendant<TextBlock>(sameContainer, "TitleBlock");
            Assert.NotNull(sameTitle);
            Assert.NotSame(runAfterFirstDecorate, sameTitle!.Inlines.FirstInline);
            Assert.Equal("alpha.txt", new System.Windows.Documents.TextRange(
                sameTitle.ContentStart, sameTitle.ContentEnd).Text);

            window.Close();
        });
    }

    private static SearchResult FileRow(string name, int[] spans) =>
        new("file", name, @"C:\" + name, @"C:\" + name, spans)
        {
            Target = new ActionTarget("file", @"C:\" + name),
        };

    /// <summary>与 SearchViewModel.RunWebSearchAsync/BuildWebRows 一致的行形状。</summary>
    private static List<SearchResult> WebRows(string terms, int suggestionCount)
    {
        var url = "https://cn.bing.com/search?q=" + terms;
        var rows = new List<SearchResult>
        {
            new("web", $"在 Bing 中搜索：{terms}", url, url, [])
            {
                Target = new ActionTarget("web", url),
                RowKey = "web:direct:Bing",
            },
        };
        for (var i = 0; i < suggestionCount; i++)
        {
            var sUrl = $"{url}+s{i}";
            rows.Add(new SearchResult("web", $"{terms} 联想{i}", sUrl, sUrl, [])
            {
                Target = new ActionTarget("web", sUrl),
                RowKey = $"web:sugg:Bing:{i}",
            });
        }
        return rows;
    }

    /// <summary>把 Dispatcher 队列跑到 Loaded 优先级为止：等布局与容器生成落地。</summary>
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
