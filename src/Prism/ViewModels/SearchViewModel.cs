using System.IO;
using Prism.Models;
using Prism.Services;

namespace Prism.ViewModels;

/// <summary>
/// 搜索窗口逻辑（frontend-spec.md 流程 B/C/D）：
/// 输入防抖 50ms → PipeClient.Search → 结果列表；
/// Enter/Ctrl+N 执行；Ctrl+Enter 定位文件夹；
/// → 进入动作面板，← 返回结果。
/// </summary>
public sealed class SearchViewModel
{
    private const int InitialResultLimit = 8;
    private const int ExpandedResultLimit = 1000;

    private readonly AppState _state;
    private readonly ISearchClient _pipe;
    private readonly IDebounceTimer _debounce;
    private readonly IDebounceTimer _generationDebounce;
    private readonly ISearchScheduler _scheduler;
    private CancellationTokenSource? _searchCts;
    private int _resultLimit = InitialResultLimit;
    private string _pendingQuery = "";
    private int _searchSeq;
    private string _lastInputQuery = "";
    private SearchCacheEntry? _completeCache;
    private SearchContext _searchContext = SearchContext.Default;
    /// <summary>进入 Actions 前保存的搜索词，离开时恢复。</summary>
    private string _queryBeforeActions = "";
    /// <summary>后端返回的完整动作列表；输入时按 Label 子串过滤。</summary>
    private IReadOnlyList<ActionItem> _allActions = Array.Empty<ActionItem>();

    public AppState State => _state;

    /// <summary>执行成功后请求隐藏窗口（由 SearchWindow 订阅）。</summary>
    public event Action? HideRequested;

    private sealed record SearchCacheEntry(SearchResponse Response, SearchContext Context);

    public SearchViewModel(
        AppState state,
        ISearchClient pipe,
        IDebounceTimerFactory? timerFactory = null,
        ISearchScheduler? scheduler = null)
    {
        _state = state;
        _pipe = pipe;
        timerFactory ??= new DispatcherDebounceTimerFactory();
        _scheduler = scheduler ?? new SearchScheduler();
        _debounce = timerFactory.Create(
            TimeSpan.FromMilliseconds(50), () => _ = OnDebounceTickAsync());
        _generationDebounce = timerFactory.Create(
            TimeSpan.FromMilliseconds(100), () => _ = OnGenerationDebounceTickAsync());
    }

    /// <summary>Schedules one refresh through the existing broker search path.</summary>
    public void OnIndexGenerationChanged()
    {
        if (_state.Mode != PanelMode.Results || string.IsNullOrWhiteSpace(_state.Query))
            return;

        _completeCache = null;
        _generationDebounce.Restart();
    }

    public void SetSearchContext(SearchContext context)
    {
        if (_searchContext.IsEquivalentTo(context)) return;
        _searchContext = context;
        _completeCache = null;
        if (_state.Mode == PanelMode.Results && !string.IsNullOrWhiteSpace(_state.Query))
            _generationDebounce.Restart();
    }

    private async Task OnGenerationDebounceTickAsync()
    {
        _generationDebounce.Stop();
        if (_state.Mode != PanelMode.Results || string.IsNullOrWhiteSpace(_state.Query))
            return;

        try
        {
            await RunSearchAsync(_state.Query, _resultLimit).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "搜索失败：" + ShortMsg(ex);
        }
    }

    private async Task OnDebounceTickAsync()
    {
        _debounce.Stop();
        // Actions 模式下输入只过滤动作，不发搜索。
        if (_state.Mode == PanelMode.Actions)
        {
            FilterActions(_pendingQuery);
            return;
        }
        var q = _pendingQuery;
        try
        {
            await RunSearchAsync(q, _resultLimit).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "搜索失败：" + ShortMsg(ex);
        }
    }

