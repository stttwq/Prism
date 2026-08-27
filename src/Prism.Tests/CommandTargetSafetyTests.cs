using System.Text.Json;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K0 施工方案补缺测试：C4/C5/C6/C10/C11。
/// 这 5 项在施工方案测试清单中标明但之前未实现。
/// C5 是「K0 最关键的单行断言」——ActionTarget.FromLegacy("command",...)
/// 必须映射成 command 而非 file。
/// </summary>
public sealed class CommandTargetSafetyTests
{
    // ------------------------------------------------------------------
    // C4：ParseSearchResponse 的 cacheable / command_catalog_generation 解析
    // ------------------------------------------------------------------

    /// <summary>无 cacheable 字段 → 默认 true（与 broker skip_serializing_if=is_true 配对）。</summary>
    [Fact]
    public void ParseSearchResponse_Missing_Cacheable_Defaults_True()
    {
        var json = """
            {"query":"a","items":[]}
            """;
        using var doc = JsonDocument.Parse(json);
        var resp = PipeClient.ParseSearchResponse(doc.RootElement, "a");

        Assert.True(resp.Cacheable);
        Assert.Null(resp.CommandCatalogGeneration);
    }

    /// <summary>cacheable:false → 解析为 false。</summary>
    [Fact]
    public void ParseSearchResponse_Cacheable_False_Parsed()
    {
        var json = """
            {"query":"a","items":[],"cacheable":false}
            """;
        using var doc = JsonDocument.Parse(json);
        var resp = PipeClient.ParseSearchResponse(doc.RootElement, "a");

        Assert.False(resp.Cacheable);
    }

    /// <summary>command_catalog_generation 解析。</summary>
    [Fact]
    public void ParseSearchResponse_Command_Catalog_Generation_Parsed()
    {
        var json = """
            {"query":"a","items":[],"command_catalog_generation":42}
            """;
        using var doc = JsonDocument.Parse(json);
        var resp = PipeClient.ParseSearchResponse(doc.RootElement, "a");

        Assert.Equal(42ul, resp.CommandCatalogGeneration);
    }

    // ------------------------------------------------------------------
    // C5：ActionTarget.FromLegacy("command", ...) → Kind == "command"
    // K0 最高价值单行断言：防止命令 id 被当文件路径送进 Shell 链路。
    // ------------------------------------------------------------------

    [Fact]
    public void FromLegacy_Command_Kind_Is_Command_Not_File()
    {
        var target = ActionTarget.FromLegacy("command", "prism.settings.open");

        Assert.Equal("command", target.Kind);
        Assert.Equal("prism.settings.open", target.Value);
    }

    // ------------------------------------------------------------------
    // C6：ParseResult 命令行 → RowKey == "command:{id}"
    // 防止 Title 变更导致容器删旧插新（重演网页行图标闪烁缺陷）。
    // ------------------------------------------------------------------

    [Fact]
    public void ParseResult_Command_Row_Has_Stable_RowKey()
    {
        var json = """
            {"kind":"command","title":"设置","subtitle":"","execute_id":"prism.settings.open"}
            """;
        using var doc = JsonDocument.Parse(json);
        var result = PipeClient.ParseResult(doc.RootElement);

        Assert.Equal("command", result.Kind);
        Assert.Equal("prism.settings.open", result.ExecuteId);
        Assert.Equal("command:prism.settings.open", result.RowKey);
    }

    // ------------------------------------------------------------------
    // C10：命令行 Enter → ExecuteAsync 不调用；Ctrl+Enter → RevealAsync 不调用
    // 用 SearchViewModelTests 同款 FakeSearchClient + 计数手法。
    // ------------------------------------------------------------------

    [Fact]
    public async Task Command_Row_Enter_Does_Not_Execute()
    {
        var client = new CommandSafetyFakeClient();
        client.Enqueue(new SearchResponse("a", [
            new SearchResult("command", "设置", "", "prism.settings.open", []),
        ], false, null, false, 1));

        var timers = new CommandSafetyTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await CommandSafetyTestsEventually(() => state.Results.Count == 1);

        state.SelectedIndex = 0;
        Assert.Equal("command", state.Results[0].Kind);
        await vm.ExecuteSelectedAsync();

        Assert.Equal(0, client.ExecuteCallCount);
        Assert.Equal("命令不可用", state.StatusMessage);
    }

    [Fact]
    public async Task Command_Row_Reveal_Does_Not_Invoke()
    {
        var client = new CommandSafetyFakeClient();
        client.Enqueue(new SearchResponse("a", [
            new SearchResult("command", "设置", "", "prism.settings.open", []),
        ], false, null, false, 1));

        var timers = new CommandSafetyTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        vm.OnQueryChanged("a");
        timers.Input.Fire();
        await CommandSafetyTestsEventually(() => state.Results.Count == 1);

        state.SelectedIndex = 0;
        await vm.RevealSelectedAsync();

        Assert.Equal(0, client.RevealCallCount);
    }

