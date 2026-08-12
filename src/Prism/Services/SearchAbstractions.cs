using Prism.Models;

namespace Prism.Services;

public sealed record SearchFilterOption(string Field, string Value);

public sealed record SearchContext(
    string Mode,
    string? Root,
    IReadOnlyList<SearchFilterOption> Filters,
    int SortVersion)
{
    /// <summary>默认模式：文件/程序/网页混排。线路上省略该值。</summary>
    public const string AllMode = "all";

    /// <summary>窗口模式（G5）：只搜可切换的顶层窗口，不与其他类型混排。</summary>
    public const string WindowMode = "window";

    public static SearchContext Default { get; } = new(AllMode, null, [], 1);

    public bool IsWindowMode =>
        string.Equals(Mode, WindowMode, StringComparison.Ordinal);

    public bool IsEquivalentTo(SearchContext other) =>
        string.Equals(Mode, other.Mode, StringComparison.Ordinal)
        && string.Equals(Root, other.Root, StringComparison.Ordinal)
        && SortVersion == other.SortVersion
        && Filters.SequenceEqual(other.Filters);
}

/// <summary>
/// broker 复核通过的窗口句柄（G5）。句柄只在这一步离开 broker，WPF 必须在真正激活前
/// 再复核一次，压掉「复核 → 激活」之间窗口关闭或句柄被复用的竞态。
/// </summary>
public sealed record WindowHandleInfo(IntPtr Handle, uint Pid, string Title, bool IsMinimized);

public interface ISearchClient
{
    bool IsConnected { get; }
    Task StartAsync(CancellationToken ct = default);
    Task<SearchResponse> SearchAsync(
        string query,
        int max,
        SearchContext context,
        CancellationToken ct = default);
    Task ExecuteAsync(ActionTarget target, CancellationToken ct = default);
    Task RevealAsync(ActionTarget target, CancellationToken ct = default);
    Task<IReadOnlyList<ActionItem>> GetActionsAsync(ActionTarget target, CancellationToken ct = default);
    Task RunActionAsync(ActionTarget target, string action, CancellationToken ct = default);
    Task RunActionAsync(ActionTarget target, string action, ActionArgs args, CancellationToken ct = default);

    /// <summary>G5：把枚举 token 换成已复核的句柄，交给前台进程激活。</summary>
    Task<WindowHandleInfo> ResolveWindowAsync(ActionTarget target, CancellationToken ct = default);

    /// <summary>G5：激活成功后回报，由 broker 写窗口历史。失败路径不得调用。</summary>
    Task RecordWindowSwitchAsync(ActionTarget target, CancellationToken ct = default);
}

/// <summary>
/// 前台窗口激活（G5）。放在接口后面，使复核失败、恢复最小化、激活被拒三条路径可测，
/// 不依赖真实窗口。
///
/// 激活必须由前台进程完成：`SetForegroundWindow` 只对前台进程（或刚收到输入的进程）
/// 生效，所以这件事留在 WPF，不能交给后台的 broker。
/// </summary>
public interface IWindowActivator
{
    /// <summary>
    /// 复核并激活。返回 false 表示未能切换（窗口已变、或被系统前台限制拒绝），
    /// 调用方据此保留 UI 且不写成功历史。
    /// </summary>
    bool TryActivate(WindowHandleInfo window);
}

public interface IDebounceTimer
{
    void Restart();
    void Stop();
}

public interface IDebounceTimerFactory
{
    IDebounceTimer Create(TimeSpan interval, Action callback);
}

public interface ISearchScheduler
{
    Task Delay(TimeSpan delay, CancellationToken ct = default);
}

public sealed class SearchScheduler : ISearchScheduler
{
    public Task Delay(TimeSpan delay, CancellationToken ct = default) => Task.Delay(delay, ct);
}
