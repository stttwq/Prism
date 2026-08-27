using Prism.Models;

namespace Prism.Services;

public sealed record SearchFilterOption(string Field, string Value);

public sealed record SearchContext(
    string Mode,
    string? Root,
    IReadOnlyList<SearchFilterOption> Filters,
    int SortVersion,
    // K0 T10.5：末位带默认值 → 现有全部构造点（含 HostScopeController.Apply 的
    // with { Root = ... }）零改动。K0 不纳入 IsEquivalentTo（D7 注释见下方）。
    CommandSearchContext? CommandContext = null)
{
    /// <summary>默认模式：文件/程序/网页混排。线路上省略该值。</summary>
    public const string AllMode = "all";

    /// <summary>窗口模式（G5）：只搜可切换的顶层窗口，不与其他类型混排。</summary>
    public const string WindowMode = "window";

    public static SearchContext Default { get; } = new(AllMode, null, [], 1);

    public bool IsWindowMode =>
        string.Equals(Mode, WindowMode, StringComparison.Ordinal);

    // D7（K1 已纳入）：K1 根搜索命令 lane 上线，IsEquivalentTo 必须纳入
    // CommandContext，否则目录切换时前缀缓存会把旧命令上下文的结果当新查询
    // 的缓存命中返回。
    public bool IsEquivalentTo(SearchContext other) =>
        string.Equals(Mode, other.Mode, StringComparison.Ordinal)
        && string.Equals(Root, other.Root, StringComparison.Ordinal)
        && SortVersion == other.SortVersion
        && Filters.SequenceEqual(other.Filters)
        && CommandContextEquals(CommandContext, other.CommandContext);

    private static bool CommandContextEquals(CommandSearchContext? a, CommandSearchContext? b)
    {
        if (a is null && b is null) return true;
        if (a is null || b is null) return false;
        return string.Equals(a.CurrentFolder, b.CurrentFolder, StringComparison.Ordinal)
            && string.Equals(a.HostKind, b.HostKind, StringComparison.Ordinal)
            && a.HostCapabilities.SequenceEqual(b.HostCapabilities);
    }
}

/// <summary>
/// K0 T10.5：命令搜索上下文（current_folder / host_kind / host_capabilities）。
/// 通道已协商且非 null 时随 SearchPayload 下发；否则 payload 逐字节等于旧格式。
/// current_folder 必须取 HostScopeController.Host.Root（条件 Host.HasUsableRoot），
/// 不是 HostScopeController.Root（后者 Ctrl+G 切回全局后返回 null，命令上下文会消失）。
/// </summary>
public sealed record CommandSearchContext(
    string? CurrentFolder,
    string? HostKind,
    IReadOnlyList<string> HostCapabilities);


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
    /// <summary>打开/执行选中项。query = 触发本次动作的查询文本（查询记忆），null 表示无查询上下文。</summary>
    Task ExecuteAsync(ActionTarget target, string? query = null, CancellationToken ct = default);

    /// <summary>在资源管理器中定位文件。query 含义同 <see cref="ExecuteAsync"/>。</summary>
    Task RevealAsync(ActionTarget target, string? query = null, CancellationToken ct = default);
    Task<IReadOnlyList<ActionItem>> GetActionsAsync(ActionTarget target, CancellationToken ct = default);
    Task RunActionAsync(ActionTarget target, string action, string? query = null, CancellationToken ct = default);
    Task RunActionAsync(ActionTarget target, string action, ActionArgs args, string? query = null, CancellationToken ct = default);

    /// <summary>G5：把枚举 token 换成已复核的句柄，交给前台进程激活。</summary>
    Task<WindowHandleInfo> ResolveWindowAsync(ActionTarget target, CancellationToken ct = default);

    /// <summary>G5：激活成功后回报，由 broker 写窗口历史。失败路径不得调用。</summary>
    Task RecordWindowSwitchAsync(ActionTarget target, string? query = null, CancellationToken ct = default);
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

/// <summary>
/// P4-RESEARCH P4a: 目标文件夹选择器。此前 SearchViewModel 直接弹 WinForms
/// FolderBrowserDialog，copy_to/move_to 分支零测试覆盖——抽出接口后该分支可测。
/// 调用方在 UI/STA 线程，保持同步签名。
/// </summary>
public interface IFolderPicker
{
    /// <summary>返回所选目录；取消返回 null。</summary>
    string? PickFolder(string? description);
}

/// <summary>默认实现：包住 WinForms FolderBrowserDialog，行为与抽取前逐字段一致。</summary>
public sealed class WinFormsFolderPicker : IFolderPicker
{
    public string? PickFolder(string? description)
    {
        using var dialog = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = description ?? "",
            ShowNewFolderButton = true,
        };
        return dialog.ShowDialog() == System.Windows.Forms.DialogResult.OK
            ? dialog.SelectedPath
            : null;
    }
}
