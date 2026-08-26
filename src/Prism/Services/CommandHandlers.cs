namespace Prism.Services;

/// <summary>
/// K0 T9.4：UI handler 注册表骨架。K0 不执行任何命令，只判定 UI-owned 命令
/// 是否已被前端注册（设计 §10.1「仅已注册 UI handler；未知 id 拒绝」）。
/// K1 加执行时把 HashSet 换成 Dictionary&lt;string, Func&lt;...&gt;&gt;。
/// </summary>
internal static class CommandHandlers
{
    /// <summary>已注册的 UI-owned 命令 id。K0 只有内置设置页。</summary>
    private static readonly HashSet<string> Known = new(StringComparer.Ordinal)
    {
        "prism.settings.open",
    };

    /// <summary>id 是否在 UI handler 注册表内。</summary>
    public static bool IsKnown(string id) => Known.Contains(id);
}
