using Prism.Models;

namespace Prism.Services;

/// <summary>
/// adapter 能做什么。UI 只看这些结构化能力位，永远拿不到原始 UIA selector 或宿主命令行，
/// 也不会把它们传给 indexer（design.md Adapter Boundary）。
/// </summary>
[Flags]
public enum HostCapability
{
    None = 0,

    /// <summary>能读出宿主当前目录。</summary>
    ReadFolder = 1,

    /// <summary>能让宿主导航到某个文件夹。</summary>
    NavigateFolder = 2,

    /// <summary>能把文件名回填到宿主（标准对话框），但不代替用户确认。</summary>
    FillFileName = 4,

    /// <summary>能让宿主自己定位到结果（Ctrl+Enter 语义）。</summary>
    RevealInHost = 8,
}

/// <summary>
/// adapter 失败原因。故意只有一组固定枚举：调用方据此降级并提示，
/// 不需要（也拿不到）宿主内部的错误字符串。
/// </summary>
public enum HostFailureReason
{
    None,

    /// <summary>该 adapter 的功能开关被关闭。</summary>
    AdapterDisabled,

    /// <summary>目标窗口不属于这个 adapter。</summary>
    NotThisHost,

    /// <summary>窗口已关闭或句柄失效。</summary>
    HostGone,

    /// <summary>宿主进程完整性级别更高（提权宿主），拒绝联动。</summary>
    HostElevated,

    /// <summary>识别过程本身失败。</summary>
    DetectFailed,

    /// <summary>识别到宿主但读不到当前目录。</summary>
    FolderUnavailable,

    /// <summary>宿主报告目录不可访问。</summary>
    AccessDenied,

    /// <summary>该 adapter 不支持请求的联动动作。</summary>
    Unsupported,

    /// <summary>联动动作执行失败。</summary>
    ActionFailed,
}

/// <summary>宿主识别结果。<paramref name="IsHost"/> 为 false 时 <paramref name="Reason"/> 说明原因。</summary>
public sealed record HostDetection(
    bool IsHost,
    HostKind Kind,
    HostCapability Capabilities,
    HostFailureReason Reason)
{
    public static HostDetection NotHost(HostKind kind, HostFailureReason reason) =>
        new(false, kind, HostCapability.None, reason);

    public static HostDetection Host(HostKind kind, HostCapability capabilities) =>
        new(true, kind, capabilities, HostFailureReason.None);
}

/// <summary>宿主当前目录。成功时 <paramref name="Path"/> 非空且 <paramref name="Reason"/> 为 None。</summary>
public sealed record HostFolder(string? Path, HostFailureReason Reason)
{
    public static HostFolder Success(string path) => new(path, HostFailureReason.None);
    public static HostFolder Failure(HostFailureReason reason) => new(null, reason);
    public bool IsSuccess => Reason == HostFailureReason.None && !string.IsNullOrEmpty(Path);
}

/// <summary>结果交回意图。标准对话框只导航或回填，最终打开/保存由用户确认。</summary>
public enum HostNavigationIntent
{
    NavigateFolder,
    FillFileName,
    RevealInHost,
}

public sealed record HostNavigationRequest(
    string Path,
    bool IsDirectory,
    HostNavigationIntent Intent);

public sealed record HostNavigation(bool Succeeded, HostFailureReason Reason)
{
    public static HostNavigation Success { get; } = new(true, HostFailureReason.None);
    public static HostNavigation Failure(HostFailureReason reason) => new(false, reason);
}

/// <summary>
/// Ctrl+Enter 交回原宿主的结果。<see cref="Attempted"/> 为 false 表示没有可用宿主上下文
/// （上层应 fallback 到 broker reveal）；为 true 时 <see cref="Succeeded"/> 表示已交回。
/// </summary>
public sealed record HostRevealResult(bool Attempted, bool Succeeded, HostFailureReason Reason)
{
    public static HostRevealResult Unavailable { get; } =
        new(false, false, HostFailureReason.None);

    public static HostRevealResult Success { get; } =
        new(true, true, HostFailureReason.None);

    public static HostRevealResult Failure(HostFailureReason reason) =>
        new(true, false, reason == HostFailureReason.None
            ? HostFailureReason.ActionFailed
            : reason);
}

/// <summary>
/// 一类宿主的联动边界。每个实现有独立开关（<see cref="IsEnabled"/>）：关闭或失败只清空
/// host context，不影响全局搜索。Explorer/系统对话框/Opus 的具体实现属于 G4 步骤 4-5。
/// </summary>
public interface IHostAdapter
{
    HostKind Kind { get; }

    /// <summary>功能开关。关闭时 <see cref="Detect"/> 必须返回 AdapterDisabled。</summary>
    bool IsEnabled { get; }

    /// <summary>判断呼出前捕获的前台窗口是否属于本宿主，并报告结构化能力。</summary>
    HostDetection Detect(IntPtr foregroundWindow);

    /// <summary>读取宿主当前目录。</summary>
    HostFolder GetFolder(IntPtr hostWindow);

    /// <summary>把结果交回宿主：导航文件夹、回填文件名或让宿主定位。</summary>
    HostNavigation NavigateOrFill(IntPtr hostWindow, HostNavigationRequest request);
}

/// <summary>
/// 尚未产品化或本轮明确不做的宿主占位实现。始终报告 AdapterDisabled，
/// 使状态机走「不识别 → 全局搜索」路径，不会因为缺实现而产生错误提示。
/// </summary>
public sealed class DisabledHostAdapter(HostKind kind) : IHostAdapter
{
    public HostKind Kind { get; } = kind;

    public bool IsEnabled => false;

    public HostDetection Detect(IntPtr foregroundWindow) =>
        HostDetection.NotHost(Kind, HostFailureReason.AdapterDisabled);

    public HostFolder GetFolder(IntPtr hostWindow) =>
        HostFolder.Failure(HostFailureReason.AdapterDisabled);

    public HostNavigation NavigateOrFill(IntPtr hostWindow, HostNavigationRequest request) =>
        HostNavigation.Failure(HostFailureReason.AdapterDisabled);

    /// <summary>
    /// 无设置上下文时的安全默认：三类宿主全部关闭。
    /// 生产路径请用 <see cref="HostAdapterCatalog.Create"/> 注入真实 adapter。
    /// </summary>
    public static IReadOnlyList<IHostAdapter> SupportMatrix { get; } =
    [
        new DisabledHostAdapter(HostKind.Explorer),
        new DisabledHostAdapter(HostKind.SystemFileDialog),
        new DisabledHostAdapter(HostKind.DirectoryOpus),
    ];
}
