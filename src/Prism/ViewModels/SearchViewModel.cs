using System.Windows.Threading;
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
    private readonly PipeClient _pipe;
    private readonly DispatcherTimer _debounce;
    private CancellationTokenSource? _searchCts;
    private int _resultLimit = InitialResultLimit;
    private string _pendingQuery = "";
    private int _searchSeq;
    /// <summary>进入 Actions 前保存的搜索词，离开时恢复。</summary>
    private string _queryBeforeActions = "";
    /// <summary>后端返回的完整动作列表；输入时按 Label 子串过滤。</summary>
    private IReadOnlyList<ActionItem> _allActions = Array.Empty<ActionItem>();

    public AppState State => _state;

    /// <summary>执行成功后请求隐藏窗口（由 SearchWindow 订阅）。</summary>
    public event Action? HideRequested;

    public SearchViewModel(AppState state, PipeClient pipe)
    {
        _state = state;
        _pipe = pipe;
        _debounce = new DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(50),
        };
        _debounce.Tick += OnDebounceTick;
    }

    private async void OnDebounceTick(object? sender, EventArgs e)
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
        _searchSeq++;
        CancelSearch();
        _resultLimit = InitialResultLimit;
        _pendingQuery = "";
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
        _state.Query = text;
        _pendingQuery = text;

        if (_state.Mode == PanelMode.Actions)
        {
            // 动作过滤：即时，无需防抖太久，但仍走 debounce 避免每键重绑。
            _debounce.Stop();
            _debounce.Start();
            return;
        }

        _resultLimit = InitialResultLimit;

        if (string.IsNullOrWhiteSpace(text))
        {
            _debounce.Stop();
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
        _state.StatusMessage = "搜索中…";

        _debounce.Stop();
        _debounce.Start();
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
            await _pipe.ExecuteAsync(item.ExecuteId).ConfigureAwait(true);
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
            await _pipe.RevealAsync(item.ExecuteId).ConfigureAwait(true);
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

        try
        {
            if (!_pipe.IsConnected)
                await _pipe.StartAsync().ConfigureAwait(true);

            var actions = await _pipe.GetActionsAsync(item.ExecuteId).ConfigureAwait(true);
            if (actions.Count == 0)
            {
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
        catch (Exception ex)
        {
            _state.StatusMessage = "动作列表失败：" + ShortMsg(ex);
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
        if (action is null || action.IsSectionHeader) return;
        if (target is null || string.IsNullOrEmpty(target.ExecuteId)) return;
        if (string.IsNullOrEmpty(action.Id) || action.IsSectionHeader)
            return;

        try
        {
            await _pipe.RunActionAsync(target.ExecuteId, action.Id).ConfigureAwait(true);
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
                _state.StatusMessage = "搜索中…";

            var resp = await _pipe.SearchAsync(query, max, cts.Token).ConfigureAwait(true);

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

    private void ApplySearchResponse(SearchResponse resp, string query, int max, int seq, bool startPoll = true)
    {
        var list = new List<SearchResult>(resp.Items.Count + 1);
        list.AddRange(resp.Items);
        if (resp.Items.Count > 0 && resp.Items.Count >= max)
            list.Add(SearchResult.More(query));

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
            _state.StatusMessage = list.Count > 0
                ? "索引加载中，正在补充文件结果…"
                : "索引加载中，请稍候…";
            if (startPoll)
                _ = PollUntilReadyAsync(query, max, seq);
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

    private async Task PollUntilReadyAsync(string query, int max, int seq)
    {
        for (var i = 0; i < 30; i++)
        {
            try
            {
                await Task.Delay(500).ConfigureAwait(true);
            }
            catch { return; }

            if (seq != _searchSeq) return;
            if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
            if (!_state.IsIndexing) return;
            if (!_pipe.IsConnected) continue;

            try
            {
                var resp = await _pipe.SearchAsync(query, max).ConfigureAwait(true);
                if (seq != _searchSeq) return;
                if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
                if (!string.Equals(resp.Query, query, StringComparison.Ordinal)) return;

                if (!resp.IsIndexing)
                {
                    ApplySearchResponse(resp, query, max, seq, startPoll: false);
                    return;
                }

                if (resp.Items.Count > 0)
                    ApplySearchResponse(resp, query, max, seq, startPoll: false);
                else
                    _state.StatusMessage = "索引加载中，请稍候…";
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
            _state.StatusMessage = _state.Results.Count > 0
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

    private static string ShortMsg(Exception ex)
    {
        var m = ex.Message;
        const string prefix = "后端返回错误：";
        return m.StartsWith(prefix, StringComparison.Ordinal) ? m[prefix.Length..] : m;
    }
}