    /// <summary>呼出窗口时重置到 Idle。</summary>
    public void ResetForShow()
    {
        _debounce.Stop();
        _generationDebounce.Stop();
        _searchSeq++;
        CancelSearch();
        _resultLimit = InitialResultLimit;
        _pendingQuery = "";
        _lastInputQuery = "";
        _completeCache = null;
        _queryBeforeActions = "";
        _allActions = Array.Empty<ActionItem>();
        _state.Query = "";
        _state.Results = Array.Empty<SearchResult>();
        _state.SelectedIndex = -1;
        _state.Actions = Array.Empty<ActionItem>();
        _state.SelectedActionIndex = -1;
        _state.ActionTarget = null;
        _state.Mode = PanelMode.Idle;
        _state.IsIndexing = false;
        _state.StatusMessage = _state.IsBackendConnected ? "" : "正在连接后端…";
    }

    /// <summary>输入框文本变化（由 SearchHeader 调用）。</summary>
    public void OnQueryChanged(string text)
    {
        if (!text.StartsWith(_lastInputQuery, StringComparison.Ordinal))
            _completeCache = null;
        _lastInputQuery = text;
        _state.Query = text;
        _pendingQuery = text;

        if (_state.Mode == PanelMode.Actions)
        {
            // 动作过滤：即时，无需防抖太久，但仍走 debounce 避免每键重绑。
            _debounce.Restart();
            return;
        }

        _resultLimit = InitialResultLimit;

        if (string.IsNullOrWhiteSpace(text))
        {
            _debounce.Stop();
            _generationDebounce.Stop();
            _searchSeq++;
            CancelSearch();
            _state.Results = Array.Empty<SearchResult>();
            _state.SelectedIndex = -1;
            _state.Mode = PanelMode.Idle;
            _state.IsIndexing = false;
            _state.StatusMessage = "";
            return;
        }

        _state.Mode = PanelMode.Results;
        SetSearchingStatus();

        _debounce.Restart();
    }

    public void MoveSelection(int delta)
    {
        if (_state.Mode == PanelMode.Actions)
        {
            if (_state.Actions.Count == 0) return;
            var next = _state.SelectedActionIndex;
            if (next < 0) next = 0;
            for (var step = 0; step < _state.Actions.Count; step++)
            {
                next = Math.Clamp(next + delta, 0, _state.Actions.Count - 1);
                if (!_state.Actions[next].IsSectionHeader) break;
            }
            if (next >= 0 && next < _state.Actions.Count && !_state.Actions[next].IsSectionHeader)
                _state.SelectedActionIndex = next;
            return;
        }

        if (_state.Results.Count == 0) return;
        var n = Math.Clamp(_state.SelectedIndex + delta, 0, _state.Results.Count - 1);
        _state.SelectedIndex = n;
    }

    public async Task ExecuteSelectedAsync()
    {
        if (_state.Mode == PanelMode.Actions)
        {
            await ExecuteActionAsync().ConfigureAwait(true);
            return;
        }

        var item = _state.SelectedResult;
        if (item is null) return;

        if (item.Kind == "more")
        {
            await ShowMoreAsync().ConfigureAwait(true);
            return;
        }

        if (string.IsNullOrEmpty(item.ExecuteId)) return;

        try
        {
            await _pipe.ExecuteAsync(item.ExecutionTarget).ConfigureAwait(true);
            HideRequested?.Invoke();
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "打开失败：" + ShortMsg(ex);
        }
    }

    public async Task RevealSelectedAsync()
    {
        if (_state.Mode == PanelMode.Actions) return;
        var item = _state.SelectedResult;
        if (item is null || item.Kind is "more" or "web") return;
        if (string.IsNullOrEmpty(item.ExecuteId)) return;

        try
        {
            await _pipe.RevealAsync(item.ExecutionTarget).ConfigureAwait(true);
            HideRequested?.Invoke();
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "定位失败：" + ShortMsg(ex);
        }
    }

    public async Task ExecuteIndexAsync(int oneBasedIndex)
    {
        if (_state.Mode == PanelMode.Actions) return;
        var i = oneBasedIndex - 1;
        if (i < 0 || i >= _state.Results.Count) return;
        _state.SelectedIndex = i;
        await ExecuteSelectedAsync().ConfigureAwait(true);
    }

