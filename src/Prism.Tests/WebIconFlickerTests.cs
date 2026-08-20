using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 网页模式逐键图标闪烁回归测试。
///
/// 闪烁的成因是**行身份**，不是图标本身：网页行的 Title 和 Subtitle/ExecuteId 都含查询词，
/// 每按一个字母就全变。原先 ResultList 按「Kind + ExecuteId + Title」判定容器复用，
/// 于是每次按键都被判成不同的行 → 删旧插新（或替换集合项）→ ListBox 重建行容器 →
/// 新容器的 Image.Source 为空，直到下一次装饰 → 一帧空白 = 闪一下。
///
/// 这里锁住两条不变量：
/// 1. SearchResult.ContainerKey 对同一行跨按键稳定（RowKey 与查询词无关）。
/// 2. WebIconProvider.IconKey 只取决于引擎，不随 URL 里的查询词变化，
///    因此 ResultList 不会逐键重新赋值 Image.Source。
///
/// 无法在单元测试里断言"没有可见闪烁"（需要真实 WPF 渲染帧），所以断言的是
/// 那个直接导致重建的键——这正是回归会先破掉的地方。
/// </summary>
public sealed class WebRowIdentityTests
{
    private static IReadOnlyList<WebEngine> Defaults => Settings.DefaultEngines();

    [Fact]
    public async Task Direct_Web_Row_Keeps_Same_ContainerKey_Across_Keystrokes()
    {
        var state = new AppState();
        var vm = NewVm(state, out var timers);
        vm.UpdateWebSettings(Defaults, suggestionsEnabled: false);

        var keys = new List<string>();
        var titles = new List<string>();
        foreach (var text in new[] { "b w", "b we", "b wea", "b weat" })
        {
            vm.OnQueryChanged(text);
            timers.Input.Fire();
            await Eventually(() => state.Results.Count == 1
                && state.Results[0].Title.EndsWith(text[2..], StringComparison.Ordinal));
            keys.Add(state.Results[0].ContainerKey);
            titles.Add(state.Results[0].Title);
        }

        // 标题确实每次都变（否则这个测试什么都没测到）……
        Assert.Equal(4, titles.Distinct().Count());
        // ……但行身份始终是同一个，容器可原地复用。
        Assert.Single(keys.Distinct());
    }

    [Fact]
    public async Task Suggestion_Rows_Keep_Slot_Identity_When_Text_Changes()
    {
        var state = new AppState();
        var sugg = new StubSuggestionService();
        var vm = NewVm(state, out var timers, sugg);
        vm.UpdateWebSettings(Defaults, suggestionsEnabled: true);

        sugg.Next = [new SuggestionItem("weather", "https://cn.bing.com/search?q=weather")];
        vm.OnQueryChanged("b we");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 2);
        var first = state.Results.Select(r => r.ContainerKey).ToArray();

