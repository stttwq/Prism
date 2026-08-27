namespace Prism.Models;

/// <summary>
/// K1：命令调用上下文。前端组装后通过 ExecuteCommand 请求发给 broker，
/// broker 按 owner 分派（broker 直接执行 / ui 回传 UiCommand 交给前端）。
/// 字段名对齐 Rust 侧 CommandInvocationContext（snake_case 线路格式）。
/// </summary>
public sealed record CommandInvocationContext
{
    public string CommandId { get; init; } = "";
    public string Source { get; init; } = "root";
    public CommandArgumentsDto? Arguments { get; init; }
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