    // ------------------------------------------------------------------
    // C11：Cacheable=false 的响应不进 _completeCache
    // 后续同前缀输入应触发真实请求（SearchCount 增加），而非命中缓存。
    // ------------------------------------------------------------------

    [Fact]
    public async Task Cacheable_False_Response_Does_Not_Seed_Cache()
    {
        var client = new CommandSafetyFakeClient();
        // 第一次响应：Cacheable=false → 不入缓存
        client.Enqueue(new SearchResponse("ab", [
            new SearchResult("file", "abc", "C:\\abc", "C:\\abc", []),
        ], false, null, false, 1, null, null, null, null, Cacheable: false));
        // 第二次响应（前缀扩展后必然发真实请求）
        client.Enqueue(new SearchResponse("abc", [
            new SearchResult("file", "abc", "C:\\abc", "C:\\abc", []),
        ], false, null, false, 1));

        var timers = new CommandSafetyTimerFactory();
        var state = new AppState();
        var vm = new SearchViewModel(state, client, timers, new ImmediateScheduler());

        // 第一次查询 "ab"
        vm.OnQueryChanged("ab");
        timers.Input.Fire();
        await CommandSafetyTestsEventually(() => state.Results.Count == 1);
        Assert.Equal(1, client.SearchCount);

        // 前缀扩展到 "abc" —— 如果 Cacheable=false 的响应入了缓存，
        // TryFilterCompleteCache 会命中，SearchCount 不会增加。
        vm.OnQueryChanged("abc");
        timers.Input.Fire();
        await CommandSafetyTestsEventually(() => client.SearchCount == 2);

        Assert.Equal(2, client.SearchCount);
    }

    // ==================================================================
    // 辅助类型（与 SearchViewModelTests 同构，独立声明避免耦合）
    // ==================================================================

    private static async Task CommandSafetyTestsEventually(Func<bool> condition)
    {
        for (var i = 0; i < 100 && !condition(); i++)
            await Task.Delay(5);
        Assert.True(condition());
    }

    private sealed class CommandSafetyTimerFactory : IDebounceTimerFactory
    {
        private readonly List<CommandSafetyTimer> _timers = [];
        public CommandSafetyTimer Input => _timers[0];

        public IDebounceTimer Create(TimeSpan interval, Action callback)
        {
            var timer = new CommandSafetyTimer(callback);
            _timers.Add(timer);
            return timer;
        }
    }

    private sealed class CommandSafetyTimer(Action callback) : IDebounceTimer
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

    private sealed class CommandSafetyFakeClient : ISearchClient
    {
        private readonly Queue<Task<SearchResponse>> _responses = [];
        public bool IsConnected { get; set; } = true;
        public int SearchCount { get; private set; }
        public int ExecuteCallCount { get; private set; }
        public int RevealCallCount { get; private set; }
        public ActionTarget? LastTarget { get; private set; }

        public void Enqueue(SearchResponse response) => Enqueue(Task.FromResult(response));
        public void Enqueue(Task<SearchResponse> response) => _responses.Enqueue(response);

        public Task StartAsync(CancellationToken ct = default)
        {
            IsConnected = true;
            return Task.CompletedTask;
        }

        public Task<SearchResponse> SearchAsync(
            string query, int max, SearchContext context, CancellationToken ct = default)
        {
            SearchCount++;
            return _responses.Dequeue();
        }

        public Task ExecuteAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
        {
            ExecuteCallCount++;
            LastTarget = target;
            return Task.CompletedTask;
        }

        public Task RevealAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
        {
            RevealCallCount++;
            LastTarget = target;
            return Task.CompletedTask;
        }

        public Task<IReadOnlyList<ActionItem>> GetActionsAsync(
            ActionTarget target, CancellationToken ct = default)
            => Task.FromResult<IReadOnlyList<ActionItem>>([]);

        public Task RunActionAsync(
            ActionTarget target, string action, string? query = null, CancellationToken ct = default)
            => Task.CompletedTask;

        public Task RunActionAsync(
            ActionTarget target, string action, ActionArgs args, string? query = null, CancellationToken ct = default)
            => Task.CompletedTask;

        public Task<WindowHandleInfo> ResolveWindowAsync(ActionTarget target, CancellationToken ct = default)
            => Task.FromResult(new WindowHandleInfo(IntPtr.Zero, 0, "", false));

        public Task RecordWindowSwitchAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
            => Task.CompletedTask;
    }
}
