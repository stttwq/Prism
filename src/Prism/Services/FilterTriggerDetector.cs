using Prism.Models;

namespace Prism.Services;

/// <summary>过滤触发词检测结果：命中的触发词 + 剩余查询内容。</summary>
public sealed record FilterTriggerResult(FilterTrigger Trigger, string QueryTerms);

/// <summary>
/// 过滤触发词检测器，复用 WebModeDetector 的首词+空格算法：
/// 裸关键词（无空格）不触发，仍当普通文件搜索。
/// 长词优先匹配，避免短词遮蔽长词。大小写不敏感。
/// </summary>
public static class FilterTriggerDetector
{
    /// <summary>
    /// 检测查询是否命中某个过滤触发词。命中返回结果，未命中返回 null。
    /// 规则同 WebModeDetector.TryDetect：首词+空格才算触发。
    /// </summary>
    public static FilterTriggerResult? TryDetect(string query, IReadOnlyList<FilterTrigger> triggers)
    {
        if (string.IsNullOrWhiteSpace(query) || triggers.Count == 0)
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

        var order = new int[triggers.Count];
        for (var i = 0; i < triggers.Count; i++) order[i] = i;
        Array.Sort(order, (a, b) =>
        {
            var cmp = triggers[b].Keyword.Length.CompareTo(triggers[a].Keyword.Length);
            return cmp != 0 ? cmp : a.CompareTo(b);
        });

        foreach (var idx in order)
        {
            var trigger = triggers[idx];
            if (string.IsNullOrEmpty(trigger.Keyword))
                continue;
            if (keywordSpan.Equals(trigger.Keyword, StringComparison.OrdinalIgnoreCase))
                return new FilterTriggerResult(trigger, rest);
        }
        return null;
    }

    /// <summary>
    /// 检测并重写查询为 ext:/path: 过滤语法。命中返回重写后的 wireQuery，未命中返回 null。
    /// 例：("tz pdf", triggers) → "ext:pdf"；("pp docs", triggers) → "path:docs"。
    /// 空 QueryTerms 时返回 "ext:"（broker 当无值过滤词处理，回退普通搜索）。
    /// </summary>
    public static string? TryRewrite(string query, IReadOnlyList<FilterTrigger> triggers)
    {
        var hit = TryDetect(query, triggers);
        if (hit is null)
            return null;
        var prefix = hit.Trigger.FilterType switch
        {
            "ext" => "ext:",
            "path" => "path:",
            _ => null,
        };
        if (prefix is null)
            return null;
        return hit.QueryTerms.Length > 0 ? $"{prefix}{hit.QueryTerms}" : prefix;
    }
}
