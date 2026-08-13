using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 网页模式检测（G8）。纯函数，无副作用，可测。
/// 从用户输入中拆出引擎关键词和剩余查询词，返回是否为网页模式及引擎信息。
/// 与 broker 端 <c>websearch::try_match</c> 使用相同的关键词优先规则（长关键词优先）。
/// </summary>
public static class WebModeDetector
{
    /// <summary>
    /// 尝试从输入中解析出网页关键词。与 broker 端逻辑一致：
    /// 首个空白分隔 token 为关键词，长度降序匹配避免 <c>bi</c> 被误匹配为 <c>b</c>。
    /// </summary>
    /// <param name="query">用户原始输入（未 trim 的文本框值）。</param>
    /// <param name="engines">当前引擎列表（含内置和自定义）。</param>
    /// <returns>命中时返回检测器结果；否则 null。</returns>
    public static WebModeResult? TryDetect(string query, IReadOnlyList<WebEngine> engines)
    {
        var q = query.AsSpan().Trim();
        if (q.IsEmpty || engines.Count == 0)
            return null;

        // 拆出首 token
        var spaceIdx = q.IndexOfAny(' ', '\t');
        ReadOnlySpan<char> keywordSpan;
        ReadOnlySpan<char> restSpan;
        if (spaceIdx < 0)
        {
            keywordSpan = q;
            restSpan = ReadOnlySpan<char>.Empty;
        }
        else
        {
            keywordSpan = q[..spaceIdx];
            restSpan = q[(spaceIdx + 1)..];
        }

        if (keywordSpan.IsEmpty)
            return null;

        var rest = restSpan.Trim().ToString();

        // 长关键词优先：bi 在 b 之前。使用索引列表排序避免原地修改 engines。
        var order = new int[engines.Count];
        for (var i = 0; i < engines.Count; i++)
            order[i] = i;
        Array.Sort(order, (a, b) =>
        {
            var cmp = engines[b].Keyword.Length.CompareTo(engines[a].Keyword.Length);
            return cmp != 0 ? cmp : a.CompareTo(b);
        });

        foreach (var idx in order)
        {
            var eng = engines[idx];
            if (string.IsNullOrEmpty(eng.Keyword))
                continue;
            if (keywordSpan.Equals(eng.Keyword, StringComparison.OrdinalIgnoreCase))
            {
                var isBuiltin = IsBuiltIn(eng);
                return new WebModeResult(
                    eng.Keyword,
                    eng.Name,
                    eng.UrlTemplate,
                    rest,
                    isBuiltin);
            }
        }
        return null;
    }

    /// <summary>内置引擎名集合，用于判断是否支持联想。</summary>
    private static readonly HashSet<string> BuiltInNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "Bing", "百度", "Google",
    };

    /// <summary>判断引擎是否为内置（支持联想）。自定义引擎不支持联想 API。</summary>
    public static bool IsBuiltIn(WebEngine engine) =>
        BuiltInNames.Contains(engine.Name);

    /// <summary>用 URL 模板和查询词构造完整 URL。</summary>
    public static string BuildUrl(string urlTemplate, string queryTerms)
    {
        if (string.IsNullOrEmpty(urlTemplate))
            return "";
        return urlTemplate.Replace("{q}", UrlEncode(queryTerms));
    }

    /// <summary>application/x-www-form-urlencoded 风格的百分号编码（UTF-8 字节）。空格编为 %20。</summary>
    private static string UrlEncode(string s)
    {
        if (string.IsNullOrEmpty(s))
            return "";
        var bytes = System.Text.Encoding.UTF8.GetBytes(s);
        var sb = new System.Text.StringBuilder(s.Length * 3);
        foreach (var b in bytes)
        {
            if ((uint)(b - 'A') <= 'Z' - 'A'
                || (uint)(b - 'a') <= 'z' - 'a'
                || (uint)(b - '0') <= '9' - '0'
                || b == '-' || b == '_' || b == '.' || b == '~')
            {
                sb.Append((char)b);
            }
            else
            {
                sb.Append('%');
                const string Hex = "0123456789ABCDEF";
                sb.Append(Hex[b >> 4]);
                sb.Append(Hex[b & 0xF]);
            }
        }
        return sb.ToString();
    }
}

/// <summary>
/// 网页模式检测结果。
/// </summary>
/// <param name="Keyword">命中的引擎关键词。</param>
/// <param name="EngineName">引擎显示名。</param>
/// <param name="UrlTemplate">引擎 URL 模板。</param>
/// <param name="QueryTerms">去掉关键词后的剩余查询词。</param>
/// <param name="IsBuiltIn">是否为内置引擎（支持联想）。</param>
public sealed record WebModeResult(
    string Keyword,
    string EngineName,
    string UrlTemplate,
    string QueryTerms,
    bool IsBuiltIn);