        sugg.Next = [new SuggestionItem("weather radar", "https://cn.bing.com/search?q=weather+radar")];
        vm.OnQueryChanged("b wea");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 2
            && state.Results[1].Title == "weather radar");
        var second = state.Results.Select(r => r.ContainerKey).ToArray();

        Assert.Equal(first, second);
    }

    [Fact]
    public void Web_Rows_Of_Different_Engines_Do_Not_Share_Identity()
    {
        var a = new SearchResult("web", "t", "s", "u", []) { RowKey = "web:direct:Bing" };
        var b = new SearchResult("web", "t", "s", "u", []) { RowKey = "web:direct:百度" };
        Assert.NotEqual(a.ContainerKey, b.ContainerKey);
    }

    [Fact]
    public void File_Rows_Without_RowKey_Fall_Back_To_Content_Identity()
    {
        var a = new SearchResult("file", "a.txt", @"C:\a.txt", @"C:\a.txt", []);
        var same = new SearchResult("file", "a.txt", @"C:\a.txt", @"C:\a.txt", []);
        var other = new SearchResult("file", "b.txt", @"C:\b.txt", @"C:\b.txt", []);
        Assert.Equal(a.ContainerKey, same.ContainerKey);
        Assert.NotEqual(a.ContainerKey, other.ContainerKey);
    }

    private static SearchViewModel NewVm(
        AppState state,
        out ManualTimerFactory timers,
        ISuggestionService? suggestions = null)
    {
        timers = new ManualTimerFactory();
        return new SearchViewModel(
            state,
            new UnusedSearchClient(),
            timers,
            new ImmediateScheduler(),
            activator: null,
            suggestions: suggestions);
    }

    private static async Task Eventually(Func<bool> condition)
    {
        for (var i = 0; i < 200 && !condition(); i++)
            await Task.Delay(5);
        Assert.True(condition());
    }

    private sealed class StubSuggestionService : ISuggestionService
    {
        public IReadOnlyList<SuggestionItem> Next { get; set; } = [];
        public Task<IReadOnlyList<SuggestionItem>> GetSuggestionsAsync(
            string engine, string query, CancellationToken ct) => Task.FromResult(Next);
    }

    /// <summary>网页模式不走 broker：任何调用都是被测逻辑走错了路。</summary>
    private sealed class UnusedSearchClient : ISearchClient
    {
        public bool IsConnected => true;
        public Task StartAsync(CancellationToken ct = default) => Task.CompletedTask;
        public Task<SearchResponse> SearchAsync(string query, int max, SearchContext context, CancellationToken ct = default)
            => throw new InvalidOperationException("web mode must not hit the broker");
        public Task ExecuteAsync(ActionTarget target, string? query = null, CancellationToken ct = default) => Task.CompletedTask;
        public Task RevealAsync(ActionTarget target, string? query = null, CancellationToken ct = default) => Task.CompletedTask;
        public Task<IReadOnlyList<ActionItem>> GetActionsAsync(ActionTarget target, CancellationToken ct = default)
            => Task.FromResult<IReadOnlyList<ActionItem>>([]);
        public Task RunActionAsync(ActionTarget target, string action, string? query = null, CancellationToken ct = default) => Task.CompletedTask;
        public Task RunActionAsync(ActionTarget target, string action, ActionArgs args, string? query = null, CancellationToken ct = default) => Task.CompletedTask;
        public Task<WindowHandleInfo> ResolveWindowAsync(ActionTarget target, CancellationToken ct = default)
            => throw new NotSupportedException();
        public Task RecordWindowSwitchAsync(ActionTarget target, string? query = null, CancellationToken ct = default) => Task.CompletedTask;
    }

    private sealed class ManualTimerFactory : IDebounceTimerFactory
    {
        private readonly List<ManualTimer> _timers = [];
        public ManualTimer Input => _timers[0];
        public IDebounceTimer Create(TimeSpan interval, Action callback)
        {
            var t = new ManualTimer(callback);
            _timers.Add(t);
            return t;
        }
    }

    private sealed class ManualTimer(Action callback) : IDebounceTimer
    {
        private bool _pending;
        public void Restart() => _pending = true;
        public void Stop() => _pending = false;
        public void Fire()
        {
            if (!_pending) return;
            _pending = false;
            callback();
        }
    }

    private sealed class ImmediateScheduler : ISearchScheduler
    {
        public Task Delay(TimeSpan delay, CancellationToken ct = default) => Task.CompletedTask;
    }
}

/// <summary>
/// 图标键只随引擎变化，不随 URL 里的查询词变化——ResultList 靠它避免逐键重设 Source。
/// </summary>
public sealed class WebIconKeyTests
{
    [Fact]
    public void IconKey_Is_Stable_Across_Query_Changes()
    {
        var p = new WebIconProvider();
        var a = p.IconKey("https://cn.bing.com/search?q=we");
        var b = p.IconKey("https://cn.bing.com/search?q=weather+radar");
        Assert.Equal(a, b);
    }

    [Fact]
    public void IconKey_Differs_Per_BuiltIn_Engine()
    {
        var p = new WebIconProvider();
        var keys = new[]
        {
            p.IconKey("https://cn.bing.com/search?q=x"),
            p.IconKey("https://www.baidu.com/s?wd=x"),
            p.IconKey("https://www.google.com/search?q=x"),
        };
        Assert.Equal(3, keys.Distinct().Count());
    }

