namespace Prism.Models;

/// <summary>别名系统：一条「目标 → 词表」绑定（设置页展示用）。</summary>
/// <param name="Target">目标（file/directory/application，value 为完整路径）。</param>
/// <param name="Words">词表（已归一化：trim + 小写）。</param>
/// <param name="BoundAtUtc">绑定时间（Unix 秒；冷启动排序用）。</param>
public sealed record AliasEntry(
    ActionTarget Target,
    IReadOnlyList<string> Words,
    long BoundAtUtc)
{
    public string WordsText => string.Join("、", Words);
}