    /// <summary>→ 键：仅 file/folder 进入动作面板。</summary>
    public async Task EnterActionsAsync()
    {
        if (_state.Mode != PanelMode.Results) return;
        var item = _state.SelectedResult;
        if (item is null) return;
        if (item.Kind is not ("file" or "folder")) return;
        if (string.IsNullOrEmpty(item.ExecuteId)) return;

        var actions = await GetActionsForAsync(item).ConfigureAwait(true);
        if (actions.Count == 0)
        {
            if (string.IsNullOrEmpty(_state.StatusMessage))
                _state.StatusMessage = "无可用动作";
            return;
        }

        _queryBeforeActions = _state.Query;
        _allActions = actions;
        _state.ActionTarget = item;
        _state.Actions = actions;
        _state.SelectedActionIndex = FirstSelectable(actions);
        _state.Mode = PanelMode.Actions;
        _state.StatusMessage = "";
        // 输入框清空，供过滤动作。
        _state.Query = "";
        _pendingQuery = "";
    }

    /// <summary>获取程序、文件或文件夹的动作列表，供动作面板和右键菜单共用。</summary>
    public async Task<IReadOnlyList<ActionItem>> GetActionsForAsync(SearchResult item)
    {
        if (item.Kind is not ("app" or "file" or "folder") || string.IsNullOrEmpty(item.ExecuteId))
            return Array.Empty<ActionItem>();

        try
        {
            if (!_pipe.IsConnected)
                await _pipe.StartAsync().ConfigureAwait(true);

            var actions = await _pipe.GetActionsAsync(item.ExecutionTarget).ConfigureAwait(true);
            if (actions.Count == 0)
                _state.StatusMessage = "无可用动作";
            return actions;
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "动作列表失败：" + ShortMsg(ex);
            return Array.Empty<ActionItem>();
        }
    }

    /// <summary>← 键或 Esc：从动作面板退回结果。</summary>
    public void LeaveActions()
    {
        if (_state.Mode != PanelMode.Actions) return;
        _state.Mode = PanelMode.Results;
        _state.Actions = Array.Empty<ActionItem>();
        _state.SelectedActionIndex = -1;
        _state.ActionTarget = null;
        _allActions = Array.Empty<ActionItem>();
        _state.Query = _queryBeforeActions;
        _pendingQuery = _queryBeforeActions;
        _state.StatusMessage = "";
    }

    public async Task ExecuteActionAsync()
    {
        if (_state.Mode != PanelMode.Actions) return;
        var action = _state.SelectedAction;
        var target = _state.ActionTarget;
        if (action is null || target is null) return;

        await RunActionOnAsync(target, action).ConfigureAwait(true);
    }

    /// <summary>执行指定目标上的动作，保持所有入口的成功隐藏和错误提示一致。</summary>
    public async Task RunActionOnAsync(SearchResult target, ActionItem action)
    {
        if (action.IsSectionHeader || string.IsNullOrEmpty(action.Id)) return;
        if (target.Kind is not ("app" or "file" or "folder") || string.IsNullOrEmpty(target.ExecuteId))
            return;

        try
        {
            await _pipe.RunActionAsync(target.ExecutionTarget, action.Id).ConfigureAwait(true);
            HideRequested?.Invoke();
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "动作失败：" + ShortMsg(ex);
        }
    }

    private void FilterActions(string filter)
    {
        if (_allActions.Count == 0) return;
        IReadOnlyList<ActionItem> filtered;
        if (string.IsNullOrWhiteSpace(filter))
        {
            filtered = _allActions;
        }
        else
        {
            var list = new List<ActionItem>();
            foreach (var a in _allActions)
            {
                if (a.IsSectionHeader)
                {
                    // 节标题保留，后面若无内容可再清；简单起见先保留。
                    list.Add(a);
                    continue;
                }
                if (a.Label.Contains(filter, StringComparison.OrdinalIgnoreCase))
                    list.Add(a);
            }
            // 去掉后面没有普通项的孤立节标题。
            for (var i = list.Count - 1; i >= 0; i--)
            {
                if (!list[i].IsSectionHeader) continue;
                var hasAfter = i + 1 < list.Count && !list[i + 1].IsSectionHeader;
                if (!hasAfter) list.RemoveAt(i);
            }
            filtered = list;
        }
        _state.Actions = filtered;
        _state.SelectedActionIndex = FirstSelectable(filtered);
    }

