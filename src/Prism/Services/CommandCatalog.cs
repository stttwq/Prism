using Prism.Models;

namespace Prism.Services;

/// <summary>
/// K0 T9.3：命令目录快照缓存 + RefreshAsync。K0 调用时机只有一处：
/// 握手成功后（ConnectionChanged → true）。
///
/// 设计 §2.1 要求命令目录与索引代际独立命名、独立状态。K0 不做轮询/防抖，
/// 且绝不复用 _generationDebounce（它服务索引代际，复用会混淆两套 generation）。
/// 命令目录的唯一写者是本进程（单实例强制），K0 没有写者，握手后拉一次足够。
/// </summary>
public sealed class CommandCatalog
{
    private readonly object _lock = new();
    private readonly PipeClient _pipe;
    private IReadOnlyList<CommandDescriptor> _snapshot = Array.Empty<CommandDescriptor>();
    private ulong _generation;
    private bool _available;

    public CommandCatalog(PipeClient pipe)
    {
        _pipe = pipe;
    }

    /// <summary>命令功能是否整体可用（握手协商成功 + 目录拉取成功）。</summary>
    public bool IsAvailable
    {
        get { lock (_lock) return _available; }
    }

    /// <summary>当前目录代际（0 = 未拉取）。</summary>
    public ulong Generation
    {
        get { lock (_lock) return _generation; }
    }

    /// <summary>当前目录快照（已过滤：IsUsable + UI-owned 须已知 handler）。</summary>
    public IReadOnlyList<CommandDescriptor> Snapshot
    {
        get { lock (_lock) return _snapshot; }
    }

    /// <summary>
    /// 握手成功后拉取命令目录。失败 → 标记整体不可用、快照清空、不抛给调用方。
    /// 过滤规则（T8 owner 分工）：
    /// - broker-owned 命令保留（broker 已校验 handler 存在性）；
    /// - UI-owned 命令仅保留已注册 handler 者（WPF 侧丢弃未知 id）；
    /// - IsUsable=false 的项一律丢弃。
    /// </summary>
    public async Task RefreshAsync(CancellationToken ct = default)
    {
        if (!_pipe.CommandsAvailable)
        {
            lock (_lock)
            {
                _available = false;
                _snapshot = Array.Empty<CommandDescriptor>();
                _generation = 0;
            }
            return;
        }

        var result = await _pipe.CommandListAsync(ct).ConfigureAwait(false);
        if (result is null)
        {
            lock (_lock)
            {
                _available = false;
                _snapshot = Array.Empty<CommandDescriptor>();
                _generation = 0;
            }
            return;
        }

        var (generation, items) = result.Value;
        var filtered = items
            .Where(d => d.IsUsable)
            .Where(d => d.Owner != "ui" || CommandHandlers.IsKnown(d.Id))
            .ToArray();

        lock (_lock)
        {
            _available = true;
            _generation = generation;
            _snapshot = filtered;
        }
    }

    /// <summary>连接断开时清空快照（与 ConnectionChanged → false 联动）。</summary>
    public void Clear()
    {
        lock (_lock)
        {
            _available = false;
            _snapshot = Array.Empty<CommandDescriptor>();
            _generation = 0;
        }
    }
}
