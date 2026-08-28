using Prism.Models;

namespace Prism.Services;

/// <summary>命令关键字检测结果：命中的命令 + 触发词 + 剩余查询内容。</summary>
public sealed record CommandKeywordResult(CommandDescriptor Command, string Keyword, string QueryTerms);

/// <summary>
/// K3 §4.4：命令关键字检测器，与 WebModeDetector 同构但独立。
/// 规则完全一致：首 token + 必须尾随空白才触发、长关键字优先、大小写不敏感、
/// 非首词不触发。路由插入点在引擎关键字检测之后，保证网页行为逐字节不变（P3）。
/// </summary>
public static class CommandKeywordDetector
{
    /// <summary>
    /// 检测查询是否命中某个命令关键字。命中返回结果，未命中返回 null。
    /// 规则同 WebModeDetector.TryDetect：首词+空格才算触发。
    /// 仅参与有 keyword binding 且 enabled+usable 的命令。
    /// </summary>
    public static CommandKeywordResult? TryDetect(string query, IReadOnlyList<CommandDescriptor> commands)
    {
        if (string.IsNullOrWhiteSpace(query) || commands.Count == 0)
            return null;

        var q = query.AsSpan();
        var start = 0;
        while (start < q.Length && char.IsWhiteSpace(q[start]))
            start++;
        if (start >= q.Length)
            return null;
        var remaining = q[start..];

        var spaceIdx = remaining.IndexOfAny(' ', '\t');
        if (spaceIdx < 0)
            return null;

        var keywordSpan = remaining[..spaceIdx];
        var restSpan = remaining[(spaceIdx + 1)..];
        if (keywordSpan.IsEmpty)
            return null;

        var rest = restSpan.Trim().ToString();

        // 候选集：有 keyword binding 且 enabled+usable 的命令，展平所有关键字。
        // trigger 字段优先于 keywords 首项（broker 侧已做此回退，此处对齐）。
        var candidates = new List<(CommandDescriptor Command, string Keyword)>();
        foreach (var cmd in commands)
        {
            if (!cmd.Enabled || !cmd.IsUsable || cmd.Bindings.Keyword is null)
                continue;
            var trigger = cmd.Bindings.Keyword.Trigger;
            if (!string.IsNullOrEmpty(trigger))
            {
                candidates.Add((cmd, trigger));
                continue;
            }
            foreach (var kw in cmd.Keywords)
            {
                if (!string.IsNullOrEmpty(kw))
                    candidates.Add((cmd, kw));
            }
        }
        if (candidates.Count == 0)
            return null;

        // 长关键字优先匹配，避免短词遮蔽长词。同长度按原始顺序（稳定排序）。
        var order = new int[candidates.Count];
        for (var i = 0; i < candidates.Count; i++) order[i] = i;
        Array.Sort(order, (a, b) =>
        {
            var cmp = candidates[b].Keyword.Length.CompareTo(candidates[a].Keyword.Length);
            return cmp != 0 ? cmp : a.CompareTo(b);
        });

        foreach (var idx in order)
        {
            var (cmd, keyword) = candidates[idx];
            if (keywordSpan.Equals(keyword, StringComparison.OrdinalIgnoreCase))
                return new CommandKeywordResult(cmd, keyword, rest);
        }
        return null;
    }
}
