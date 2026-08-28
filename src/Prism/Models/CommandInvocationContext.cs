namespace Prism.Models;

/// <summary>
/// K1：命令调用上下文。前端组装后通过 ExecuteCommand 请求发给 broker，
/// broker 按 owner 分派（broker 直接执行 / ui 回传 UiCommand 交给前端）。
/// 字段名对齐 Rust 侧 CommandInvocationContext（snake_case 线路格式）。
/// K2 §4.1：补 Selection（带 typed target）与 StagedPaths，支撑动作面板命令段
/// 与暂存区批量链路。
/// </summary>
public sealed record CommandInvocationContext
{
    public string CommandId { get; init; } = "";
    public string Source { get; init; } = "root";
    public CommandArgumentsDto? Arguments { get; init; }
    public CommandSelectionDto? Selection { get; init; }
    public IReadOnlyList<string> StagedPaths { get; init; } = Array.Empty<string>();
    public string? CurrentFolder { get; init; }
    public string? HostKind { get; init; }
    public IReadOnlyList<string> HostCapabilities { get; init; } = Array.Empty<string>();
}

/// <summary>命令参数（K1 暂不支持参数输入，留空结构）。</summary>
public sealed record CommandArgumentsDto
{
    public string? Text { get; init; }
    public string? Destination { get; init; }
    public string? OutputPath { get; init; }
}

/// <summary>
/// K2 §4.1：命令调用时携带的选中项快照。target 是 typed 操作对象，broker 据此
/// 复核；title/subtitle 只是有界 UI 快照，不可作为路径或权限依据。
/// </summary>
public sealed record CommandSelectionDto
{
    public ActionTarget? Target { get; init; }
    public string Title { get; init; } = "";
    public string Subtitle { get; init; } = "";
}
