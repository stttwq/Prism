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
            return _responses.Dequeue();
        }
        public Task ExecuteAsync(ActionTarget target, CancellationToken ct = default)
        {
            LastTarget = target;
            return Task.CompletedTask;
        }
        public Task RevealAsync(ActionTarget target, CancellationToken ct = default)
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
            CancellationToken ct = default)
        {
            LastTarget = target;
            return Task.CompletedTask;
        }
    }
}
