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

    // ── 工作集名字召回（2026-08-22 计划阶段三）────────────────────────

    [Fact]
    public async Task ExactWorksetNameQueryInjectsRecallRowAtTopAndEnterRaisesEvent()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("8月报告", false, 1, Result("alpha")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var staging = new StagingArea();
        staging.Restore([], [new WorksetEntry("8月报告", "还差一张图", ["C:\\a.docx"])]);
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler(), staging: staging);

        vm.OnQueryChanged("8月报告");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 2);

        var recall = state.Results[0];
        Assert.Equal("workset", recall.Kind);
        Assert.Equal("8月报告", recall.Title);
        Assert.Equal("workset:8月报告", recall.RowKey);
        // 后端文件结果仍在其后；默认选中第 0 行 = 合成行。
        Assert.Equal("alpha", state.Results[1].Title);
        Assert.Equal(0, state.SelectedIndex);

        string? recalled = null;
        vm.WorksetRecallRequested += name => recalled = name;
        await vm.ExecuteSelectedAsync();
        Assert.Equal("8月报告", recalled);
        // 载入后窗口不隐藏：HideRequested 不触发（用状态佐证——无断言手段，仅行为不炸）。
    }

    [Fact]
    public async Task NonExactWorksetNameQueryInjectsNothing()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("8月", false, 1, Result("alpha")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var staging = new StagingArea();
        staging.Restore([], [new WorksetEntry("8月报告", null, ["C:\\a.docx"])]);
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler(), staging: staging);

        vm.OnQueryChanged("8月");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);
        Assert.Equal("alpha", state.Results[0].Title);
        Assert.All(state.Results, r => Assert.NotEqual("workset", r.Kind));
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
    public async Task PathQueryBypassesPrefixCacheAndGoesToBroker()
    {
        // P1（第一轮 bug 修复）："E" 的完整缓存不得拦截 "E:\foo"——路径前缀增长
        // 是换路径，不是同名过滤；必须发 broker（那里按路径语义处理）。
        var client = new FakeSearchClient();
        client.Enqueue(Response("E", false, 7, Result("Excel")));
        client.Enqueue(Response(@"E:\foo", false, 7, Result("bar")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("E");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1 && state.Results.Count == 1);

        vm.OnQueryChanged(@"E:\foo");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2 && state.Results.Count == 1);
        Assert.Equal("bar", state.Results[0].Title);
    }

    [Theory]
    [InlineData(@"E:\foo", true)]
    [InlineData("e:/x", true)]
    [InlineData(@"\\server\share", true)]
    [InlineData("//srv/x", true)]
    [InlineData("\"E:\\foo bar\"", true)]   // Explorer 复制文件地址形态
    [InlineData(" \"E:\\foo\" ", true)]     // 引号外再带空白
    [InlineData("note:foo", false)]
    [InlineData("E", false)]
    [InlineData(@"foo\bar", false)]
    [InlineData("\"\"", false)]
    public void PathQueryDetectionMatchesBrokerRules(string query, bool expected)
    {
        Assert.Equal(expected, SearchViewModel.IsAbsolutePathQuery(query));
    }

    /// <summary>H7（全仓复审 2026-08-22）：ext:/path: 过滤词查询的识别口径。
    /// 宽松超集：空值（"path:"）broker 会回退为普通文本，这里仍判 true——
    /// 宁可少缓存，不可错过滤。</summary>
    [Theory]
    [InlineData("ext:pdf", true)]
    [InlineData("EXT:PDF", true)]            // 大小写不敏感，与 broker 一致
    [InlineData("note ext:pdf", true)]       // token 起点在查询中段也识别
    [InlineData(@"path:""E:\foo""", true)]   // 带引号的 path 值
    [InlineData("path:", true)]              // 空值也按过滤词排除（保守超集）
    [InlineData("foo:bar", false)]           // 未知前缀
    [InlineData("extent", false)]            // 前缀必须顶 token 头
    public void FilterTokenDetectionMatchesBrokerPrefixes(string query, bool expected)
    {
        Assert.Equal(expected, SearchViewModel.HasFilterToken(query));
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

    /// <summary>
    /// 2026-08-24 修复：查询与别名词精确相等时必须直发 broker——前缀缓存按
    /// 标题子串过滤，永远变不出别名行（别名通道只在精确相等时由 broker 出行，
    /// 缓存响应里也不会有该词的别名行）。词集已加载且包含查询词 → 绕过缓存。
    /// </summary>
    [Fact]
    public async Task AliasWordQueryBypassesPrefixCacheAndGoesToBroker()
    {
        var client = new FakeSearchClient();
        // 三轮真实查询：首击 "wx"、删字 "w"、再回到别名精确词 "wx"。
        client.Enqueue(Response("wx", false, 7, Result("wxWidgets.h")));
        client.Enqueue(Response("w", false, 7, Result("wxWidgets.h"), Result("weight.csv")));
        client.Enqueue(Response("wx", false, 7, Result("微信")));
        var timers = new ManualTimerFactory();
        var state = new AppState();
        IReadOnlyList<AliasEntry> aliases =
        [
            new AliasEntry(
                new ActionTarget("application", @"C:\Program Files\Tencent\WeChat\WeChat.exe"),
                ["wx"],
                1000),
        ];
        var vm = new SearchViewModel(
            state, client, timers, new ImmediateScheduler(),
            aliasList: () => Task.FromResult(aliases));

        vm.OnQueryChanged("wx");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);
        vm.OnQueryChanged("w");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 2);
        // "w" 的完整响应已入缓存；"wx" 是其前缀增长且恰为别名词——必须绕过
        // 缓存直发 broker（缓存过滤只会给出 wxWidgets.h，别名行变不出来）。
        vm.OnQueryChanged("wx");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 3 && state.Results.Count == 1);
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

    /// <summary>Bug 4：普通搜索框直接输入网址（无 g 关键词）即显示一行"打开 …"结果，
    /// 不走本地文件搜索。Enter 后 ExecuteAsync 收到 kind="web"。</summary>
    [Fact]
    public async Task BareUrlInSearchBoxShowsDirectOpenRow()
    {
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("example.com");
        timers.Input.Fire();
        await Eventually(() => state.Results.Count == 1);

        Assert.False(state.IsWebMode);                // 非网页搜索模式
        Assert.Equal("打开 example.com", state.Results[0].Title);
        Assert.Equal("https://example.com", state.Results[0].Subtitle);
        Assert.Equal("web", state.Results[0].Target!.Kind);
        Assert.Equal("https://example.com", state.Results[0].Target!.Value);
        Assert.Equal(0, client.SearchCount);           // 不走本地文件搜索
    }

    /// <summary>Bug 4：文件名类输入（setup.exe）不当网址，继续走文件搜索。</summary>
    [Fact]
    public async Task FileNameInSearchBoxGoesToFileSearch()
    {
        var client = new FakeSearchClient();
        var timers = new ManualTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("setup.exe");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        Assert.False(state.IsWebMode);
        // 未产生 web 行——走的是文件搜索（SearchAsync 被调用）。
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

    // ── 2026-08-25 修复：delete_permanent 的 DestructiveDialog 协同 ──

    /// <summary>永久删除发起前后触发 Started/Ended；未固定成功后不走「保留窗口刷新」
    /// 路径（窗口已为本动作隐藏，刷新无意义），也不触发 Failed。</summary>
    [Fact]
    public async Task PermanentDeleteRaisesDialogEventsAndSkipsRefreshWhenUnpinned()
    {
        var client = new FakeSearchClient();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, new ManualTimerFactory(), new ImmediateScheduler());
        var order = new List<string>();
        vm.DestructiveDialogStarted += () => order.Add("started");
        vm.DestructiveDialogEnded += () => order.Add("ended");
        vm.DestructiveDialogFailed += _ => order.Add("failed");

        var target = new SearchResult("file", "x.txt", @"C:\x.txt", @"C:\x.txt", []);
        await vm.RunActionOnAsync(target, new ActionItem("delete_permanent", "永久删除", "", false, false));

        Assert.Equal(new[] { "started", "ended" }, order);
        Assert.Equal(0, client.SearchCount); // 未触发刷新重搜
    }

    /// <summary>永久删除失败走 DestructiveDialogFailed（窗口侧保证错误可见），
    /// 超时文案与其他 mutation 一致报「结果未知」。</summary>
    [Fact]
    public async Task PermanentDeleteFailureRaisesFailedEventWithTimeoutWording()
    {
        var client = new FakeSearchClient
        {
            RunActionException = new IOException("后端响应超时（300 秒）"),
        };
        var state = new AppState();
        var vm = new SearchViewModel(state, client, new ManualTimerFactory(), new ImmediateScheduler());
        string? failed = null;
        vm.DestructiveDialogFailed += m => failed = m;

        var target = new SearchResult("file", "x.txt", @"C:\x.txt", @"C:\x.txt", []);
        await vm.RunActionOnAsync(target, new ActionItem("delete_permanent", "永久删除", "", false, false));

        Assert.NotNull(failed);
        Assert.Contains("结果未知", failed);
        // 超时异常不再落 StatusMessage（窗口可能已隐藏，由 Failed 事件的订阅方展示）。
        Assert.DoesNotContain("结果未知", state.StatusMessage);
    }

    /// <summary>钉住窗口不隐藏，成功后照旧刷新结果（与 recycle 等一致）。</summary>
    [Fact]
    public async Task PermanentDeletePinnedStillRefreshesResults()
    {
        var client = new FakeSearchClient();
        client.Enqueue(Response("x", false, 1, Result("x")));
        client.Enqueue(Response("x", false, 2, Result("x")));
        var timers = new ManualTimerFactory();
        var state = new AppState { IsPinned = true };
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());
        vm.OnQueryChanged("x");
        timers.Input.Fire();
        await Eventually(() => client.SearchCount == 1);

        var target = new SearchResult("file", "x.txt", @"C:\x.txt", @"C:\x.txt", []);
        await vm.RunActionOnAsync(target, new ActionItem("delete_permanent", "永久删除", "", false, false));

        // RefreshAsync 的 3 秒 generation 超时在 ImmediateScheduler 下立即到期并重搜。
        await Eventually(() => client.SearchCount == 2);
    }

    // K2 commit 4 回归：命令动作（InvocationKind=command）走 ExecuteCommand 降级分支，
    // 不进 rename/picker/delete 硬编码内置分支。fake client 非 PipeClient → 降级。
    [Fact]
    public async Task CommandActionDoesNotEnterBuiltinBranches()
    {
        var client = new FakeSearchClient();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, new ManualTimerFactory(), new ImmediateScheduler());

        var target = new SearchResult("folder", "Windows", @"C:\Windows", @"C:\Windows", []);
        var cmdAction = new ActionItem("cmd:prism.terminal.open", "在此处打开终端", "", false, false)
        {
            InvocationKind = "command",
            CommandId = "prism.terminal.open",
        };
        await vm.RunActionOnAsync(target, cmdAction);

        // fake client 非 PipeClient → 降级分支文案。
        Assert.Equal("命令执行不可用", state.StatusMessage);
        // 确认没进内置动作路径（RunActionAsync 未被调用）。
        Assert.Equal(0, client.RunActionCallCount);
    }

    // K2 commit 4 回归：禁用动作不可执行，展示 disabled_reason。
    [Fact]
    public async Task DisabledActionShowsReasonAndDoesNotExecute()
    {
        var client = new FakeSearchClient();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, new ManualTimerFactory(), new ImmediateScheduler());

        var target = new SearchResult("file", "x.txt", @"C:\x.txt", @"C:\x.txt", []);
        var disabled = new ActionItem("zip", "压缩为 ZIP", "", false, false)
        {
            IsEnabled = false,
            DisabledReason = "需要 7-Zip",
        };

        // 模拟动作面板选中禁用项 + Enter。
        state.Mode = PanelMode.Actions;
        state.Actions = new[] { disabled };
        state.SelectedActionIndex = 0;
        state.ActionTarget = target;
        await vm.ExecuteActionAsync();

        Assert.Equal("需要 7-Zip", state.StatusMessage);
        Assert.Equal(0, client.RunActionCallCount);
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

    // 2026-08-22 拖拽阶段一：OLE 拖出进行中不隐藏——鼠标按住拖离窗口时
    // 激活/前台变更会先于松手到达，闸不挡则拖拽中途窗口消失、拖拽断裂。
    [Fact]
    public void FocusLossNeverHidesWhileDragging()
    {
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, false, false, false, false, true));
        // 其余守卫为假时 isDragging 单独挡；与其余守卫组合时同样挡（任一为真即不隐藏）。
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, false, false, false, false, isDragging: true));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(true, false, false, false, false, true));
        Assert.False(SearchWindowFocusPolicy.ShouldHide(false, true, false, true, false, true));
        // 拖拽结束后闸恢复常规语义。
        Assert.True(SearchWindowFocusPolicy.ShouldHide(false, false, false, false, false, false));
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

        // K2 commit 4：命令动作不应进内置 RunActionAsync 路径。
        private int _runActionCallCount;
        public int RunActionCallCount => _runActionCallCount;

        private Task RunActionCoreAsync()
        {
            _runActionCallCount++;
            return RunActionException is { } ex ? Task.FromException(ex) : Task.CompletedTask;
        }

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
