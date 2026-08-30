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
    /// 检测并重写查询为过滤语法。命中返回重写后的 wireQuery，未命中返回 null。
    /// 所有类型都支持「条件 + 文件名」：形似该类型条件的 token 作过滤值，其余
    /// token 全部保留为文件名搜索词（与 WebModeDetector 首词+空格互补）。
    /// - ext："tz txt 报告" → "ext:txt 报告"；"tz 报告" → "报告"（纯名字不过滤）；
    ///   多扩展名逗号 OR（"tz txt,doc"）。
    /// - size/dm/dc：条件 token 各自成一条过滤（AND 语义），如
    ///   "dz >1mb <100mb 报告" → "size:>1mb size:<100mb 报告"。
    /// - file/folder：旗标不取值，"wj 报告" → "file: 报告"。
    /// - path：含 \ / : 的 token 作路径值（多个合并加引号），其余作文件名；
    ///   无路径形 token 时维持旧行为（整段作路径子串）。
    /// 空 QueryTerms 时返回裸前缀（broker 当无值过滤词处理，回退普通搜索）。
    /// 条件形状判定与 broker parse_query 的校验同口径，宽松版——最终合法性由
    /// broker 裁决（不合法 token 降级普通文本）。
    /// </summary>
    public static string? TryRewrite(string query, IReadOnlyList<FilterTrigger> triggers)
    {
        var hit = TryDetect(query, triggers);
        if (hit is null)
            return null;
        var terms = hit.QueryTerms;
        switch (hit.Trigger.FilterType)
        {
            case "ext":
                return RewriteExt(terms);
            case "size":
                return RewriteValueFilters(terms, IsSizeToken, "size:");
            case "dm":
            case "dc":
                return RewriteValueFilters(terms, IsDateToken, hit.Trigger.FilterType + ":");
            case "file":
            case "folder":
                var flag = hit.Trigger.FilterType + ":";
                return terms.Length > 0 ? $"{flag} {terms}" : flag;
            case "path":
                return RewritePath(terms);
            default:
                return null;
        }
    }

    /// <summary>条件 + 文件名分区重写：条件 token 各自带前缀（broker AND 语义），
    /// 名字 token 原样放行。</summary>
    private static string RewriteValueFilters(
        string terms, Func<string, bool> isCondition, string prefix)
    {
        if (terms.Length == 0)
            return prefix;
        Partition(terms, isCondition, out var conds, out var names);
        if (conds.Count == 0)
            return terms;
        var parts = new List<string>(conds.Select(c => prefix + c));
        if (names.Count > 0)
            parts.Add(string.Join(" ", names));
        return string.Join(" ", parts);
    }

    private static void Partition(
        string terms, Func<string, bool> isCondition,
        out List<string> conds, out List<string> names)
    {
        conds = new List<string>();
        names = new List<string>();
        foreach (var token in terms.Split(' ', StringSplitOptions.RemoveEmptyEntries))
            (isCondition(token) ? conds : names).Add(token);
    }

    private static string RewriteExt(string terms)
    {
        if (terms.Length == 0)
            return "ext:";
        // 歧义防护（2026-08-30 用户实测 "tz exe speed" 搜出一堆不相干 .exe）：
        // IsExtShape 对任意 ASCII 字母数字词都为真——"speed"/"report" 这类普通
        // 文件名词若被继续吞成第二个扩展名，查询就变成 "ext:exe,speed" 整段失配。
        // 因此只有**第一个** ext 形 token 作过滤值，其余 token 一律保留为文件名；
        // 要多个扩展名用逗号（"tz exe,msi 报告"）。
        string? ext = null;
        var names = new List<string>();
        foreach (var token in terms.Split(' ', StringSplitOptions.RemoveEmptyEntries))
        {
            if (ext is null && IsExtShape(token))
                ext = token;
            else
                names.Add(token);
        }
        if (ext is null)
            return terms;
        var extToken = "ext:" + ext;
        return names.Count > 0 ? $"{extToken} {string.Join(" ", names)}" : extToken;
    }

    private static string RewritePath(string terms)
    {
        if (terms.Length == 0)
            return "path:";
        var merged = MergeQuotedTokens(terms.Split(' ', StringSplitOptions.RemoveEmptyEntries));
        var pathParts = new List<string>();
        var names = new List<string>();
        foreach (var token in merged)
        {
            if (token.IndexOfAny(PathShapeChars) >= 0)
                pathParts.Add(token.Trim('"'));
            else
                names.Add(token);
        }
        if (pathParts.Count == 0)
            return "path:" + terms; // 无路径形 token：整段作路径子串（旧行为）
        var joined = string.Join(" ", pathParts);
        var value = joined.Contains(' ') ? $"\"{joined}\"" : joined;
        return names.Count > 0 ? $"path:{value} {string.Join(" ", names)}" : $"path:{value}";
    }

    private static readonly char[] PathShapeChars = { '\\', '/', ':' };

    /// <summary>引号含空格的路径 token 合并（`"D:\my docs"` 一个 token）。</summary>
    private static List<string> MergeQuotedTokens(string[] tokens)
    {
        var merged = new List<string>(tokens.Length);
        for (var i = 0; i < tokens.Length; i++)
        {
            var t = tokens[i];
            if (t.StartsWith('"') && !t.EndsWith('"'))
            {
                while (i + 1 < tokens.Length && !t.EndsWith('"'))
                {
                    i++;
                    t += " " + tokens[i];
                }
            }
            merged.Add(t);
        }
        return merged;
    }

    /// <summary>像扩展名的 token：≤16 字符、仅 ASCII 字母数字/点/逗号、至少一个字母
    /// （排除纯数字的年份类词，"7z"/"mp3" 通过）。</summary>
    private static bool IsExtShape(string token) =>
        token.Length > 0 && token.Length <= 16 && token.IndexOf('/') < 0
        && token.All(c => char.IsAsciiLetterOrDigit(c) || c is '.' or ',')
        && token.Any(char.IsAsciiLetter);

    // ── size/dm/dc 条件形状（宽松版，broker parse_query 最终裁决）──────────

    private static readonly string[] SizeBuckets =
        { "empty", "tiny", "small", "medium", "large", "huge", "gigantic" };

    private static readonly string[] DateNamedWords =
    {
        "today", "yesterday", "thisweek", "lastweek",
        "thismonth", "lastmonth", "thisyear", "lastyear",
    };

    private static bool IsSizeToken(string token)
    {
        var body = StripConditionOps(token);
        if (SizeBuckets.Contains(body, StringComparer.OrdinalIgnoreCase))
            return true;
        var parts = body.Split(new[] { ".." }, StringSplitOptions.None);
        return parts.Length <= 2 && parts.All(p => p.Length > 0 && IsSizeAtom(p));
    }

    private static bool IsSizeAtom(string s)
    {
        if (s.Length == 0) return false;
        var i = 0;
        var dots = 0;
        while (i < s.Length && (char.IsAsciiDigit(s[i]) || s[i] == '.'))
        {
            if (s[i] == '.') dots++;
            i++;
        }
        if (i == 0 || dots > 1) return false;
        var unit = s[i..].ToLowerInvariant();
        return unit is "" or "b" or "kb" or "mb" or "gb" or "tb";
    }

    private static bool IsDateToken(string token)
    {
        var body = StripConditionOps(token);
        var parts = body.Split(new[] { ".." }, StringSplitOptions.None);
        if (parts.Length > 2) return false;
        return parts.All(IsDateAtom) && parts[0].Length > 0;
    }

    private static bool IsDateAtom(string s)
    {
        if (DateNamedWords.Contains(s, StringComparer.OrdinalIgnoreCase)) return true;
        return s.Length is 4 or 6 or 8 && s.All(char.IsAsciiDigit);
    }

    private static string StripConditionOps(string token)
    {
        if (token.StartsWith(">=", StringComparison.Ordinal)
            || token.StartsWith("<=", StringComparison.Ordinal))
            return token[2..];
        if (token.Length > 0 && (token[0] is '>' or '<' or '='))
            return token[1..];
        return token;
    }
}
