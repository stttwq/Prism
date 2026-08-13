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
    /// <summary>窗口激活（G5）。为空表示未装配，窗口结果只显示不可切换。</summary>
    private readonly IWindowActivator? _activator;
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

    /// <summary>
    /// 后端拒绝了本次请求携带的 root（结构化原因）。结果集已经是全局搜索，
    /// 订阅方负责把范围状态切回全局并提示，避免 UI 与实际搜索范围背离。
    /// </summary>
    public event Action<RootRejection>? RootRejected;

    private sealed record SearchCacheEntry(SearchResponse Response, SearchContext Context);

    public SearchViewModel(
        AppState state,
        ISearchClient pipe,
        IDebounceTimerFactory? timerFactory = null,
        ISearchScheduler? scheduler = null,
        IWindowActivator? activator = null)
    {
        _state = state;
        _pipe = pipe;
        _activator = activator;
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
        if (_state.Mode != PanelMode.Results)
            return;
        if (string.IsNullOrWhiteSpace(_state.Query) && string.IsNullOrWhiteSpace(_searchContext.Root))
            return;

        _completeCache = null;
        _generationDebounce.Restart();
    }

    public void SetSearchContext(SearchContext context)
    {
        if (_searchContext.IsEquivalentTo(context)) return;
        _searchContext = context;
        _completeCache = null;
        if (_state.Mode == PanelMode.Actions)
            return;

        if (!string.IsNullOrWhiteSpace(_state.Query))
        {
            if (_state.Mode == PanelMode.Results)
                _generationDebounce.Restart();
            return;
        }

        // Empty box: a host root means "show recent under root"; clearing root
        // (or never having one) returns to Idle — recent windows wait on G5.
        if (!string.IsNullOrWhiteSpace(_searchContext.Root))
        {
            _pendingQuery = _state.Query;
            _resultLimit = InitialResultLimit;
            _state.Mode = PanelMode.Results;
            SetSearchingStatus();
            _debounce.Restart();
        }
        else
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
        }
    }

    /// <summary>当前搜索上下文，供调用方在保留其他字段的前提下改单个维度。</summary>
    public SearchContext SearchContext => _searchContext;

    /// <summary>窗口模式前缀（G5）。只在输入首字符处生效。</summary>
    private const char WindowModePrefix = '>';

    /// <summary>
    /// 解析 `>` 前缀（G5）。模式来自输入文本本身而不是环境状态，所以每次请求都就地推导，
    /// 不把它存进 <see cref="_searchContext"/>——存起来就会和输入框脱节。
    ///
    /// 只认首字符：`a>b` 是普通文件搜索，路径和文件名里出现 `>` 不该改变模式。
    /// </summary>
    private static bool IsWindowQuery(string query) =>
        query.Length > 0 && query[0] == WindowModePrefix;

    /// <summary>去掉 `>` 前缀后真正发给后端的查询。</summary>
    private static string StripWindowPrefix(string query) =>
        IsWindowQuery(query) ? query[1..] : query;

    /// <summary>本次请求的上下文：窗口模式下把 root/filters 一并清掉，窗口不受目录范围约束。</summary>
    private SearchContext ContextFor(string query) =>
        IsWindowQuery(query)
            ? _searchContext with
            {
                Mode = SearchContext.WindowMode,
                Root = null,
                Filters = [],
            }
            : _searchContext;

    /// <summary>
    /// 切换搜索范围（G4）：root 为 null 表示全局。其余上下文字段（排除路径等）保持不变，
    /// 变化会像其他上下文维度一样触发一次重查。
    /// </summary>
    public void SetScopeRoot(string? root) =>
        SetSearchContext(_searchContext with { Root = root });

    private async Task OnGenerationDebounceTickAsync()
    {
        _generationDebounce.Stop();
        if (_state.Mode != PanelMode.Results)
            return;
        if (string.IsNullOrWhiteSpace(_state.Query)
            && string.IsNullOrWhiteSpace(_searchContext.Root))
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
        _state.RenameTarget = null;
        _state.RenameNewName = null;
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

        // 重命名编辑态：输入直接更新新文件名，不触发搜索或动作过滤。
        if (_state.RenameTarget is not null)
        {
            _state.RenameNewName = text;
            return;
        }

        _state.Query = text;
        _pendingQuery = text;

        if (_state.Mode == PanelMode.Actions)
        {
            // 动作过滤：即时，无需防抖太久，但仍走 debounce 避免每键重绑。
            _debounce.Restart();
            return;
        }

        _resultLimit = InitialResultLimit;

        // A bare `>` is a real query: window mode with empty input lists recent windows
        // that still exist. It must not fall through to the Idle branch below.
        if (IsWindowQuery(text))
        {
            _state.Mode = PanelMode.Results;
            SetSearchingStatus();
            _debounce.Restart();
            return;
        }

        if (string.IsNullOrWhiteSpace(text))
        {
            // G4 §4.4: host context (valid root) + empty input → recent items under root
            // via the normal search path. Without a root and outside window mode, keep Idle
            // and do not ask the indexer for a full scan.
            if (!string.IsNullOrWhiteSpace(_searchContext.Root))
            {
                _state.Mode = PanelMode.Results;
                SetSearchingStatus();
                _debounce.Restart();
                return;
            }

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

        if (item.Kind == "window")
        {
            await SwitchToWindowAsync(item).ConfigureAwait(true);
            return;
        }

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

    /// <summary>
    /// 切换到窗口（G5）。顺序是硬要求：**先激活、再隐藏**。
    ///
    /// 先隐藏会让本进程失去前台身份，随后的 `SetForegroundWindow` 就会被系统降级成任务栏
    /// 闪烁——看起来像"没反应"。所以激活必须发生在本窗口仍是前台的时候。
    ///
    /// 失败时保留 UI、不写成功历史、不结束目标进程。
    /// </summary>
    private async Task SwitchToWindowAsync(SearchResult item)
    {
        if (_activator is null)
        {
            _state.StatusMessage = "窗口切换不可用";
            return;
        }

        WindowHandleInfo window;
        try
        {
            window = await _pipe.ResolveWindowAsync(item.ExecutionTarget).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            // Window closed, handle recycled, or the list went stale.
            _state.StatusMessage = "切换失败：" + ShortMsg(ex);
            return;
        }

        // Activate first, while this window is still the foreground process.
        if (!_activator.TryActivate(window))
        {
            _state.StatusMessage = "无法切换到该窗口";
            return;
        }

        HideRequested?.Invoke();

        try
        {
            await _pipe.RecordWindowSwitchAsync(item.ExecutionTarget).ConfigureAwait(true);
        }
        catch
        {
            // The switch already succeeded; a failed history write must not be reported as
            // a failed switch.
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
        _state.RenameTarget = null;
        _state.RenameNewName = null;
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
        // 重命名编辑态：Enter 提交重命名，不执行动作面板选中项。
        if (_state.RenameTarget is not null)
        {
            var newName = _state.RenameNewName ?? "";
            await CommitRenameAsync(newName).ConfigureAwait(true);
            return;
        }

        if (_state.Mode != PanelMode.Actions) return;
        var action = _state.SelectedAction;
        var target = _state.ActionTarget;
        if (action is null || target is null) return;

        await RunActionOnAsync(target, action).ConfigureAwait(true);
    }

    /// <summary>
    /// 动作成功后隐藏窗口的动作集合。不在集合中的动作（rename/move_to/
    /// recycle/delete_permanent/zip）保留窗口，等待 generation 更新或有界超时后重搜。
    /// </summary>
    private static readonly HashSet<string> HideAfterSuccessActions = new()
    {
        "open_folder", "copy", "cut", "copy_path", "copy_app_path",
        "properties", "app_properties", "open_with", "locate_app", "run_as_admin",
    };

    /// <summary>执行指定目标上的动作，保持所有入口的成功隐藏和错误提示一致。</summary>
    public async Task RunActionOnAsync(SearchResult target, ActionItem action)
    {
        if (action.IsSectionHeader || string.IsNullOrEmpty(action.Id)) return;
        if (target.Kind is not ("app" or "file" or "folder") || string.IsNullOrEmpty(target.ExecuteId))
            return;

        // rename 需要内联编辑新文件名，不直接发送 IPC。
        if (action.Id == "rename")
        {
            _state.RenameTarget = target;
            _state.RenameNewName = System.IO.Path.GetFileName(target.ExecuteId);
            _state.StatusMessage = "";
            return;
        }

        // copy_to/move_to：弹出文件夹选择器。
        if (action.Id is "copy_to" or "move_to")
        {
            var destination = PickDestinationFolder();
            if (destination is null)
            {
                // 用户取消了，不报错。
                return;
            }
            try
            {
                await _pipe.RunActionAsync(
                    target.ExecutionTarget,
                    action.Id,
                    new ActionArgs { Destination = destination },
                    CancellationToken.None).ConfigureAwait(true);
                _state.StatusMessage = action.Id == "copy_to" ? "复制完成，正在刷新…" : "移动完成，正在刷新…";
            }
            catch (Exception ex)
            {
                _state.StatusMessage = "动作失败：" + ShortMsg(ex);
            }
            await RefreshAsync();
            return;
        }

        try
        {
            await _pipe.RunActionAsync(target.ExecutionTarget, action.Id).ConfigureAwait(true);
            // mutation 动作（recycle/delete_permanent/zip）保留窗口等待 generation 刷新；
            // 其余成功动作隐藏 Prism。
            if (HideAfterSuccessActions.Contains(action.Id))
            {
                HideRequested?.Invoke();
            }
            else
            {
                _state.StatusMessage = "操作完成，正在刷新…";
                await RefreshAsync();
            }
        }
        catch (Exception ex)
        {
            _state.StatusMessage = "动作失败：" + ShortMsg(ex);
        }
    }

    /// <summary>弹出文件夹选择对话框，返回选中的目录路径或 null（用户取消）。</summary>
    private string? PickDestinationFolder()
    {
        using var dialog = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = "选择目标文件夹",
            ShowNewFolderButton = true,
        };
        return dialog.ShowDialog() == System.Windows.Forms.DialogResult.OK
            ? dialog.SelectedPath
            : null;
    }

    /// <summary>用当前查询重新搜索，用于 mutation 动作成功后刷新结果。</summary>
    public async Task RefreshAsync()
    {
        if (_state.Mode == PanelMode.Actions)
        {
            // 离开动作面板，回到结果模式。
            LeaveActions();
        }
        if (!string.IsNullOrWhiteSpace(_state.Query) || !string.IsNullOrWhiteSpace(_searchContext.Root))
        {
            _resultLimit = InitialResultLimit;
            // 清掉 prefix cache，强制不使用本地缓存。
            _completeCache = null;
            // 清掉旧结果，显示刷新中状态。
            _state.Results = Array.Empty<SearchResult>();
            _state.StatusMessage = "正在刷新…";
            // 等 indexer 的 USN watcher 消化文件变更。
            // 先等 1.5 秒做一次搜索，后续 generation 通知到达时会自动再刷一次。
            await Task.Delay(1500).ConfigureAwait(true);
            await RunSearchAsync(_state.Query, _resultLimit).ConfigureAwait(true);
        }
    }

    /// <summary>提交重命名：将新文件名发送给 broker。</summary>
    public async Task CommitRenameAsync(string newName)
    {
        var target = _state.RenameTarget;
        if (target is null || string.IsNullOrWhiteSpace(newName)) return;

        try
        {
            await _pipe.RunActionAsync(
                target.ExecutionTarget,
                "rename",
                new ActionArgs { NewName = newName.Trim() },
                CancellationToken.None).ConfigureAwait(true);
            _state.RenameTarget = null;
            _state.RenameNewName = null;
            _state.StatusMessage = "重命名完成，正在刷新…";
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            // 保留编辑态，显示错误。
            _state.StatusMessage = "重命名失败：" + ShortMsg(ex);
        }
    }

    /// <summary>取消重命名，清空编辑态。</summary>
    public void CancelRename()
    {
        _state.RenameTarget = null;
        _state.RenameNewName = null;
        _state.StatusMessage = "";
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
        var context = ContextFor(query);
        // The broker never sees the `>`; it echoes back the stripped query, so every
        // comparison against the echo below has to use this form too.
        var wireQuery = StripWindowPrefix(query);
        // Empty query is only meaningful with a host root (recent under root) or in window
        // mode (recent windows). Otherwise the UI stays Idle and never reaches here; still
        // guard for context changes.
        var isEmptyQuery = string.IsNullOrWhiteSpace(wireQuery);
        if (isEmptyQuery
            && !context.IsWindowMode
            && string.IsNullOrWhiteSpace(context.Root))
        {
            return;
        }

        var seq = ++_searchSeq;
        CancelSearch();
        // Window results are per-enumeration: their tokens expire on the next publish, so
        // they must never be served from the prefix cache.
        if (!isEmptyQuery && !context.IsWindowMode && TryFilterCompleteCache(query, out var cached))
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

            var resp = await _pipe.SearchAsync(wireQuery, max, context, cts.Token).ConfigureAwait(true);

            if (seq != _searchSeq) return;
            if (cts.IsCancellationRequested) return;
            // Staleness is judged against the box, which still holds the `>`.
            if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
            // Broker echoes the request query; for empty input accept either "" or the
            // whitespace the box still holds, as long as both sides trim empty.
            if (!QueryMatchesResponse(wireQuery, resp.Query)) return;

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
            && !string.IsNullOrWhiteSpace(query)
            // Window tokens are only valid inside the enumeration that minted them, so a
            // window response can never back local prefix filtering. Today's broker also
            // sends no IndexGeneration for window mode, which would block the cache on its
            // own — but relying on that alone would make this safety incidental.
            // Window tokens are only valid inside the enumeration that minted them, so a
            // window response must never back local prefix filtering.
            //
            // Three independent things already prevent it: window responses carry no
            // IndexGeneration, the cached raw query keeps the `>` while the echo does not
            // (so the prefix check cannot match), and the context Mode differs. This guard
            // is deliberate redundancy — it states the intent so a later change to any of
            // those three does not silently start caching volatile tokens. A unit test
            // cannot isolate it for exactly that reason.
            && !IsWindowQuery(query)
            && !resp.IsIndexing
            && string.IsNullOrWhiteSpace(resp.IndexError)
            && !resp.IsTruncated
            && resp.IndexGeneration.HasValue
            && resp.RootRejection is null
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
        else if (IsWindowQuery(query))
        {
            _state.StatusMessage = string.IsNullOrEmpty(StripWindowPrefix(query))
                ? "没有最近使用过的窗口"
                : "没有匹配的窗口";
        }
        else if (string.IsNullOrWhiteSpace(query)
                 && !string.IsNullOrWhiteSpace(_searchContext.Root))
        {
            // Host empty input with no recent-under-root hits: keep the panel open
            // without the generic "无匹配结果" copy used for typed searches.
            _state.StatusMessage = "";
        }
        else
        {
            _state.StatusMessage = "无匹配结果";
        }

        if (resp.RootRejection is not null)
        {
            // 结果已经是全局的，范围状态必须立刻跟上，绝不能停留在「当前目录」。
            _completeCache = null;
            RootRejected?.Invoke(resp.RootRejection.Value);
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
                    StripWindowPrefix(query), max, ContextFor(query)).ConfigureAwait(true);
                if (seq != _searchSeq) return;
                if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;
                // Echo carries no `>`; compare against the stripped form.
                if (!string.Equals(resp.Query, StripWindowPrefix(query), StringComparison.Ordinal))
                    return;

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

    private static bool QueryMatchesResponse(string requested, string echoed)
    {
        if (string.Equals(requested, echoed, StringComparison.Ordinal))
            return true;
        // Empty-query recent list: both sides are blank after trim.
        return string.IsNullOrWhiteSpace(requested) && string.IsNullOrWhiteSpace(echoed);
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
