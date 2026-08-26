using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using System.Windows.Threading;
using Prism.Controls;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using SettingsWindow = Prism.Windows.SettingsWindow;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 设置窗三处渲染缺陷的可视化回归（问题 1 / 2 / 6）。
/// 托盘与 DWM 非客户区（问题 3 / 4 / 5）无法单测，走手工清单。
///
/// STA 线程与布局泵写法照抄 WebIconFlickerVisualTests（RunOnSta / FindDescendant / Pump）。
/// 主题字典挂在窗口资源上（与 ResultListScrollTests 同一手法），避免依赖 Application.Current。
/// </summary>
public sealed class SettingsWindowVisualTests
{
    /// <summary>问题1：别名分组头只读属性在 OneWay 绑定下折叠/展开不触发异常风暴。</summary>
    [Fact]
    public void Alias_Group_Header_Collapse_Does_Not_Throw_XamlParse()
    {
        var exceptions = new List<Exception>();
        RunOnSta(() =>
        {
            var vm = NewVm(new[]
            {
                Alias("app1.exe", "application", "a"),
                Alias("dir1", "directory", "d"),
                Alias("file1.txt", "file", "f"),
            });
            var win = NewWindow(vm);

            // 收集 Dispatcher 线程上的异常：修复前每帧重试 XamlParseException。
            Dispatcher.CurrentDispatcher.UnhandledException += (_, e) =>
            {
                exceptions.Add(e.Exception);
                e.Handled = true;
            };

            win.Show();
            Pump();

            vm.SelectTabCommand.Execute(1); // QuickAccess（TabIndex 为私有嵌套常量，用字面量）
            Pump();
            _ = vm.LoadAliasesAsync();
            Pump();

            // 展开「文件别名」分组节，再折叠分组头——后者会实体化 GroupItem 的 Expander，
            // 正是触发 Run.Text TwoWay 绑定异常的路径。
            var aliasExpander = FindExpanderByHeader(win, "文件别名");
            Assert.True(aliasExpander is not null,
                "未找到「文件别名」Expander，现有：" + string.Join("|", AllExpanders(win).Select(HeaderText)));
            aliasExpander!.IsExpanded = true;
            Pump();

            var groupExpander = FindDescendant<Expander>(aliasExpander);
            Assert.NotNull(groupExpander);
            groupExpander!.IsExpanded = true;
            Pump();
            groupExpander.IsExpanded = false;
            Pump();
            groupExpander.IsExpanded = true;
            Pump();
            groupExpander.IsExpanded = false;
            Pump();

            win.Close();
        });

        Assert.Empty(exceptions);
    }

    /// <summary>问题2：动作选择框选中项真的渲染，且与右侧录键框同高（≥32px）。</summary>
    [Fact]
    public void Action_ComboBox_Renders_Selected_Item_And_Aligns_Height()
    {
        RunOnSta(() =>
        {
            var vm = NewVm(Array.Empty<AliasEntry>());
            var win = NewWindow(vm);
            win.Show();
            Pump();

            vm.SelectTabCommand.Execute(1); // QuickAccess
            Pump();
            vm.AddActionHotkeyCommand.Execute(null);
            Pump();

            var comboBox = FindDescendant<ComboBox>(win);
            Assert.NotNull(comboBox);
            var recorder = FindDescendant<HotkeyRecorderBox>(win);
            Assert.NotNull(recorder);

            Assert.True(comboBox!.ActualHeight >= 32,
                $"ComboBox 高度 {comboBox.ActualHeight} 应 ≥ 32（与 HotkeyRecorderBox 同高）");
            Assert.True(Math.Abs(comboBox.ActualHeight - recorder!.ActualHeight) <= 2,
                $"两个框高度差 {Math.Abs(comboBox.ActualHeight - recorder.ActualHeight):F1}px 应 ≤ 2");

            // 选中项渲染验证：选择框里至少有一个文本非空的 TextBlock（动作名）。
            var label = FindDescendant<TextBlock>(comboBox);
            Assert.NotNull(label);
            Assert.False(string.IsNullOrWhiteSpace(label!.Text),
                "选择框应显示选中的动作名，而非空白");

            win.Close();
        });
    }

    /// <summary>问题6：折叠分节标题在深色下继承 Expander.Foreground（TextTitle），非系统黑。</summary>
    [Fact]
    public void Expander_Header_Text_Follows_Token_Foreground_In_Dark()
    {
        RunOnSta(() =>
        {
            var vm = NewVm(Array.Empty<AliasEntry>());
            var win = NewWindow(vm);
            win.Show();
            Pump();

            vm.SelectTabCommand.Execute(1); // QuickAccess
            Pump();

            var staging = FindExpanderByHeader(win, "暂存区");
            Assert.True(staging is not null,
                "未找到「暂存区」Expander，现有：" + string.Join("|", AllExpanders(win).Select(HeaderText)));
            staging!.IsExpanded = true;
            Pump();

            // Expander 模板：HeaderSite (ToggleButton) 含 Arrow TextBlock（Foreground=TextSubtitle）
            // 与 Header ContentPresenter（渲染字符串 header → 一个 TextBlock）。要找的是后者：
            // 文本恰为 header 字符串「暂存区」，不是箭头字形。
            TextBlock? header = null;
            FindAllDescendants(staging).OfType<TextBlock>()
                .Where(t => t.Text == "暂存区").ToList().ForEach(t => header ??= t);
            Assert.True(header is not null, "未在 header 中找到文本「暂存区」的 TextBlock");

            // 深色 TextTitle = #E8EAED；系统默认 ControlTextBrush 是黑色。断言前景色是令牌值。
            // 令牌字典挂在应用资源（EnsureAppResources），不是窗口资源。
            var expected = (SolidColorBrush)Application.Current.Resources["TextTitle"];
            Assert.NotNull(expected);
            var actual = header!.Foreground as SolidColorBrush;
            Assert.NotNull(actual);
            Assert.Equal(expected!.Color, actual!.Color);

            win.Close();
        });
    }

