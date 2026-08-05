using Prism.Models;

namespace Prism.Services;

/// <summary>
/// G4 支持矩阵工厂：Explorer / Directory Opus 使用真实 adapter（默认关），
/// SystemFileDialog 仍为占位（本轮不做 UIA）。
/// </summary>
public static class HostAdapterCatalog
{
    /// <summary>
    /// 按设置构造 adapter 列表。两个真实 adapter 的 <see cref="IHostAdapter.IsEnabled"/>
    /// 读取<strong>可变</strong>设置委托，保存设置后无需重建 controller 即可生效。
    /// </summary>
    public static IReadOnlyList<IHostAdapter> Create(Func<Settings> settings)
    {
        ArgumentNullException.ThrowIfNull(settings);
        return
        [
            new ExplorerHostAdapter(isEnabled: () => settings().ExplorerHostIntegrationEnabled),
            new DisabledHostAdapter(HostKind.SystemFileDialog),
            new DirectoryOpusHostAdapter(isEnabled: () => settings().DirectoryOpusHostIntegrationEnabled),
        ];
    }

    /// <summary>固定快照版本，便于单测构造。</summary>
    public static IReadOnlyList<IHostAdapter> Create(Settings settings) =>
        Create(() => settings);
}
