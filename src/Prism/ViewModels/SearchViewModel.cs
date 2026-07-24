using System.Windows.Threading;
using Prism.Models;
using Prism.Services;

namespace Prism.ViewModels;

/// <summary>
/// 搜索窗口逻辑（frontend-spec.md 流程 B/C）：
/// 输入防抖 50ms → PipeClient.Search → 结果列表；
/// Enter/Ctrl+N 执行；Ctrl+Enter 定位文件夹。
/// </summary>
public sealed class SearchViewModel
{
    private readonly AppState _state;
    private readonly PipeClient _pipe;
    private readonly DispatcherTimer _debounce;
    private CancellationTokenSource? _searchCts;
    private int _resultLimit = 100;
    private string _pendingQuery = "";
    private int _searchSeq;

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
        var q = _pendingQuery;
        try
        {
            await RunSearchAsync(q, _resultLimit).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            // async void 最后防线：绝不能让异常冲出 UI 线程。
            _state.StatusMessage = "搜索失败：" + ShortMsg(ex);
        }
    }

    /// <summary>呼出窗口时重置到 Idle。</summary>
    public void ResetForShow()
    {
        _debounce.Stop();
        // 递增 seq，让在途搜索 / 索引轮询全部失效。
        _searchSeq++;
        CancelSearch();
        _resultLimit = 100;
        _pendingQuery = "";
        _state.Query = "";
        _state.Results = Array.Empty<SearchResult>();
        _state.SelectedIndex = -1;
        _state.Mode = PanelMode.Idle;
        _state.IsIndexing = false;
        _state.StatusMessage = _state.IsBackendConnected ? "" : "正在连接后端…";
    }

    /// <summary>输入框文本变化（由 SearchHeader 调用）。</summary>
    public void OnQueryChanged(string text)
    {
        _state.Query = text;
        _pendingQuery = text;
        _resultLimit = 100;

        if (string.IsNullOrWhiteSpace(text))
        {
            _debounce.Stop();
            // 递增 seq，避免清空后旧轮询/旧响应用错 query 的结果刷回列表。
            _searchSeq++;
            CancelSearch();
            _state.Results = Array.Empty<SearchResult>();
            _state.SelectedIndex = -1;
            _state.Mode = PanelMode.Idle;
            _state.IsIndexing = false;
            _state.StatusMessage = "";
            return;
        }

        // 立刻给反馈。旧结果先保留到新响应返回，避免闪空；
        // 过期响应靠 seq + resp.Query 双校验丢弃，不会再把 cla 的结果贴到 clash 上。
        _state.Mode = PanelMode.Results;
        _state.StatusMessage = "搜索中…";

        _debounce.Stop();
        _debounce.Start();
    }

    public void MoveSelection(int delta)
    {
        if (_state.Results.Count == 0) return;
        var next = Math.Clamp(_state.SelectedIndex + delta, 0, _state.Results.Count - 1);
        _state.SelectedIndex = next;
    }

    public async Task ExecuteSelectedAsync()
    {
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
        var i = oneBasedIndex - 1;
        if (i < 0 || i >= _state.Results.Count) return;
        _state.SelectedIndex = i;
        await ExecuteSelectedAsync().ConfigureAwait(true);
    }

    private async Task ShowMoreAsync()
    {
        _resultLimit = 1000;
        await RunSearchAsync(_state.Query, _resultLimit).ConfigureAwait(true);
    }

    private async Task RunSearchAsync(string query, int max)
    {
        if (string.IsNullOrWhiteSpace(query)) return;

        var seq = ++_searchSeq;
        CancelSearch(); // 取消上一次搜索（业务层丢弃；管道层仍会读完响应）
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

            // 过期响应一律丢弃（管道配对已在 PipeClient 内完成，这里只护 UI）。
            if (seq != _searchSeq) return;
            if (cts.IsCancellationRequested) return;
            if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
            // 后端回显 query 也必须一致，双保险防止错位结果上屏。
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

    /// <param name="startPoll">
    /// 是否在 is_indexing 时启动后台轮询。轮询内部回写结果时必须传 false，避免叠多个轮询。
    /// </param>
    private void ApplySearchResponse(SearchResponse resp, string query, int max, int seq, bool startPoll = true)
    {
        var list = new List<SearchResult>(resp.Items.Count + 1);
        list.AddRange(resp.Items);
        if (resp.Items.Count > 0)
            list.Add(SearchResult.More(query));

        // 保留选中项：若仍在列表中则尽量不动，否则落到 0。
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
        _state.Mode = PanelMode.Results;
        _state.IsIndexing = resp.IsIndexing;

        if (resp.IsIndexing)
        {
            // 程序清单可能已出结果，文件索引仍在加载——必须继续轮询补齐文件。
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

    /// <summary>
    /// 索引缓存反序列化可能要数秒。期间程序结果可能已返回（is_indexing=true），
    /// 仍需轮询直到索引就绪，把文件结果补进列表。最多约 15 秒。
    /// </summary>
    private async Task PollUntilReadyAsync(string query, int max, int seq)
    {
        // 0.5s × 30 ≈ 15s，覆盖 ~8s 的缓存加载 + 余量。
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
                // 轮询不走取消令牌：避免与主搜索抢取消时弄乱管道；靠 seq 丢弃即可。
                var resp = await _pipe.SearchAsync(query, max).ConfigureAwait(true);
                if (seq != _searchSeq) return;
                if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
                if (!string.Equals(resp.Query, query, StringComparison.Ordinal)) return;

                if (!resp.IsIndexing)
                {
                    // 索引已就绪：用完整结果（程序+文件）覆盖，不再启动新轮询。
                    ApplySearchResponse(resp, query, max, seq, startPoll: false);
                    return;
                }

                // 仍在加载：若条数变多则刷新列表（例如程序清单稍后就绪）。
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