    private static int FirstSelectable(IReadOnlyList<ActionItem> items)
    {
        for (var i = 0; i < items.Count; i++)
            if (!items[i].IsSectionHeader) return i;
        return -1;
    }

    private async Task ShowMoreAsync()
    {
        _resultLimit = ExpandedResultLimit;
        await RunSearchAsync(_state.Query, _resultLimit).ConfigureAwait(true);
    }

    private async Task RunSearchAsync(string query, int max)
    {
        if (string.IsNullOrWhiteSpace(query)) return;

        var seq = ++_searchSeq;
        CancelSearch();
        if (TryFilterCompleteCache(query, out var cached))
        {
            ApplySearchResponse(cached, query, max, seq, startPoll: false, updateCache: false);
            return;
        }
        var cts = new CancellationTokenSource();
        _searchCts = cts;

        if (!_pipe.IsConnected)
        {
            _state.IsBackendConnected = false;
            _state.StatusMessage = "正在连接后端…";
            try
            {
                await _pipe.StartAsync(cts.Token).ConfigureAwait(true);
                _state.IsBackendConnected = true;
            }
            catch (Exception ex)
            {
                if (seq != _searchSeq) return;
                _state.StatusMessage = "后端未就绪：" + ShortMsg(ex);
                return;
            }
        }

        try
        {
            if (seq == _searchSeq)
                SetSearchingStatus();

            var resp = await _pipe.SearchAsync(query, max, _searchContext, cts.Token).ConfigureAwait(true);

            if (seq != _searchSeq) return;
            if (cts.IsCancellationRequested) return;
            if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
            if (!string.Equals(resp.Query, query, StringComparison.Ordinal)) return;

            ApplySearchResponse(resp, query, max, seq);
        }
        catch (OperationCanceledException)
        {
            // 新的输入打断旧搜索，忽略。
        }
        catch (Exception ex)
        {
            if (seq != _searchSeq) return;
            _state.IsBackendConnected = false;
            _state.StatusMessage = "搜索失败：" + ShortMsg(ex);
            System.Diagnostics.Debug.WriteLine("[Prism] search error: " + ex);
            try
            {
                await _pipe.StartAsync().ConfigureAwait(true);
                _state.IsBackendConnected = true;
            }
            catch
            {
                // 保持断开提示。
            }
        }
    }

    private void SetSearchingStatus()
    {
        if (_state.Results.Count == 0)
        {
            _state.StatusMessage = "搜索中…";
        }
        else if (!_state.IsIndexing)
        {
            // Keep existing rows at a stable height while the replacement
            // response is in flight. Indexing progress remains visible.
            _state.StatusMessage = "";
        }
    }

    private void ApplySearchResponse(
        SearchResponse resp,
        string query,
        int max,
        int seq,
        bool startPoll = true,
        bool updateCache = true)
    {
        var list = new List<SearchResult>(resp.Items.Count + 1);
        list.AddRange(resp.Items);
        if (resp.IsTruncated)
            list.Add(SearchResult.More(query));

        if (updateCache
            && !resp.IsIndexing
            && string.IsNullOrWhiteSpace(resp.IndexError)
            && !resp.IsTruncated
            && resp.IndexGeneration.HasValue
            && (resp.PinyinStatus is null or "disabled")
            && resp.Items.All(item => item.ResultKind != SearchResultKind.Web))
        {
            _completeCache = new SearchCacheEntry(resp, _searchContext);
        }

        var prevId = _state.SelectedResult?.ExecuteId;
        _state.Results = list;
        var keep = -1;
        if (!string.IsNullOrEmpty(prevId))
        {
            for (var i = 0; i < list.Count; i++)
            {
                if (list[i].ExecuteId == prevId) { keep = i; break; }
            }
        }
        _state.SelectedIndex = keep >= 0 ? keep : (list.Count > 0 ? 0 : -1);
        // 不在 Actions 时才切回 Results。
        if (_state.Mode != PanelMode.Actions)
            _state.Mode = PanelMode.Results;
        _state.IsIndexing = resp.IsIndexing;

        if (resp.IsIndexing)
        {
            _state.StatusMessage = IndexingStatus(resp, list.Count > 0);
            if (startPoll)
                _ = PollUntilReadyAsync(query, max, seq);
        }
        else if (!string.IsNullOrWhiteSpace(resp.IndexError))
        {
            _state.StatusMessage = "文件索引不可用：" + ShortMsg(new IOException(resp.IndexError));
        }
        else if (resp.PinyinStatus is "corrupt" or "version_mismatch" or "index_mismatch" or "missing")
        {
            _state.StatusMessage = "拼音索引不可用，已使用字面搜索";
        }
        else if (!string.IsNullOrWhiteSpace(resp.HistoryStatus))
        {
            _state.StatusMessage = "使用历史已重置";
        }
        else if (list.Count > 0)
        {
            _state.StatusMessage = "";
        }
        else
        {
            _state.StatusMessage = "无匹配结果";
        }
    }

