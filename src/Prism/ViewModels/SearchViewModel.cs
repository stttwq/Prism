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
    /// <summary>在线联想服务（G8）。为空表示未装配，网页模式不发送联想请求。</summary>
    private readonly ISuggestionService? _suggestions;

    /// <summary>P4a: 目标文件夹选择器（copy_to/move_to），可注入假实现测该分支。</summary>
    private readonly IFolderPicker _folderPicker;
    /// <summary>当前引擎列表（G8 网页模式检测用），由设置更新时刷新。</summary>
    private IReadOnlyList<WebEngine> _webEngines = Settings.DefaultEngines();
    /// <summary>在线联想开关（G8），默认关闭。</summary>
    private bool _suggestionsEnabled;
    private CancellationTokenSource? _searchCts;
    /// <summary>P4c: 网页联想的发起/取消/身份治理抽出到了 WebSearchCoordinator。</summary>
    private readonly WebSearchCoordinator _webSuggestions;
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
    /// <summary>mutation 完成后等待 generation 变化的信号源。</summary>
    private TaskCompletionSource? _mutationGenerationSignal;
    /// <summary>暂存区（2026-08-22 计划阶段三）：工作集名字召回注入用。
    /// 为空表示未装配，不注入合成行。</summary>
    private readonly StagingArea? _staging;

    public AppState State => _state;

    /// <summary>执行成功后请求隐藏窗口（由 SearchWindow 订阅）。</summary>
    public event Action? HideRequested;

    /// <summary>
    /// 复审 M1（2026-08-21）：copy_to/move_to 的目标文件夹选择是模态对话框，
    /// 打开即夺走前台。键盘路径（动作面板 Enter）此前没有失活守卫——窗口在
    /// 对话框后面自行隐藏并清空查询。窗口侧订阅这对事件，在弹窗期间挂起
    /// 失活隐藏（与右键菜单路径的守卫同一纪律）。
    /// </summary>
    public event Action? ModalPickStarted;
    public event Action? ModalPickEnded;

    /// <summary>
    /// 查询从非空变为空时请求释放空闲内存（由 SearchWindow 订阅）。
    /// 清空查询会丢弃结果引用，但 GC 只在窗口隐藏时跑——这里让窗口在 idle 时
    /// 额外做一次轻量回收，避免反复搜索后工作集只涨不降。
    /// </summary>
    public event Action? IdleMemoryReleaseRequested;

    /// <summary>
    /// 后端拒绝了本次请求携带的 root（结构化原因）。结果集已经是全局搜索，
    /// 订阅方负责把范围状态切回全局并提示，避免 UI 与实际搜索范围背离。
    /// </summary>
    public event Action<RootRejection>? RootRejected;

    /// <summary>
    /// 工作集召回（阶段三）：Enter 落在 workset 合成行时触发，名字由订阅方
    /// （SearchWindow → StagingArea.LoadWorkset）载入暂存区。窗口不隐藏——
    /// 载入后通常要继续拖出使用。
    /// </summary>
    public event Action<string>? WorksetRecallRequested;

    private sealed record SearchCacheEntry(SearchResponse Response, SearchContext Context);

    public SearchViewModel(
        AppState state,
        ISearchClient pipe,
        IDebounceTimerFactory? timerFactory = null,
        ISearchScheduler? scheduler = null,
        IWindowActivator? activator = null,
        ISuggestionService? suggestions = null,
        IFolderPicker? folderPicker = null,
        StagingArea? staging = null)
    {
        _state = state;
        _pipe = pipe;
        _activator = activator;
        _suggestions = suggestions;
        _staging = staging;
        // P4a: 缺省保持 WinForms 对话框（照抄 ISuggestionService 的可选注入先例）。
        _folderPicker = folderPicker ?? new WinFormsFolderPicker();
        // P4c: 联想结果必须回 UI 线程写 Results；无 Application（单元测试）或已在
        // UI 线程时同步执行，否则 BeginInvoke 排队——与抽取前的行为一致。
        _webSuggestions = new WebSearchCoordinator(suggestions, action =>
        {
            var dispatcher = System.Windows.Application.Current?.Dispatcher;
            if (dispatcher is null || dispatcher.CheckAccess())
            {
                action();
                return;
            }
            dispatcher.BeginInvoke(action);
        });
        timerFactory ??= new DispatcherDebounceTimerFactory();
        _scheduler = scheduler ?? new SearchScheduler();
        _debounce = timerFactory.Create(
            TimeSpan.FromMilliseconds(50), () => _ = OnDebounceTickAsync());
        _generationDebounce = timerFactory.Create(
            TimeSpan.FromMilliseconds(100), () => _ = OnGenerationDebounceTickAsync());
    }

    /// <summary>更新网页引擎列表和联想开关（G8）。设置保存后由 App 调用。</summary>
    public void UpdateWebSettings(IReadOnlyList<WebEngine> engines, bool suggestionsEnabled)
    {
        _webEngines = engines.Count > 0 ? engines : Settings.DefaultEngines();
        _suggestionsEnabled = suggestionsEnabled;
    }

    /// <summary>Schedules one refresh through the existing broker search path.</summary>
    public void OnIndexGenerationChanged()
    {
        if (_state.Mode != PanelMode.Results)
            return;
        if (string.IsNullOrWhiteSpace(_state.Query) && string.IsNullOrWhiteSpace(_searchContext.Root))
            return;

        _completeCache = null;
        // C-D10: 只发信号不清字段——清空归属权在等待它的 RefreshAsync（CAS 只清自己
        // 那份）。这里置 null 会把并发的下一次 RefreshAsync 刚装上的新信号一并清掉，
        // 逼它走 3 秒超时路径（结果刷新变慢）。
        _mutationGenerationSignal?.TrySetResult();
        _generationDebounce.Restart();
    }

    public void SetSearchContext(SearchContext context)
    {
        if (_searchContext.IsEquivalentTo(context)) return;
        _searchContext = context;
        _completeCache = null;
        // 复审 L4（2026-08-21）：上下文维度变了（root/filters/mode），在飞的
        // 旧上下文搜索响应会通过全部既有 staleness 守卫（seq/查询文本/回显都
        // 没变）被误应用——约一个防抖+搜索时延内新标签下显示旧范围结果。
        // 与空查询分支同一纪律：bump seq + 取消在飞请求。
        _searchSeq++;
        CancelSearch();
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
            CancelSuggestions();
            _state.Results = Array.Empty<SearchResult>();
            _state.SelectedIndex = -1;
            _state.Mode = PanelMode.Idle;
            _state.IsIndexing = false;
            _state.IsWebMode = false;
            _state.StatusMessage = "";
        }
    }

    /// <summary>当前搜索上下文，供调用方在保留其他字段的前提下改单个维度。</summary>
    public SearchContext SearchContext => _searchContext;

    /// <summary>窗口模式前缀（G5）。只在输入首字符处生效。</summary>
    private const char WindowModePrefix = '>';

    /// <summary>
    /// 绝对路径形查询（P1，第一轮 bug 修复）：盘符+分隔符（E:\… / e:/…）或
    /// UNC 前缀。外层包裹引号先剥（Explorer「复制文件地址」形态）。与 broker
    /// 侧 is_absolute_path_query 同规则——broker 把这类查询当路径解析，前端
    /// 据此跳过前缀缓存（见 RunSearchAsync 注释）。
    /// </summary>
    internal static bool IsAbsolutePathQuery(string query)
    {
        var body = query.Trim().AsSpan();
        if (body.Length >= 2 && body[0] == '"' && body[^1] == '"')
            body = body[1..^1].Trim();
        if (body.Length >= 3
            && char.IsAsciiLetter(body[0])
            && body[1] == ':'
            && (body[2] == '\\' || body[2] == '/'))
            return true;
        return body.StartsWith(@"\\", StringComparison.Ordinal)
            || body.StartsWith("//", StringComparison.Ordinal);
    }

    /// <summary>
    /// H7（全仓复审 2026-08-22）：查询里含 ext:/path: 过滤 token（与 broker
    /// parse_query 的 known_prefixes 同口径，宽松版：不校验值合法性——凡是
    /// 形似过滤词的查询一律不进/清前缀缓存，宁可少缓存不可错过滤）。broker
    /// 会把这些 token 从名字查询里剥掉，但回显仍是原文；前缀缓存按 Title
    /// 子串过滤，标题永远不会包含 "ext:" 文本——下一击键会把整页结果滤空。
    /// </summary>
    internal static bool HasFilterToken(string query)
    {
        foreach (var token in query.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries))
        {
            if (token.StartsWith("ext:", StringComparison.OrdinalIgnoreCase)
                || token.StartsWith("path:", StringComparison.OrdinalIgnoreCase))
                return true;
        }
        return false;
    }

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
        CancelSuggestions();
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
        _state.IsWebMode = false;
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
            CancelSuggestions();
            // 之前有结果时，清空查询丢弃了大量引用；请求窗口在 idle 时做一次轻量
            // 回收，避免反复搜索后工作集只涨不降（不在 hot path 上同步阻塞）。
            if (_state.Results.Count > 0)
                IdleMemoryReleaseRequested?.Invoke();
            _state.Results = Array.Empty<SearchResult>();
            _state.SelectedIndex = -1;
            _state.Mode = PanelMode.Idle;
            _state.IsIndexing = false;
            _state.IsWebMode = false;
            _state.StatusMessage = "";
            return;
        }

        // 小问题 Q2：进入网页模式即置位（不等防抖搜索），离开（删掉关键词）即复位，
        // 窗口据此隐藏"当前目录"前缀。
        _state.IsWebMode = WebModeDetector.TryDetect(text, _webEngines) is not null;
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

        // 工作集合成行（阶段三）：Enter = 载入暂存区（不隐藏窗口、不经 broker）。
        if (item.Kind == "workset")
        {
            WorksetRecallRequested?.Invoke(item.ExecuteId);
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
            await _pipe.ExecuteAsync(item.ExecutionTarget, _state.Query).ConfigureAwait(true);
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
            await _pipe.RecordWindowSwitchAsync(item.ExecutionTarget, _state.Query).ConfigureAwait(true);
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
        // workset 是前端合成行，没有可定位的文件系统对象。
        if (item is null || item.Kind is "more" or "web" or "workset") return;
        if (string.IsNullOrEmpty(item.ExecuteId)) return;

        try
        {
            await _pipe.RevealAsync(item.ExecutionTarget, _state.Query).ConfigureAwait(true);
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
        // 先恢复 Query 再切 Mode：窗口侧 ApplyState 在「离开动作面板」转换时把
        // Header 拉回 State.Query，若 Mode 先变，转换瞬间读到的 Query 还是
        // 动作过滤/重命名留下的旧文本，之后 Query 恢复不再触发回写。
        _state.Query = _queryBeforeActions;
        _pendingQuery = _queryBeforeActions;
        _state.Mode = PanelMode.Results;
        _state.Actions = Array.Empty<ActionItem>();
        _state.SelectedActionIndex = -1;
        _state.ActionTarget = null;
        _allActions = Array.Empty<ActionItem>();
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
            ModalPickStarted?.Invoke();
            string? destination;
            try
            {
                destination = PickDestinationFolder();
            }
            finally
            {
                ModalPickEnded?.Invoke();
            }
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
                    _state.Query,
                    CancellationToken.None).ConfigureAwait(true);
                _state.StatusMessage = action.Id == "copy_to" ? "复制完成，正在刷新…" : "移动完成，正在刷新…";
            }
            catch (Exception ex)
            {
                _state.StatusMessage = ActionErrorMessage(ex, action.Id);
            }
            await RefreshAsync();
            return;
        }

        try
        {
            await _pipe.RunActionAsync(target.ExecutionTarget, action.Id, _state.Query).ConfigureAwait(true);
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
            _state.StatusMessage = ActionErrorMessage(ex, action.Id);
        }
    }

    /// <summary>
    /// FRESH-AUDIT-2 F7: 动作超时 ≠ 未执行——broker 可能已完成删除/移动而响应丢失，
    /// 用户照旧文案重试会重复执行。mutation 类超时改报"结果未知"，
    /// 非 mutation（open/locate 等）超时仍按普通失败处理（重试无副作用）。
    /// </summary>
    private static string ActionErrorMessage(Exception ex, string actionId)
    {
        const string timeoutMarker = "响应超时";
        var isMutation = !HideAfterSuccessActions.Contains(actionId) && actionId != "rename";
        if (isMutation && ex is IOException && ex.Message.Contains(timeoutMarker))
            return $"动作超时，结果未知：{actionId} 可能已执行，请核实文件状态后再决定是否重试";
        return "动作失败：" + ShortMsg(ex);
    }

    /// <summary>弹出文件夹选择对话框，返回选中的目录路径或 null（用户取消）。P4a: 转发给注入的 IFolderPicker。</summary>
    private string? PickDestinationFolder() => _folderPicker.PickFolder("选择目标文件夹");

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
            // L22（全仓复审 2026-08-22）：入口捕获查询——等待 generation/超时期间用户
            // 可能继续打字，_state.Query 已是新文本；刷新该刷的是触发它的那次查询，
            // 而防抖路径自己去读最新值，两者互不冒领。
            var query = _state.Query;
            _resultLimit = InitialResultLimit;
            _completeCache = null;
            _state.Results = Array.Empty<SearchResult>();
            _state.StatusMessage = "正在刷新…";

            // 等 indexer 的 USN watcher 消化文件变更：generation 变化或 3 秒超时。
            // 超时只提示索引尚未刷新，不标为失败。
            // C-D10+P4b: 信号局部持有、归还只清自己那份（CAS）——第二次 RefreshAsync
            // 已换上新信号时，第一次的续体不得把它清掉，否则第二次只能等满超时。
            // 超时走 _scheduler.Delay 缝（P4b）：测试可快进/暂停，不再真睡 3 秒。
            var signal = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _mutationGenerationSignal = signal;
            var generationTask = signal.Task;
            var timeoutTask = _scheduler.Delay(TimeSpan.FromMilliseconds(3000));
            var completed = await Task.WhenAny(generationTask, timeoutTask).ConfigureAwait(true);
            Interlocked.CompareExchange(ref _mutationGenerationSignal, null, signal);

            if (completed == timeoutTask)
            {
                // generation 超时：先搜索一次（用当前索引），再提示索引尚未刷新。
                await RunSearchAsync(query, _resultLimit).ConfigureAwait(true);
                _state.StatusMessage = "索引尚未刷新，结果可能不完整";
            }
            else
            {
                // generation 变化：OnIndexGenerationChanged 已经触发了一次搜索，
                // 但 generation debounce 可能在 Mode != Results 时被跳过，所以再搜一次。
                // L22：generation 已到手，武装中的 debounce 只会再搜一次旧查询——停掉。
                _generationDebounce.Stop();
                await RunSearchAsync(query, _resultLimit).ConfigureAwait(true);
            }
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
                _state.Query,
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
        // 清除选中，使 ApplySearchResponse 不会尝试保留 "more" 行的选中——
        // 后者会让 SelectedIndex 落在列表末尾（展开后仍有截断时），用户看到的
        // 是滚到底部而非从顶部开始浏览。
        _state.SelectedIndex = -1;
        await RunSearchAsync(_state.Query, _resultLimit).ConfigureAwait(true);
    }

    private async Task RunSearchAsync(string query, int max)
    {
        // Bug 4: 裸网址直开——普通搜索框输入 example.com / https://… / localhost
        // 即显示一行"打开 …"结果，Enter 经 broker ShellExecuteExW 打开浏览器。
        // 在网页关键词检测之前、窗口模式之后：>example.com 仍是窗口模式，不走此处。
        // 不设 IsWebMode（非网页搜索模式，无联想）。
        if (!IsWindowQuery(query))
        {
            var directUrl = WebModeDetector.TryGetDirectUrlForSearchBox(query);
            if (directUrl is not null)
            {
                _state.IsWebMode = false;
                RunDirectUrlSearch(query, directUrl);
                return;
            }
        }

        // G8: web mode — detect web keyword before pipe search. The broker still produces
        // web results in AllMode, but the dedicated web mode isolates them: only 1 direct
        // result + up to 5 suggestions, no file/app/window mixing.
        var webMode = WebModeDetector.TryDetect(query, _webEngines);
        _state.IsWebMode = webMode is not null;
        if (webMode is not null)
        {
            // 同步方法（G1：原 async 无 await，CS1998 伪装）：直接结果本就同步产生，
            // 联想是 coordinator 内部的 fire-and-forget。
            RunWebSearch(query, webMode);
            return;
        }

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
        // 常规搜索路径也必须取消遗留的 web 联想 Task.Run——从 web mode 切到非 web mode 时，
        // 旧的 Phase B 联想仍在飞行中，不取消它会通过 staleness 守卫后覆盖文件搜索结果。
        CancelSuggestions();
        // 路径查询（P1，第一轮 bug 修复）不得走前缀缓存：缓存的键是上一次的
        // 完整查询（如 "E"），路径增长（"E:\foo"）是其前缀，但语义是"换了一个
        // 路径"，按标题子串过滤缓存必得空集——清缓存，直接发 broker。
        // 窗口结果按枚举逐次签发，也绝不能从前缀缓存供给。
        // H7：ext:/path: 过滤词同理——broker 剥掉 token 后按名字过滤，回显却是
        // 原文，Title.Contains("ext:pdf") 永远为假，下一击键整页滤空。
        if (!isEmptyQuery
            && !context.IsWindowMode
            && (IsAbsolutePathQuery(query) || HasFilterToken(query)))
        {
            _completeCache = null;
        }
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
        // 工作集名字召回（阶段三）：查询与工作集名完全一致时在列表头注入合成行。
        // SearchResult.More 是前端合成行的先例——零协议改动，broker 无感知。
        var recall = BuildWorksetRecallRow(query);
        if (recall is not null)
            list.Insert(0, recall);

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
            // H7：过滤词查询不入缓存（TryFilterCompleteCache 的子串过滤对它们必然失真）。
            && !HasFilterToken(query)
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

        var prev = _state.SelectedResult;
        _state.Results = list;
        var keep = -1;
        if (prev is not null)
        {
            for (var i = 0; i < list.Count; i++)
            {
                // "more" 行的 ExecuteId 为空，不能按 ExecuteId 匹配；按 Kind 匹配，
                // 否则 generation 变化触发的重搜会把 "more" 选中丢失，跳回第 0 行。
                if (prev.Kind == "more" && list[i].Kind == "more"
                    || !string.IsNullOrEmpty(prev.ExecuteId) && list[i].ExecuteId == prev.ExecuteId)
                {
                    keep = i; break;
                }
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
    /// <summary>查询完全命中工作集名（忽略大小写）时构造合成行；否则 null。
    /// ExecuteId=工作集名（Enter 事件回传用），RowKey 与查询无关保稳定容器。</summary>
    private SearchResult? BuildWorksetRecallRow(string query)
    {
        if (_staging is null || string.IsNullOrWhiteSpace(query)) return null;
        foreach (var ws in _staging.Worksets)
        {
            if (!string.Equals(ws.Name, query, StringComparison.OrdinalIgnoreCase))
                continue;
            return new SearchResult(
                Kind: "workset",
                Title: ws.Name,
                Subtitle: $"工作集 · {ws.Paths.Count} 个文件 · Enter 载入暂存区",
                ExecuteId: ws.Name,
                MatchSpans: [])
            {
                RowKey = "workset:" + ws.Name,
            };
        }
        return null;
    }

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
        // B6（AUDIT-4 批次C）：轮询请求带上发起搜索的取消令牌——broker wedge
        // 时轮询的 SearchAsync 只在等 _ioLock 阶段被取消（写出后仍按配对读
        // 纪律读完响应再丢弃），不再占住通道锁到 8s 读超时。
        var pollToken = _searchCts?.Token ?? CancellationToken.None;
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
                    StripWindowPrefix(query), max, ContextFor(query), pollToken).ConfigureAwait(true);
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
        // G3: 只 Cancel 不 Dispose——在途请求的配对读仍持有该 token
        // （SendAsync 完成读后要 ThrowIfCancellationRequested），Dispose 会让它
        // 撞上 ObjectDisposedException。CTS 无内部定时器，交给 GC 即可。
        try { _searchCts?.Cancel(); } catch { /* ignore */ }
        _searchCts = null;
    }

    /// <summary>取消正在进行的联想请求（G8）。查询变化时调用。P4c: 转发给 coordinator。</summary>
    private void CancelSuggestions() => _webSuggestions.CancelPending();

    /// <summary>
    /// 网页模式搜索（G8）。直接结果同步产生，联想异步获取并追加。
    /// 直接结果不等待网络；联想 800ms 超时静默放弃。
    /// </summary>
    private void RunWebSearch(string query, WebModeResult webMode)
    {
        var seq = ++_searchSeq;
        CancelSearch();
        CancelSuggestions();
        _completeCache = null;

        // 构造直接提交结果：首行显示提交原查询。网址类查询词直接打开（小问题 Q3）。
        var directOpenUrl = WebModeDetector.TryGetDirectUrl(webMode.QueryTerms);
        var directUrl = directOpenUrl ?? WebModeDetector.BuildUrl(webMode.UrlTemplate, webMode.QueryTerms);
        var directTitle = directOpenUrl is not null
            ? $"打开 {webMode.QueryTerms.Trim()}"
            : string.IsNullOrEmpty(webMode.QueryTerms)
                ? $"在 {webMode.EngineName} 中搜索"
                : $"在 {webMode.EngineName} 中搜索：{webMode.QueryTerms}";
        var directResult = new SearchResult(
            Kind: "web",
            Title: directTitle,
            Subtitle: directUrl,
            ExecuteId: directUrl,
            MatchSpans: WebSearchCoordinator.BuildWebMatchSpans(directTitle, webMode.QueryTerms))
        {
            Target = new ActionTarget("web", directUrl),
            // 首行身份与查询词无关：标题/URL 每按一键都变，但它始终是"同一行"。
            // 不给稳定键，ResultList 会删旧行插新行，容器重建导致图标闪烁。
            RowKey = "web:direct:" + webMode.EngineName,
        };

        // 立即显示直接结果，不等待网络。
        var list = new List<SearchResult> { directResult };
        if (seq != _searchSeq) return;
        if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;

        _state.Results = list;
        _state.SelectedIndex = 0;
        _state.Mode = PanelMode.Results;
        _state.IsIndexing = false;
        _state.StatusMessage = _suggestionsEnabled && webMode.IsBuiltIn
            ? ""  // 联想加载中，但不显示干扰性等待文案
            : (string.IsNullOrEmpty(webMode.QueryTerms)
                ? $"按 Enter 在 {webMode.EngineName} 中搜索"
                : "");

        // 只有内置引擎 + 联想开关开启 + 有查询词 + 非网址直开时才发请求
        // （网址本身没有搜索联想的意义）。
        if (directOpenUrl is not null)
            return;
        if (!_suggestionsEnabled || !webMode.IsBuiltIn || string.IsNullOrWhiteSpace(webMode.QueryTerms))
            return;
        if (_suggestions is null)
            return;

        // P4c: 联想的发起/取消/迟到丢弃全部在 WebSearchCoordinator 里。
        // stillCurrent 三重身份（搜索序号 + 当前查询）在应用前后各查一次。
        var seqAtFetch = seq;
        var queryAtFetch = query;
        _webSuggestions.FetchSuggestions(
            webMode,
            stillCurrent: () => seqAtFetch == _searchSeq
                && string.Equals(queryAtFetch, _state.Query, StringComparison.Ordinal),
            apply: suggestions =>
            {
                _state.Results = WebSearchCoordinator.BuildWebRows(directResult, suggestions, webMode);
                _state.StatusMessage = "";
            });
    }

    /// <summary>
    /// Bug 4: 搜索框裸网址直开。镜像 <see cref="RunWebSearch"/> 的单行直接结果，
    /// 但不发联想请求（网址无搜索联想意义），不设 IsWebMode。
    /// 执行路径复用 ExecuteSelectedAsync → _pipe.ExecuteAsync（kind="web"）→
    /// broker ShellExecuteExW → 默认浏览器打开。
    /// </summary>
    private void RunDirectUrlSearch(string query, string url)
    {
        var seq = ++_searchSeq;
        CancelSearch();
        CancelSuggestions();
        _completeCache = null;

        var terms = query.Trim();
        var directResult = new SearchResult(
            Kind: "web",
            Title: $"打开 {terms}",
            Subtitle: url,
            ExecuteId: url,
            MatchSpans: WebSearchCoordinator.BuildWebMatchSpans($"打开 {terms}", terms))
        {
            Target = new ActionTarget("web", url),
            RowKey = "web:direct:url",
        };

        if (seq != _searchSeq) return;
        if (!string.Equals(query, _state.Query, StringComparison.Ordinal)) return;

        _state.Results = new List<SearchResult> { directResult };
        _state.SelectedIndex = 0;
        _state.Mode = PanelMode.Results;
        _state.IsIndexing = false;
        _state.StatusMessage = "";
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