    // ── 辅助 ───────────────────────────────────────────────────────────

    private static SettingsViewModel NewVm(IReadOnlyList<AliasEntry> aliases)
    {
        var dir = Path.Combine(Path.GetTempPath(), "prism-settings-visual-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        var store = new SettingsStore(dir);
        store.Save(Settings.Default);
        return new SettingsViewModel(store, new AutoStartService(),
            onAliasList: () => Task.FromResult<IReadOnlyList<AliasEntry>>(aliases));
    }

    private static SettingsWindow NewWindow(SettingsViewModel vm)
    {
        // BAML 加载（InitializeComponent）即解析 Style="{StaticResource PrismScrollViewer}"，
        // 该样式在 Styles.xaml、当前由 App.xaml 合并到应用资源。测试进程无 Application，
        // 故先确保应用资源就绪（Styles + Dark 令牌），否则 BAML 解析阶段抛 StaticResource 失败。
        EnsureAppResources();
        var win = new SettingsWindow(vm)
        {
            WindowStyle = WindowStyle.None,
            ShowInTaskbar = false,
            Left = -10000,
            Top = -10000,
            Width = 700,
            Height = 600,
        };
        return win;
    }

    private static AliasEntry Alias(string name, string kind, string word)
    {
        var path = kind == "file" ? $@"C:\{name}" : $@"C:\{name}";
        return new AliasEntry(new ActionTarget(kind, path), new[] { word }, 0);
    }

    private static readonly object _appInitLock = new();

    /// <summary>
    /// 测试进程无 WPF Application 单例；SettingsWindow 的 BAML 在 InitializeComponent 期
    /// 间即解析 Style="{StaticResource PrismScrollViewer}" 等应用级资源。此处懒创建一个
    /// Application 并把 Styles + Dark 令牌合并进应用资源，使 BAML 能找到这些键。
    /// 关键约束：Application 绑在创建它的 Dispatcher 上，而每个测试的 RunOnSta 在结束时
    /// 会 InvokeShutdown 关掉自己线程的 Dispatcher。若在测试线程上建 Application，下一个
    /// 测试线程拿到的 Application 已死、窗口布局起不来。故 Application 必须建在一根常驻
    /// STA 线程上且永不关闭——资源解析只读 Application.Current，跨线程安全。
    /// </summary>
    private static void EnsureAppResources()
    {
        lock (_appInitLock)
        {
            if (Application.Current is not null && _appReady) return;
            if (Application.Current is not null)
            {
                MergeResources(Application.Current);
                _appReady = true;
                return;
            }
            var init = new ManualResetEventSlim(false);
            var thread = new Thread(() =>
            {
                _ = new Application();
                MergeResources(Application.Current!);
                _appReady = true;
                init.Set();
                // 推空帧保持 Dispatcher 不退出；线程随进程退出而回收。
                Dispatcher.Run();
            });
            thread.SetApartmentState(ApartmentState.STA);
            thread.IsBackground = true;
            thread.Start();
            init.Wait();
        }
    }

    private static bool _appReady;

    private static void MergeResources(Application app)
    {
        foreach (var src in new[] { "Themes/Tokens.Dark.xaml", "Themes/Styles.xaml" })
        {
            app.Resources.MergedDictionaries.Add(new ResourceDictionary
            {
                Source = new Uri($"pack://application:,,,/Prism;component/{src}", UriKind.Absolute),
            });
        }
    }

    /// <summary>枚举窗口可视化树里所有 Expander（调试用）。</summary>
    private static List<Expander> AllExpanders(DependencyObject root)
    {
        var list = new List<Expander>();
        void Walk(DependencyObject d)
        {
            if (d is Expander e) list.Add(e);
            var c = VisualTreeHelper.GetChildrenCount(d);
            for (var i = 0; i < c; i++) Walk(VisualTreeHelper.GetChild(d, i));
        }
        Walk(root);
        return list;
    }

    /// <summary>深度枚举所有可视化后代。</summary>
    private static IEnumerable<DependencyObject> FindAllDescendants(DependencyObject root)
    {
        var c = VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < c; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            yield return child;
            foreach (var nested in FindAllDescendants(child)) yield return nested;
        }
    }

    /// <summary>按 Header 文本找首个匹配的 Expander。</summary>
    private static Expander? FindExpanderByHeader(DependencyObject root, string header)
    {
        if (root is Expander ex && HeaderText(ex) == header) return ex;
        var count = VisualTreeHelper.GetChildrenCount(root);
        for (var i = 0; i < count; i++)
        {
            var found = FindExpanderByHeader(VisualTreeHelper.GetChild(root, i), header);
            if (found is not null) return found;
        }
        return null;
    }

    private static string HeaderText(Expander ex)
    {
        if (ex.Header is string s) return s;
        if (ex.Header is TextBlock tb) return tb.Text;
        return ex.Header?.ToString() ?? "";
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

    /// <summary>WPF 控件必须在 STA 线程上构造；xunit 工作线程是 MTA。</summary>
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
