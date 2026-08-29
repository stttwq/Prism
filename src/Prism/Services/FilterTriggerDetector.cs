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
    /// ext 触发词：像扩展名的 token（ASCII 字母数字，可带点/逗号，须含字母）
    /// 作为过滤值，其余 token 全部保留为文件名搜索词——
    /// "tz txt 报告" → "ext:txt 报告"（过滤+名字）、"tz 报告" → "报告"（纯名字
    /// 不过滤）、"tz 报告 txt" → "ext:txt 报告"（顺序无关）。旧行为把触发词后
    /// 整段当扩展名值，"tz 报告" 变成 ext:「报告」永远查不到。
    /// path 触发词：整段 rest 作路径值（路径可含空格，引号由 broker 解析）。
    /// 空 QueryTerms 时返回裸前缀（broker 当无值过滤词处理，回退普通搜索）。
    /// </summary>
    public static string? TryRewrite(string query, IReadOnlyList<FilterTrigger> triggers)
    {
        var hit = TryDetect(query, triggers);
        if (hit is null)
            return null;
        switch (hit.Trigger.FilterType)
        {
            case "ext":
                return RewriteExt(hit.QueryTerms);
            case "path":
                return hit.QueryTerms.Length > 0 ? $"path:{hit.QueryTerms}" : "path:";
            default:
                return null;
        }
    }

    private static string RewriteExt(string terms)
    {
        if (terms.Length == 0)
            return "ext:";
        var exts = new List<string>();
        var names = new List<string>();
        foreach (var token in terms.Split(' ', StringSplitOptions.RemoveEmptyEntries))
        {
            if (IsExtShape(token)) exts.Add(token);
            else names.Add(token);
        }
        if (exts.Count == 0)
            return terms;
        var extToken = "ext:" + string.Join(",", exts);
        return names.Count > 0 ? $"{extToken} {string.Join(" ", names)}" : extToken;
    }

    /// <summary>像扩展名的 token：≤16 字符、仅 ASCII 字母数字/点/逗号、至少一个字母
    /// （排除纯数字的年份类词，"7z"/"mp3" 通过）。</summary>
    private static bool IsExtShape(string token) =>
        token.Length > 0 && token.Length <= 16 && token.IndexOf('/') < 0
        && token.All(c => char.IsAsciiLetterOrDigit(c) || c is '.' or ',')
        && token.Any(char.IsAsciiLetter);
}
