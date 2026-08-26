using System.IO;

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

    /// <summary>目标类型中文名（分组 header 用）：应用程序 / 文件夹 / 文件。</summary>
    public string KindLabel => Target.Kind switch
    {
        "application" => "应用程序",
        "directory" => "文件夹",
        _ => "文件",
    };

    /// <summary>目标去全路径文件名（行内显示用，ToolTip 仍显全路径）。</summary>
    public string TargetFileName => Path.GetFileName(Target.Value);
}