    [Fact]
    public void IconKey_For_Custom_Engine_Uses_Origin()
    {
        var p = new WebIconProvider();
        var a = p.IconKey("https://duckduckgo.com/?q=we");
        var b = p.IconKey("https://duckduckgo.com/?q=weather");
        Assert.Equal(a, b);
        Assert.NotEqual(a, p.IconKey("https://example.org/?q=weather"));
    }

    [Fact]
    public void BuiltIn_Icon_Instance_Is_Reused_And_Frozen()
    {
        var p = new WebIconProvider();
        var a = p.GetIcon("https://cn.bing.com/search?q=we");
        var b = p.GetIcon("https://cn.bing.com/search?q=weather");
        // 同一冻结实例：即使真的重设 Source 也不会重新光栅化。
        Assert.Same(a, b);
        Assert.True(a.IsFrozen);
    }
}

/// <summary>
/// AUDIT-2026-08-18 C-D6: favicon 负缓存 + 后台磁盘探测。
/// 同一未命中 origin 连续装饰只允许一次磁盘探测；命中负缓存后不再探测；
/// Invalidate 清掉负缓存（下载完成后新 favicon 可见）。
/// </summary>
public sealed class WebIconNegativeCacheTests
{
    private static FaviconCache NewCache()
    {
        var dir = Path.Combine(Path.GetTempPath(), "prism-favicon-test-" + Guid.NewGuid().ToString("N"));
        return new FaviconCache(dir);
    }

    private static async Task Eventually(Func<bool> condition)
    {
        for (var i = 0; i < 200 && !condition(); i++)
            await Task.Delay(5);
        Assert.True(condition());
    }

    [Fact]
    public async Task Repeated_Decorations_Of_A_Missing_Origin_Probe_Disk_Only_Once()
    {
        var p = new WebIconProvider(NewCache(), isGranted: _ => true);
        var url = "https://example.org/search?q=a";

        // 连续装饰同一个未命中 origin。
        for (var i = 0; i < 5; i++)
            p.GetIcon(url);

        // 探测必须收敛为一次，且结果进入负缓存。
        await Eventually(() => p.IsNegativeCached("https://example.org"));
        Assert.Equal(1, p.DiskProbes);

        // 负缓存命中后继续装饰：不再有新探测，图标回通用款。
        for (var i = 0; i < 5; i++)
            p.GetIcon(url);
        await Task.Delay(50);
        Assert.Equal(1, p.DiskProbes);
    }

    [Fact]
    public async Task Invalidate_Clears_The_Negative_Cache()
    {
        var p = new WebIconProvider(NewCache(), isGranted: _ => true);
        var url = "https://example.net/search?q=a";
        p.GetIcon(url);
        await Eventually(() => p.IsNegativeCached("https://example.net"));

        p.Invalidate();
        Assert.False(p.IsNegativeCached("https://example.net"));

        // 失效后重新装饰会再探测一次（下载完成后新 favicon 因此可见）。
        p.GetIcon(url);
        await Eventually(() => p.DiskProbes >= 2);
    }

    /// <summary>Bug 1：隐藏时 ClearTransientCaches 清空无界增长的瞬态缓存，
    /// 下次装饰重新从磁盘探测 favicon。</summary>
    [Fact]
    public async Task ClearTransientCaches_DropsResolvedAndNegative()
    {
        var p = new WebIconProvider(NewCache(), isGranted: _ => true);
        var url = "https://clear.example/search?q=a";

        // 装饰一个未命中 origin：进 _negative + 一次磁盘探测。
        p.GetIcon(url);
        await Eventually(() => p.IsNegativeCached("https://clear.example"));
        Assert.Equal(1, p.DiskProbes);

        // 清理：负缓存应清空。
        p.ClearTransientCaches();
        Assert.False(p.IsNegativeCached("https://clear.example"));

        // 下次装饰：_negative 为空 → 重新探测磁盘（DiskProbes 增长）。
        p.GetIcon(url);
        await Eventually(() => p.DiskProbes >= 2);
    }
}
