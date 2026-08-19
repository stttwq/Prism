using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using System.Text.Json;
using Xunit;

namespace Prism.Tests;

public sealed class SearchViewModelTests
{
    [Fact]
    public async Task TruncatedResultsShowMoreAndExpansionRequestsOneThousand()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("a", truncated: true, generation: 1, Result("alpha")));
        client.Enqueue(Response("a", truncated: false, generation: 1, Result("alpha")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 2);
        Assert.Equal(PanelMode.Results, state.Mode);
        Assert.Equal(SearchResultKind.More, state.Results[^1].ResultKind);

        state.SelectedIndex = state.Results.Count - 1;
        await vm.ExecuteSelectedAsync();
        Assert.Equal(new[] { 8, 1000 }, client.SearchMaxima);
        Assert.DoesNotContain(state.Results, item => item.ResultKind == SearchResultKind.More);
    }

    [Fact]
    public async Task CompletePrefixGrowthUsesCacheButTruncatedAndDeletionDoNot()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("al", false, 7, Result("alpha"), Result("alpine")));
        client.Enqueue(Response("a", true, 7, Result("alpha")));
        client.Enqueue(Response("ax", true, 7, Result("axle")));
        client.Enqueue(Response("axl", false, 7, Result("axle")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("al");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        vm.OnQueryChanged("alp");
        timers.Input.Fire();
        await Eventually(() => state.Query == "alp" && state.Results.Count == 2);
        Assert.Equal(1, client.SearchCount);

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2);
        vm.OnQueryChanged("ax");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 3);
        vm.OnQueryChanged("axl");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 4);
    }

    [Fact]
    public async Task PinyinEnabledResponseDoesNotSeedLiteralPrefixCache()
    {
        var client = new FakeSearchClient();
        client.Enqueue(PinyinResponse("w", 7));
        client.Enqueue(PinyinResponse("wx", 7, Result("微信")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("w");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        vm.OnQueryChanged("wx");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2 && state.Results.Count == 1);

        Assert.Equal("微信", state.Results[0].Title);
    }

    [Fact]
    public async Task WebModeSetsIsWebModeAndClearsOnEmptyQuery()
    {
        // 小问题 Q2：网页模式进出必须同步 IsWebMode，窗口据此隐藏"当前目录"前缀。
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("g 天气");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        Assert.True(state.IsWebMode);
        Assert.StartsWith("在 Google 中搜索", state.Results[0].Title);

        vm.OnQueryChanged("");
        Assert.False(state.IsWebMode);
    }

    [Fact]
    public async Task WebModeUrlLikeTermsOpenDirectly()
    {
        // 小问题 Q3：网址类查询词直接打开，不套引擎搜索模板。
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("g baidu.com");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);

        Assert.True(state.IsWebMode);
        Assert.Equal("打开 baidu.com", state.Results[0].Title);
        Assert.Equal("https://baidu.com", state.Results[0].Subtitle);
        Assert.Equal("https://baidu.com", state.Results[0].ExecuteId);
        Assert.Equal(0, client.SearchCount); // 网页模式不走本地搜索
    }

    /// <summary>F7（FRESH-AUDIT-2）：动作超时 ≠ 未执行——mutation 类超时文案必须
    /// 报"结果未知"引导核实，普通失败仍走"动作失败"。</summary>
    [Fact]
    public async Task MutationActionTimeoutReportsUnknownOutcome()
    {
        var client = new FakeSearchClient
        {
            RunActionException = new IOException("后端响应超时（300 秒）"),
        };
        var state = new AppState();
        var vm = new SearchViewModel(state, client, new ManualTimerFactory(), new ImmediateScheduler());

        var target = new SearchResult("file", "x.txt", @"C:\x.txt", @"C:\x.txt", []);
        await vm.RunActionOnAsync(target, new ActionItem("recycle", "删除", "", false, false));
        Assert.Contains("结果未知", state.StatusMessage);
        Assert.DoesNotContain("动作失败", state.StatusMessage);

        // 非超时异常仍是普通失败文案。
        client.RunActionException = new IOException("管道断开");
        await vm.RunActionOnAsync(target, new ActionItem("recycle", "删除", "", false, false));
        Assert.Contains("动作失败", state.StatusMessage);
    }

    [Fact]
    public async Task GenerationInvalidatesCacheAndLateResponseCannotOverwriteNewQuery()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("seed", false, 1, Result("seed")));
        client.Enqueue(Response("seed", false, 2, Result("seed")));
        var old = new TaskCompletionSource<SearchResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        var current = new TaskCompletionSource<SearchResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        client.Enqueue(old.Task);
        client.Enqueue(current.Task);
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("seed");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        vm.OnIndexGenerationChanged();
        timers.Generation.Fire();
        await Eventually(() => client.SearchCount == 2);

        vm.OnQueryChanged("old");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 3);
        vm.OnQueryChanged("new");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 4);
        current.SetResult(Response("new", false, 2, Result("new-result")));
        await Eventually(() => state.Results.FirstOrDefault()?.Title == "new-result");
        old.SetResult(Response("old", false, 2, Result("old-result")));
        await Task.Delay(20);
        Assert.Equal("new-result", state.Results[0].Title);
    }

    [Fact]
    public async Task ModeRootFiltersAndSortChangesRequeryBackend()
    {
        var client = new FakeSearchClient();
        for (ulong generation = 1; generation <= 5; generation++)
            client.Enqueue(Response("x", false, generation, Result("x")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        vm.OnQueryChanged("x");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        var contexts = new[]
        {
            new SearchContext("files", null, [], 1),
            new SearchContext("files", "C:\\", [], 1),
            new SearchContext("files", "C:\\", [new("ext", "txt")], 1),
            new SearchContext("files", "C:\\", [new("ext", "txt")], 2),
        };
        foreach (var context in contexts)
        {
            var expected = client.SearchCount + 1;
            vm.SetSearchContext(context);
            timers.Generation.Fire();
            await Eventually(() => client.SearchCount == expected);
        }
        Assert.Equal(5, client.SearchCount);
    }

    [Fact]
    public async Task ActionsIdlePinAndUnknownKindHaveStableStateTransitions()
    {
        var client = new FakeSearchClient
        {
            Actions = [new ActionItem("copy_path", "Copy path", "", false, false)],
        };
        var timers = new ManualTimerFactory();
        var state = new AppState { IsPinned = true };
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        state.Mode = PanelMode.Results;
        state.Query = "x";
        state.Results = [new SearchResult("file", "x", "C:\\x", "C:\\x", [])];
        state.SelectedIndex = 0;

        await vm.EnterActionsAsync();
        Assert.Equal(PanelMode.Actions, state.Mode);
        vm.LeaveActions();
        Assert.Equal(PanelMode.Results, state.Mode);
        vm.ResetForShow();
        Assert.Equal(PanelMode.Idle, state.Mode);
        Assert.True(state.IsPinned);

        var unknown = new SearchResult("future_kind", "x", "", "", []);
        Assert.Equal(SearchResultKind.Unknown, unknown.ResultKind);
        Assert.Equal("future_kind", unknown.Kind);
        Assert.Equal(2, timers.CreatedCount);
    }

    [Fact]
    public void FocusLossOnlyHidesWithoutAnActiveProtectionState()
    {
        Assert.True(SearchWindowFocusPolicy.ShouldHide(false, false, false, false, false));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(true, false, false, false, false));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, true, false, false, false));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, false, true, false, false));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, false, false, true, false));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, false, false, false, true));
    }

    [Fact]
    public void ProtocolReaderAcceptsOldOptionalShapeAndUnknownKind()
    {
        using var oldDocument = JsonDocument.Parse(
            """{"type":"results","query":"x","items":[],"is_indexing":false}""");
        var oldResponse = PipeClient.ParseSearchResponse(oldDocument.RootElement, "fallback");
        Assert.False(oldResponse.IsTruncated);
        Assert.Null(oldResponse.IndexGeneration);

        using var futureDocument = JsonDocument.Parse(
            """{"kind":"future_kind","title":"x","subtitle":"","execute_id":"","match_spans":[],"future_field":1}""");
        var future = PipeClient.ParseResult(futureDocument.RootElement);
        Assert.Equal(SearchResultKind.Unknown, future.ResultKind);
        Assert.Equal("future_kind", future.Kind);

        using var typedDocument = JsonDocument.Parse(
            """{"kind":"web","title":"x","subtitle":"","execute_id":"legacy","target":{"kind":"web","value":"https://example.com"},"match_spans":[]}""");
        var typed = PipeClient.ParseResult(typedDocument.RootElement);
        Assert.Equal(new ActionTarget("web", "https://example.com"), typed.ExecutionTarget);
        Assert.Equal(new ActionTarget("file", "legacy"),
            new SearchResult("file", "x", "", "legacy", []).ExecutionTarget);

        var wire = JsonSerializer.Serialize(
            PipeClient.TargetPayload(new ActionTarget("web", "https://example.com")));
        Assert.Equal("""{"kind":"web","value":"https://example.com"}""", wire);
    }

    [Fact]
    public async Task PartialIndexResultsNeverSeedThePrefixCache()
    {
        var client = new FakeSearchClient();
        client.Enqueue(IndexingResponse(
            "a",
            new IndexProgress(10, 100, 2, 1, "D:\\"),
            Result("alpha")));
        client.Enqueue(Response("al", false, 2, Result("alpha")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new PausedScheduler());

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1 && state.IsIndexing);

        vm.OnQueryChanged("al");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2 && !state.IsIndexing);

        Assert.Equal("alpha", state.Results[0].Title);
    }

    [Fact]
    public async Task ProgressOnlyPollRefreshesTheVisibleVolumeStatus()
    {
        var client = new FakeSearchClient();
        client.Enqueue(IndexingResponse(
            "missing",
            new IndexProgress(10, 100, 2, 0, "C:\\")));
        client.Enqueue(IndexingResponse(
            "missing",
            new IndexProgress(60, 100, 2, 1, "D:\\")));
        client.Enqueue(Response("missing", false, 3));
        var timers = new ManualTimerFactory();
        var scheduler = new ManualSearchScheduler();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, scheduler);

        vm.OnQueryChanged("missing");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1 && scheduler.PendingCount == 1);
        Assert.Contains("0/2", state.StatusMessage);
        Assert.Contains("C:\\", state.StatusMessage);

        scheduler.FireNext();
        await Eventually(() => client.SearchCount == 2 && scheduler.PendingCount == 1);
        Assert.Empty(state.Results);
        Assert.Contains("1/2", state.StatusMessage);
        Assert.Contains("D:\\", state.StatusMessage);

        scheduler.FireNext();
        await Eventually(() => client.SearchCount == 3 && !state.IsIndexing);
    }

    [Fact]
    public void ProtocolReaderParsesOptionalBuildProgress()
    {
        using var document = JsonDocument.Parse(
            """{"type":"results","query":"x","items":[],"is_indexing":true,"pinyin_status":"ready","history_status":"corrupt","index_progress":{"scanned":12,"total_estimate":100,"volumes_total":3,"volumes_done":1,"current_volume":"D:\\"}}""");

        var response = PipeClient.ParseSearchResponse(document.RootElement, "fallback");

        Assert.True(response.IsIndexing);
        Assert.Equal(12UL, response.IndexProgress?.Scanned);
        Assert.Equal(100UL, response.IndexProgress?.TotalEstimate);
        Assert.Equal(3, response.IndexProgress?.VolumesTotal);
        Assert.Equal(1, response.IndexProgress?.VolumesDone);
        Assert.Equal("D:\\", response.IndexProgress?.CurrentVolume);
        Assert.Equal("ready", response.PinyinStatus);
        Assert.Equal("corrupt", response.HistoryStatus);
    }

    [Fact]
    public async Task ScopeRootChangesRequeryAndKeepOtherContextDimensions()
    {
        var client = new FakeSearchClient();
        for (ulong generation = 1; generation <= 3; generation++)
            client.Enqueue(Response("x", false, generation, Result("x")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        vm.SetSearchContext(SearchContext.Default with
        {
            Filters = [new SearchFilterOption("exclude_path", @"C:\build")],
        });
        vm.OnQueryChanged("x");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        vm.SetScopeRoot(@"C:\Users\me\Docs");
        timers.Generation.Fire();
        await Eventually(() => client.SearchCount == 2);
        Assert.Equal(@"C:\Users\me\Docs", client.LastContext?.Root);
        Assert.Equal(
            new SearchFilterOption("exclude_path", @"C:\build"),
            client.LastContext?.Filters.Single());

        // 回到全局：root 清空同样触发一次重查。
        vm.SetScopeRoot(null);
        timers.Generation.Fire();
        await Eventually(() => client.SearchCount == 3);
        Assert.Null(client.LastContext?.Root);
        Assert.Single(client.LastContext!.Filters);
    }

    /// <summary>
    /// G4 §4.4 A: empty input under a host root must request the broker recent-under-root
    /// list instead of collapsing to Idle.
    /// </summary>
    [Fact]
    public async Task EmptyQueryWithHostRootRequestsRecentUnderRoot()
    {
        var client = new FakeSearchClient();
        client.Enqueue(new SearchResponse(
            "",
            [Result("recent")],
            false,
            null,
            false,
            null));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        vm.SetScopeRoot(@"C:\Users\me\Docs");

        // Capturing a host root with an empty box schedules one empty search.
        Assert.Equal(PanelMode.Results, state.Mode);
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        Assert.Equal("", client.LastQuery);
        Assert.Equal(@"C:\Users\me\Docs", client.LastContext?.Root);
        Assert.Equal("recent", state.Results[0].Title);

        // Re-clearing the box while still scoped must search again, not collapse to Idle.
        client.Enqueue(new SearchResponse(
            "",
            [Result("again")],
            false,
            null,
            false,
            null));
        vm.OnQueryChanged("x");
        // Do not fire the non-empty debounce — jump straight back to empty.
        vm.OnQueryChanged("");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2);
        Assert.Equal("", client.LastQuery);
        Assert.Equal(PanelMode.Results, state.Mode);
        Assert.Equal("again", state.Results[0].Title);
    }

    /// <summary>
    /// G4 §4.4 B (deferred to G5): empty input without a root stays Idle and never hits
    /// the broker — avoids a meaningless full-index scan for "recent windows".
    /// </summary>
    [Fact]
    public async Task EmptyQueryWithoutRootStaysIdleAndDoesNotSearch()
    {
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("x");
        // Do not fire debounce — switch back to empty without a root.
        vm.OnQueryChanged("");
        timers.Input.Fire();
        await Task.Delay(20);

        Assert.Equal(0, client.SearchCount);
        Assert.Equal(PanelMode.Idle, state.Mode);
        Assert.Empty(state.Results);

        // Explicitly clearing a previous root with an empty box also returns to Idle
        // without issuing a second broker call for "recent windows" (G5).
        client.Enqueue(new SearchResponse("", [], false, null, false, null));
        vm.SetScopeRoot(@"C:\Users\me\Docs");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        vm.SetScopeRoot(null);
        timers.Input.Fire();
        await Task.Delay(20);
        Assert.Equal(1, client.SearchCount);
        Assert.Equal(PanelMode.Idle, state.Mode);
        Assert.Empty(state.Results);
    }

    /// <summary>
    /// Empty-query host responses must never seed the literal prefix cache: a later typed
    /// query still hits the broker even when the empty reply had a generation and no items.
    /// </summary>
    [Fact]
    public async Task EmptyHostRootResponseDoesNotSeedPrefixCache()
    {
        var client = new FakeSearchClient();
        // Empty recent-under-root reply with a generation — still must not become a cache seed.
        client.Enqueue(new SearchResponse(
            "",
            [],
            false,
            null,
            false,
            9));
        client.Enqueue(Response("doc", false, 9, Result("docs")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.SetScopeRoot(@"C:\Users\me\Docs");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        Assert.Equal(PanelMode.Results, state.Mode);
        Assert.Empty(state.Results);
        Assert.Equal("", state.StatusMessage);

        vm.OnQueryChanged("doc");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2);
        Assert.Equal("docs", state.Results[0].Title);
    }

    [Fact]
    public void SearchPayloadCarriesRootOnlyWhileTheScopeIsCurrentDirectory()
    {
        var global = JsonSerializer.Serialize(
            PipeClient.SearchPayload("x", 8, SearchContext.Default));
        // 全局搜索的线上格式必须与加入 root 之前完全一致（旧 broker 也能解析）。
        Assert.Equal("""{"type":"search","query":"x","max":8}""", global);

        var blank = JsonSerializer.Serialize(
            PipeClient.SearchPayload("x", 8, SearchContext.Default with { Root = "   " }));
        Assert.DoesNotContain("root", blank);

        var scoped = JsonSerializer.Serialize(PipeClient.SearchPayload("x", 1000, new SearchContext(
            "all",
            @"C:\Users\me\Docs",
            [new SearchFilterOption("exclude_path", @"C:\build")],
            1)));
        Assert.Equal(
            """{"type":"search","query":"x","max":1000,"filters":[{"field":"exclude_path","value":"C:\\build"}],"root":"C:\\Users\\me\\Docs"}""",
            scoped);
    }

    [Fact]
    public void ProtocolReaderMapsRootRejectionToTheStructuredEnum()
    {
        using var rejected = JsonDocument.Parse(
            """{"type":"results","query":"x","items":[],"is_indexing":false,"root_rejection":"volume_not_indexed","root_message":"root volume is not indexed"}""");
        var response = PipeClient.ParseSearchResponse(rejected.RootElement, "fallback");
        Assert.Equal(RootRejection.VolumeNotIndexed, response.RootRejection);
        Assert.Equal("root volume is not indexed", response.RootMessage);

        // 每个后端稳定字符串都要能解析回来，未知值退化为「无结构原因」而不是异常。
        foreach (var rejection in Enum.GetValues<RootRejection>())
            Assert.Equal(rejection, RootRejectionCodes.Parse(RootRejectionCodes.ToCode(rejection)));
        Assert.Null(RootRejectionCodes.Parse("future_reason"));

        using var ordinary = JsonDocument.Parse(
            """{"type":"results","query":"x","items":[],"is_indexing":false}""");
        Assert.Null(PipeClient.ParseSearchResponse(ordinary.RootElement, "fallback").RootRejection);
    }

    /// <summary>
    /// 后端拒绝 root 时：结果已是全局的，范围状态必须同步回到全局并给出提示，
    /// 之后的请求不再带 root —— 不允许出现「UI 说当前目录，实际搜全局」。
    /// </summary>
    [Fact]
    public async Task BackendRootRejectionFallsBackToGlobalAndStopsSendingTheRoot()
    {
        var window = new IntPtr(0x4321);
        var scope = new HostScopeController(
            [new StubHostAdapter(window, @"C:\Users\me\Docs")],
            new AcceptingRootValidator(),
            new AliveWindowProbe());
        scope.Capture(window);
        Assert.Equal(@"C:\Users\me\Docs", scope.Root);

        var client = new FakeSearchClient();
        client.Enqueue(new SearchResponse(
            "x",
            [Result("global-hit")],
            false,
            null,
            false,
            5,
            RootRejection: RootRejection.VolumeNotIndexed,
            RootMessage: "root volume is not indexed"));
        client.Enqueue(Response("x", false, 5, Result("global-hit")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        vm.RootRejected += rejection => scope.Invalidate(rejection);
        scope.Changed += () => vm.SetScopeRoot(scope.Root);
        vm.SetScopeRoot(scope.Root);

        vm.OnQueryChanged("x");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        Assert.Equal(@"C:\Users\me\Docs", client.LastContext?.Root);
        Assert.Equal(SearchScope.Global, scope.Scope);
        Assert.Null(scope.Root);
        Assert.Contains("不在索引中", scope.Notice);

        // 降级触发的重查必须是纯全局请求。
        timers.Generation.Fire();
        await Eventually(() => client.SearchCount == 2);
        Assert.Null(client.LastContext?.Root);
        Assert.Equal("global-hit", state.Results[0].Title);
    }

    // ── G5 窗口模式 ──────────────────────────────────────────────────────────────

    /// <summary>窗口结果：ExecuteId/target.value 是本次枚举的 token，不是路径。</summary>
    private static SearchResult WindowResult(string title, string app = "notepad", string token = "1024") =>
        new("window", title, app, token, [])
        {
            Target = new ActionTarget("window", token),
        };

    private static SearchResponse WindowResponse(string query, params SearchResult[] results) =>
        new(query, results, false, null, false, null);

    /// <summary>
    /// 窗口响应，但带上 generation：用于证明「窗口结果不进前缀缓存」靠的是模式判断，
    /// 而不是恰好缺字段。
    /// </summary>
    private static SearchResponse CacheableWindowResponse(
        string query,
        params SearchResult[] results) =>
        new(query, results, false, null, false, 1, PinyinStatus: "disabled");

    private static (SearchViewModel Vm, AppState State, FakeSearchClient Client,
        ManualTimerFactory Timers, FakeWindowActivator Activator) WindowFixture()
    {
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var activator = new FakeWindowActivator();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler(), activator);
        return (vm, state, client, timers, activator);
    }

    [Fact]
    public async Task WindowPrefixSendsWindowModeAndStripsThePrefix()
    {
        var (vm, state, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse("记事", WindowResult("无标题 - 记事本")));

        vm.OnQueryChanged(">记事");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        // The broker never sees the `>`.
        Assert.Equal("记事", client.LastQuery);
        Assert.Equal(SearchContext.WindowMode, client.LastContext?.Mode);
        Assert.True(client.LastContext?.IsWindowMode);
        await Eventually(() => state.Results.Count == 1);
        Assert.Equal(SearchResultKind.Window, state.Results[0].ResultKind);
    }

    [Fact]
    public async Task BareWindowPrefixListsRecentWindowsInsteadOfGoingIdle()
    {
        var (vm, state, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse("", WindowResult("无标题 - 记事本")));

        vm.OnQueryChanged(">");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        Assert.Equal("", client.LastQuery);
        Assert.Equal(SearchContext.WindowMode, client.LastContext?.Mode);
        Assert.Equal(PanelMode.Results, state.Mode);
    }

    [Fact]
    public async Task GlobalSearchStillSendsNoModeSoOldBrokersAreUnaffected()
    {
        var (vm, _, client, timers, _) = WindowFixture();
        client.Enqueue(Response("a", false, 1, Result("alpha")));

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        Assert.Equal(SearchContext.AllMode, client.LastContext?.Mode);
        Assert.False(client.LastContext?.IsWindowMode);
    }

    [Fact]
    public async Task PrefixOnlyCountsAtPositionZero()
    {
        var (vm, _, client, timers, _) = WindowFixture();
        client.Enqueue(Response("a>b", false, 1, Result("a>b")));

        vm.OnQueryChanged("a>b");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        // A `>` inside a filename must not switch modes.
        Assert.Equal("a>b", client.LastQuery);
        Assert.Equal(SearchContext.AllMode, client.LastContext?.Mode);
    }

    [Fact]
    public async Task LeavingWindowModeReturnsToGlobalSearch()
    {
        var (vm, _, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));
        client.Enqueue(Response("a", false, 1, Result("alpha")));

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        Assert.Equal(SearchContext.WindowMode, client.LastContext?.Mode);

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2);
        Assert.Equal(SearchContext.AllMode, client.LastContext?.Mode);
    }

    [Fact]
    public async Task WindowModeIgnoresHostRootBecauseWindowsAreNotScopedToADirectory()
    {
        var (vm, _, client, timers, _) = WindowFixture();
        vm.SetScopeRoot(@"C:\Users\me\Docs");
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        Assert.Null(client.LastContext?.Root);
    }

    /// <summary>
    /// 窗口 token 只在生成它的那次枚举内有效，所以扩展查询必须重新请求，不能本地过滤旧结果。
    ///
    /// 这里只断言可观察行为（确实又发了一次请求）。代码里有三重机制共同保证这件事，
    /// 单元测试无法把其中任何一条单独隔离出来——mutation 掉 ViewModel 里的模式判断，
    /// 本测试依然通过。真正的回归信号是「请求次数」，不是某一行代码。
    /// </summary>
    [Fact]
    public async Task WindowQueryGrowthReQueriesInsteadOfFilteringLocally()
    {
        var (vm, _, client, timers, _) = WindowFixture();
        // Generation supplied so the response satisfies the other cache preconditions and
        // the assertion is not resting on a missing field.
        client.Enqueue(CacheableWindowResponse("a", WindowResult("abc - Notepad")));
        client.Enqueue(CacheableWindowResponse("ab", WindowResult("abc - Notepad")));

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        vm.OnQueryChanged(">ab");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2);
        Assert.Equal("ab", client.LastQuery);
    }

    /// <summary>对照组：普通文件搜索的前缀增长确实会命中缓存，证明上面那条不是因为缓存整体失效。</summary>
    [Fact]
    public async Task GlobalPrefixGrowthStillUsesTheCache()
    {
        var (vm, _, client, timers, _) = WindowFixture();
        client.Enqueue(Response("al", false, 7, Result("alpha"), Result("alpine")));

        vm.OnQueryChanged("al");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        vm.OnQueryChanged("alp");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        Assert.Equal(1, client.SearchCount);
    }

    [Fact]
    public async Task SuccessfulSwitchActivatesThenHidesAndRecordsHistory()
    {
        var (vm, state, client, timers, activator) = WindowFixture();
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));
        var hidden = false;
        var activatedBeforeHide = false;
        vm.HideRequested += () =>
        {
            hidden = true;
            activatedBeforeHide = activator.Calls.Count > 0;
        };

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        state.SelectedIndex = 0;
        await vm.ExecuteSelectedAsync();

        Assert.Single(activator.Calls);
        Assert.True(hidden);
        // Hiding first would surrender foreground rights and the activation would degrade
        // to a taskbar flash, so the order is load-bearing.
        Assert.True(activatedBeforeHide);
        Assert.Single(client.RecordCalls);
        Assert.Equal("window", client.RecordCalls[0].Kind);
    }

    [Fact]
    public async Task WindowsAreSwitchedNotOpenedThroughTheShell()
    {
        var (vm, state, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        state.SelectedIndex = 0;
        await vm.ExecuteSelectedAsync();

        // Activation is a foreground-bound Win32 call, not a Shell verb; execute must not
        // be used for a window.
        Assert.Single(client.ResolveCalls);
    }

    [Fact]
    public async Task RejectedActivationKeepsTheUiAndWritesNoSuccessHistory()
    {
        var (vm, state, client, timers, activator) = WindowFixture();
        activator.Result = false;
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));
        var hidden = false;
        vm.HideRequested += () => hidden = true;

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        state.SelectedIndex = 0;
        await vm.ExecuteSelectedAsync();

        Assert.False(hidden);
        Assert.Empty(client.RecordCalls);
        Assert.Equal("无法切换到该窗口", state.StatusMessage);
        Assert.Single(state.Results);
    }

    [Fact]
    public async Task ClosedWindowReportsFailureAndWritesNoSuccessHistory()
    {
        var (vm, state, client, timers, activator) = WindowFixture();
        client.ResolveFailure = "the window no longer exists";
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));
        var hidden = false;
        vm.HideRequested += () => hidden = true;

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        state.SelectedIndex = 0;
        await vm.ExecuteSelectedAsync();

        // Resolve failed, so activation must never have been attempted.
        Assert.Empty(activator.Calls);
        Assert.False(hidden);
        Assert.Empty(client.RecordCalls);
        Assert.Contains("切换失败", state.StatusMessage);
    }

    [Fact]
    public async Task FailedHistoryWriteDoesNotTurnASuccessfulSwitchIntoAFailure()
    {
        var (vm, state, client, timers, activator) = WindowFixture();
        client.RecordFailure = "history lock is poisoned";
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));
        var hidden = false;
        vm.HideRequested += () => hidden = true;

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        state.SelectedIndex = 0;
        await vm.ExecuteSelectedAsync();

        Assert.Single(activator.Calls);
        Assert.True(hidden);
        Assert.DoesNotContain("失败", state.StatusMessage);
    }

    [Fact]
    public async Task WithoutAnActivatorWindowSwitchingReportsUnavailable()
    {
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        // No activator supplied: the feature must degrade visibly, not throw.
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        state.SelectedIndex = 0;
        await vm.ExecuteSelectedAsync();

        Assert.Equal("窗口切换不可用", state.StatusMessage);
        Assert.Empty(client.RecordCalls);
    }

    [Fact]
    public async Task EmptyWindowListExplainsItselfWithoutTheGenericNoMatchCopy()
    {
        var (vm, state, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse(""));

        vm.OnQueryChanged(">");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        Assert.Equal("没有最近使用过的窗口", state.StatusMessage);
    }

    [Fact]
    public async Task NoMatchingWindowIsDistinctFromNoRecentWindows()
    {
        var (vm, state, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse("zzz"));

        vm.OnQueryChanged(">zzz");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        Assert.Equal("没有匹配的窗口", state.StatusMessage);
    }

    [Fact]
    public async Task WindowModeNeverClaimsTheIndexIsBuilding()
    {
        var (vm, state, client, timers, _) = WindowFixture();
        client.Enqueue(WindowResponse("a", WindowResult("a - Notepad")));

        vm.OnQueryChanged(">a");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);

        // Window availability is unrelated to index readiness.
        Assert.False(state.IsIndexing);
    }

    /// <summary>
    /// AUDIT-2026-08-18 C-D10 + P4b: 两次 RefreshAsync 重叠时，先结束的那次
    /// （这里走超时路径）绝不能清掉后一次刚装上的世代信号——否则第二次 mutation
    /// 的 generation 变化丢失，后一次刷新被迫等满 3 秒超时。
    /// 超时全走 ManualSearchScheduler（永不自动到点），r2 只可能经信号路径完成：
    /// 修复前 r2 永远挂起，修复后秒级完成。
    /// </summary>
    [Fact]
    public async Task OverlappingRefreshesDoNotClobberTheNewerGenerationSignal()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("a", false, 1, Result("alpha"))); // 首搜
        client.Enqueue(Response("a", false, 1, Result("alpha"))); // r1 超时路径的补搜
        client.Enqueue(Response("a", false, 2, Result("beta")));  // r2 信号路径的补搜
        var timers = new ManualTimerFactory();
        var scheduler = new ManualSearchScheduler();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, scheduler);

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        Assert.Equal(PanelMode.Results, state.Mode);

        var r1 = vm.RefreshAsync(); // 信号1 装上，等待 generation 或超时（delay 排队 #1）
        var r2 = vm.RefreshAsync(); // 信号2 覆盖装入（delay 排队 #2）

        scheduler.FireNext(); // 只完成 r1 的超时 → r1 续体执行清字段动作（旧代码清掉信号2）
        await Eventually(() => client.SearchCount == 2); // r1 已进入补搜 ⇒ 清字段动作已发生

        vm.OnIndexGenerationChanged(); // 修复前信号已丢；修复后完成信号2

        // r2 的超时（#2）永不 FireNext——r2 完成即证明走了信号路径。
        Assert.True(
            await Task.WhenAny(r2, Task.Delay(TimeSpan.FromSeconds(5))) == r2,
            "第二次刷新必须在世代信号（而非超时）路径完成");
        await r1.WaitAsync(TimeSpan.FromSeconds(5));
    }

    /// <summary>P4b: RefreshAsync 的 3 秒超时改走 ISearchScheduler 缝——测试可用 FireNext 快进，不再真睡。</summary>
    [Fact]
    public async Task RefreshAsyncTimeoutIsDrivenByTheSchedulerSeam()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("a", false, 1, Result("alpha"))); // 首搜
        client.Enqueue(Response("a", false, 1, Result("alpha"))); // 超时路径的补搜
        var timers = new ManualTimerFactory();
        var scheduler = new ManualSearchScheduler();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, scheduler);

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        var refresh = vm.RefreshAsync();
        scheduler.FireNext(); // 快进 3 秒超时
        await refresh.WaitAsync(TimeSpan.FromSeconds(5));

        Assert.Equal("索引尚未刷新，结果可能不完整", state.StatusMessage);
    }

    // ------------------------------------------------------------------
    // P4a: IFolderPicker 注入——copy_to/move_to 分支此前零测试覆盖
    // ------------------------------------------------------------------

    private static SearchResult FileTarget(string path) =>
        new("file", System.IO.Path.GetFileName(path), path, path, []);

    private static ActionItem Action(string id) => new(id, id, "", false, false);

    private sealed class FakeFolderPicker : IFolderPicker
    {
        public Queue<string?> Results { get; } = new();
        public List<string?> Descriptions { get; } = [];
        public string? PickFolder(string? description)
        {
            Descriptions.Add(description);
            return Results.Dequeue();
        }
    }

    /// <summary>用户取消选目录：不得发 RunActionAsync、不报错、不刷新。</summary>
    [Fact]
    public async Task CopyToWithCancelledPickerDoesNothing()
    {
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var picker = new FakeFolderPicker();
        picker.Results.Enqueue(null);
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler(), folderPicker: picker);

        await vm.RunActionOnAsync(FileTarget(@"C:\src\a.txt"), Action("copy_to"));

        Assert.Null(client.LastTarget);
        Assert.Null(client.LastActionArgs);
        Assert.Equal("", state.StatusMessage);
    }

    /// <summary>选了目录：Destination 原样传给后端；generation 信号到来后刷新，不走超时文案。</summary>
    [Fact]
    public async Task CopyToSendsPickedDestinationAndRefreshes()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("a", false, 1, Result("alpha"))); // 首搜
        client.Enqueue(Response("a", false, 2, Result("alpha"))); // RefreshAsync 的补搜
        var timers = new ManualTimerFactory();
        var scheduler = new ManualSearchScheduler();
        var state = new AppState();
        var picker = new FakeFolderPicker();
        picker.Results.Enqueue(@"D:\dest");
        var vm = new SearchViewModel(state, client, timers, scheduler, folderPicker: picker);
        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        var runAction = vm.RunActionOnAsync(FileTarget(@"C:\src\a.txt"), Action("copy_to"));
        await Eventually(() => scheduler.PendingCount == 1); // RefreshAsync 已挂起等信号/超时
        vm.OnIndexGenerationChanged(); // generation 到来 → 信号路径
        await runAction.WaitAsync(TimeSpan.FromSeconds(5));

        Assert.NotNull(client.LastActionArgs);
        Assert.Equal(@"D:\dest", client.LastActionArgs!.Destination);
        Assert.DoesNotContain("索引尚未刷新", state.StatusMessage);
        Assert.True(client.SearchCount >= 2, "动作成功后必须刷新结果");
    }

    /// <summary>第二次选择不同的目录：传的是本次选择，不是上次的缓存。</summary>
    [Fact]
    public async Task MoveToUsesTheLatestPickedFolder()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("a", false, 1, Result("alpha"))); // 首搜
        client.Enqueue(Response("a", false, 2, Result("alpha"))); // 第一次刷新补搜
        client.Enqueue(Response("a", false, 3, Result("alpha"))); // 第二次刷新补搜
        var timers = new ManualTimerFactory();
        var scheduler = new ManualSearchScheduler();
        var state = new AppState();
        var picker = new FakeFolderPicker();
        picker.Results.Enqueue(@"D:\one");
        picker.Results.Enqueue(@"E:\two");
        var vm = new SearchViewModel(state, client, timers, scheduler, folderPicker: picker);
        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        var firstRun = vm.RunActionOnAsync(FileTarget(@"C:\src\a.txt"), Action("move_to"));
        // 每轮 RefreshAsync 各排一个超时任务；信号路径胜出后旧任务留在队列里（永不触发）。
        await Eventually(() => scheduler.PendingCount >= 1);
        vm.OnIndexGenerationChanged();
        await firstRun.WaitAsync(TimeSpan.FromSeconds(5));
        var first = client.LastActionArgs!.Destination;

        var secondRun = vm.RunActionOnAsync(FileTarget(@"C:\src\b.txt"), Action("move_to"));
        await Eventually(() => scheduler.PendingCount >= 2);
        vm.OnIndexGenerationChanged();
        await secondRun.WaitAsync(TimeSpan.FromSeconds(5));

        Assert.Equal(@"D:\one", first);
        Assert.Equal(@"E:\two", client.LastActionArgs!.Destination);
    }

    private static SearchResult Result(string title) =>
        new("file", title, $"C:\\{title}", $"C:\\{title}", []);

    private static SearchResponse Response(
        string query,
        bool truncated,
        ulong generation,
        params SearchResult[] results) =>
        new(query, results, false, null, truncated, generation);

    private static SearchResponse PinyinResponse(
        string query,
        ulong generation,
        params SearchResult[] results) =>
        new(query, results, false, null, false, generation, PinyinStatus: "ready");

    private static SearchResponse IndexingResponse(
        string query,
        IndexProgress progress,
        params SearchResult[] results) =>
        new(query, results, true, null, false, 1, progress);

    private static async Task Eventually(Func<bool> condition)
    {
        for (var i = 0; i < 100 && !condition(); i++)
            await Task.Delay(5);
        Assert.True(condition());
    }

    private sealed class ManualTimerFactory : IDebounceTimerFactory
    {
        private readonly List<ManualTimer> _timers = [];
        public ManualTimer Input => _timers[0];
        public ManualTimer Generation => _timers[1];
        public int CreatedCount => _timers.Count;

        public IDebounceTimer Create(TimeSpan interval, Action callback)
        {
            var timer = new ManualTimer(callback);
            _timers.Add(timer);
            return timer;
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

    private sealed class PausedScheduler : ISearchScheduler
    {
        public Task Delay(TimeSpan delay, CancellationToken ct = default) =>
            new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously).Task;
    }

    private sealed class ManualSearchScheduler : ISearchScheduler
    {
        private readonly Queue<TaskCompletionSource> _pending = [];
        private readonly object _gate = new();

        public int PendingCount
        {
            get { lock (_gate) return _pending.Count; }
        }

        public Task Delay(TimeSpan delay, CancellationToken ct = default)
        {
            var completion = new TaskCompletionSource(
                TaskCreationOptions.RunContinuationsAsynchronously);
            lock (_gate) _pending.Enqueue(completion);
            return completion.Task;
        }

        public void FireNext()
        {
            TaskCompletionSource completion;
            lock (_gate) completion = _pending.Dequeue();
            completion.SetResult();
        }
    }

    private sealed class FakeSearchClient : ISearchClient
    {
        private readonly Queue<Task<SearchResponse>> _responses = [];
        public bool IsConnected { get; set; } = true;
        public int SearchCount { get; private set; }
        public List<int> SearchMaxima { get; } = [];
        public IReadOnlyList<ActionItem> Actions { get; init; } = [];
        public ActionTarget? LastTarget { get; private set; }
        public SearchContext? LastContext { get; private set; }
        public string? LastQuery { get; private set; }

        public void Enqueue(SearchResponse response) => Enqueue(Task.FromResult(response));
        public void Enqueue(Task<SearchResponse> response) => _responses.Enqueue(response);
        public Task StartAsync(CancellationToken ct = default)
        {
            IsConnected = true;
            return Task.CompletedTask;
        }
        public Task<SearchResponse> SearchAsync(
            string query,
            int max,
            SearchContext context,
            CancellationToken ct = default)
        {
            SearchCount++;
            SearchMaxima.Add(max);
            LastContext = context;
            LastQuery = query;
            return _responses.Dequeue();
        }
        public Task ExecuteAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
        {
            LastTarget = target;
            return Task.CompletedTask;
        }
        public Task RevealAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
        {
            LastTarget = target;
            return Task.CompletedTask;
        }
        public Task<IReadOnlyList<ActionItem>> GetActionsAsync(
            ActionTarget target,
            CancellationToken ct = default)
        {
            LastTarget = target;
            return Task.FromResult(Actions);
        }
        public Task RunActionAsync(
            ActionTarget target,
            string action,
            string? query = null,
            CancellationToken ct = default)
        {
            LastTarget = target;
            LastActionArgs = null;
            return RunActionCoreAsync();
        }
        public Task RunActionAsync(
            ActionTarget target,
            string action,
            ActionArgs args,
            string? query = null,
            CancellationToken ct = default)
        {
            LastTarget = target;
            LastActionArgs = args;
            return RunActionCoreAsync();
        }

        private Task RunActionCoreAsync() =>
            RunActionException is { } ex ? Task.FromException(ex) : Task.CompletedTask;

        /// <summary>非空时 RunActionAsync 抛出该异常（F7 超时文案测试用）。</summary>
        public Exception? RunActionException { get; set; }

        /// <summary>最后一次带参数调用传入的 args，用于断言 rename new_name 等。</summary>
        public ActionArgs? LastActionArgs { get; set; }

        // --- G5 window mode ---

        /// <summary>非空表示 resolve 应当失败，用于覆盖窗口已关闭/句柄复用/列表过期。</summary>
        public string? ResolveFailure { get; set; }
        public WindowHandleInfo ResolvedWindow { get; set; } =
            new(new IntPtr(0x900), 77, "报告.docx - Word", IsMinimized: false);
        public List<ActionTarget> ResolveCalls { get; } = [];
        public List<ActionTarget> RecordCalls { get; } = [];
        /// <summary>非空表示写历史失败；切换本身已成功，不应被报成失败。</summary>
        public string? RecordFailure { get; set; }

        public Task<WindowHandleInfo> ResolveWindowAsync(
            ActionTarget target,
            CancellationToken ct = default)
        {
            ResolveCalls.Add(target);
            LastTarget = target;
            if (ResolveFailure is not null)
                throw new InvalidOperationException(ResolveFailure);
            return Task.FromResult(ResolvedWindow);
        }

        public Task RecordWindowSwitchAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
        {
            RecordCalls.Add(target);
            if (RecordFailure is not null)
                throw new InvalidOperationException(RecordFailure);
            return Task.CompletedTask;
        }
    }

    /// <summary>可控的窗口激活器，用于覆盖激活成功与被系统前台限制拒绝两条路径。</summary>
    private sealed class FakeWindowActivator : IWindowActivator
    {
        public bool Result { get; set; } = true;
        public List<WindowHandleInfo> Calls { get; } = [];

        public bool TryActivate(WindowHandleInfo window)
        {
            Calls.Add(window);
            return Result;
        }
    }

    /// <summary>只识别一个窗口的 Explorer 占位 adapter，用于把范围状态推到「当前目录」。</summary>
    private sealed class StubHostAdapter(IntPtr window, string folder) : IHostAdapter
    {
        public HostKind Kind => HostKind.Explorer;
        public bool IsEnabled => true;

        public HostDetection Detect(IntPtr foregroundWindow) =>
            foregroundWindow == window
                ? HostDetection.Host(Kind, HostCapability.ReadFolder)
                : HostDetection.NotHost(Kind, HostFailureReason.NotThisHost);

        public HostFolder GetFolder(IntPtr hostWindow) => HostFolder.Success(folder);

        public HostNavigation NavigateOrFill(IntPtr hostWindow, HostNavigationRequest request) =>
            HostNavigation.Success;
    }

    /// <summary>本地校验通过：「是否在索引里」只有后端知道，正是本测试要覆盖的路径。</summary>
    private sealed class AcceptingRootValidator : IRootValidator
    {
        public RootRejection? Validate(string? path, out string normalized)
        {
            normalized = path ?? "";
            return null;
        }
    }

    private sealed class AliveWindowProbe : IHostWindowProbe
    {
        public bool IsAlive(IntPtr window) => window != IntPtr.Zero;
    }
}
