namespace Prism.Models;

/// <summary>
/// 受支持的宿主类型（G4 支持矩阵）。首版只承诺 Explorer、Windows 标准打开/保存对话框
/// 与 Directory Opus 13.23；其余前台窗口一律视为 <see cref="None"/>，直接全局搜索。
/// </summary>
public enum HostKind
{
    None,
    Explorer,
    SystemFileDialog,
    DirectoryOpus,
}

/// <summary>
/// 当前目录识别结果。除 <see cref="Detected"/> 外都表示「已回到全局搜索」，
/// 且必须伴随 root 清空 —— 任何失败都不允许沿用上一次目录。
/// </summary>
public enum HostDetectionStatus
{
    /// <summary>还没有呼出过，或上一次呼出后状态已被清空。</summary>
    NotAttempted,

    /// <summary>识别成功且 root 可用。</summary>
    Detected,

    /// <summary>当前目录搜索总开关已关闭，没有尝试识别。</summary>
    FeatureDisabled,

    /// <summary>前台窗口不是受支持的宿主（不提示，属正常情况）。</summary>
    NoSupportedHost,

    /// <summary>宿主本身受支持，但该 adapter 的功能开关被关闭。</summary>
    AdapterDisabled,

    /// <summary>adapter 识别过程失败。</summary>
    DetectFailed,

    /// <summary>识别到宿主但拿不到当前目录。</summary>
    FolderUnavailable,

    /// <summary>目录存在但不可访问。</summary>
    AccessDenied,

    /// <summary>目录路径本身无效（相对路径、UNC/设备路径、过长、过深、不是目录）。</summary>
    RootInvalid,

    /// <summary>路径有效但不在索引中（非 NTFS、卷未索引或目录尚未建到索引）。</summary>
    RootNotIndexed,

    /// <summary>捕获的宿主窗口已经消失。</summary>
    HostGone,
}

/// <summary>搜索范围：全局或宿主当前目录（递归含子目录）。</summary>
public enum SearchScope
{
    Global,
    CurrentDirectory,
}

/// <summary>
/// 一次呼出对应的宿主上下文快照。<see cref="CapturedWindow"/> 是呼出前的前台 HWND，
/// <see cref="Root"/> 只在 <see cref="Status"/> 为 <see cref="HostDetectionStatus.Detected"/>
/// 时有值；失败一律用 <see cref="Cleared"/> 生成一个无 root 的实例。
/// </summary>
public sealed record HostContext(
    HostKind Kind,
    IntPtr CapturedWindow,
    string? Root,
    HostDetectionStatus Status)
{
    public static HostContext None { get; } =
        new(HostKind.None, IntPtr.Zero, null, HostDetectionStatus.NotAttempted);

    /// <summary>失败/关闭时使用：保留原因，但清空宿主与 root。</summary>
    public static HostContext Cleared(HostDetectionStatus status) =>
        new(HostKind.None, IntPtr.Zero, null, status);

    public bool HasUsableRoot =>
        Status == HostDetectionStatus.Detected
        && Kind != HostKind.None
        && !string.IsNullOrEmpty(Root);
}