    /// <summary>
    /// 索引未建完时的提示文案。后端给出逐卷进度就显示可解释进度（G9 R4），
    /// 缺进度字段时回落到原来的等待文案，行为与旧后端一致。
    /// </summary>
    private static string IndexingStatus(SearchResponse resp, bool hasResults)
    {
        var progress = resp.IndexProgress?.Describe();
        if (progress is { Length: > 0 })
        {
            return hasResults
                ? $"正在建立索引（{progress}），已可搜索部分文件…"
                : $"正在建立索引（{progress}）…";
        }
        return hasResults
            ? "索引加载中，正在补充文件结果…"
            : "索引加载中，请稍候…";
    }

    private async Task PollUntilReadyAsync(string query, int max, int seq)
    {
        SearchResponse? lastIndexingResponse = null;
        for (var i = 0; i < 30; i++)
        {
            try
            {
                await _scheduler.Delay(TimeSpan.FromMilliseconds(500)).ConfigureAwait(true);
            }
            catch { return; }

            if (seq != _searchSeq) return;
            if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
            if (!_state.IsIndexing) return;
            if (!_pipe.IsConnected) continue;

            try
            {
                var resp = await _pipe.SearchAsync(
                    query, max, _searchContext).ConfigureAwait(true);
                if (seq != _searchSeq) return;
                if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
                if (!string.Equals(resp.Query, query, StringComparison.Ordinal)) return;

                if (!resp.IsIndexing)
                {
                    ApplySearchResponse(resp, query, max, seq, startPoll: false);
                    return;
                }

                // Progress-only responses are still meaningful. Applying them keeps the
                // displayed volume/count snapshot current even when this query has no hits.
                lastIndexingResponse = resp;
                ApplySearchResponse(resp, query, max, seq, startPoll: false);
            }
            catch (OperationCanceledException)
            {
                return;
            }
            catch
            {
                // 继续轮询。
            }
        }

        if (seq == _searchSeq
            && string.Equals(query, _state.Query, StringComparison.Ordinal)
            && _state.IsIndexing)
        {
            _state.StatusMessage = lastIndexingResponse is not null
                ? IndexingStatus(lastIndexingResponse, _state.Results.Count > 0)
                : _state.Results.Count > 0
                    ? "文件索引仍在加载，当前仅显示已就绪结果"
                    : "索引仍在加载，请稍后再试";
        }
    }

    private void CancelSearch()
    {
        try { _searchCts?.Cancel(); } catch { /* ignore */ }
        _searchCts?.Dispose();
        _searchCts = null;
    }

    private bool TryFilterCompleteCache(string query, out SearchResponse response)
    {
        var cached = _completeCache;
        if (cached is null
            || !cached.Context.IsEquivalentTo(_searchContext)
            || !query.StartsWith(cached.Response.Query, StringComparison.Ordinal)
            || string.Equals(query, cached.Response.Query, StringComparison.Ordinal))
        {
            response = null!;
            return false;
        }

        var items = cached.Response.Items
            .Where(item => item.Title.Contains(query, StringComparison.OrdinalIgnoreCase))
            .ToArray();
        response = cached.Response with { Query = query, Items = items };
        return true;
    }

    private static string ShortMsg(Exception ex)
    {
        var m = ex.Message;
        const string prefix = "后端返回错误：";
        return m.StartsWith(prefix, StringComparison.Ordinal) ? m[prefix.Length..] : m;
    }
}
