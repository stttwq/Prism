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
    /// **必须有关键词 + 空格**才触发网页模式——仅输入 <c>g</c> 或 <c>bi</c> 不带空格时
    /// 返回 null，让本地文件搜索正常工作。例如 <c>g</c> 搜本地，<c>g </c> 进入网页模式。
    /// </summary>
    /// <param name="query">用户原始输入（未 trim 的文本框值）。</param>
    /// <param name="engines">当前引擎列表（含内置和自定义）。</param>
    /// <returns>命中时返回检测器结果；否则 null。</returns>
    public static WebModeResult? TryDetect(string query, IReadOnlyList<WebEngine> engines)
    {
        if (string.IsNullOrWhiteSpace(query) || engines.Count == 0)
            return null;

        // Find first non-whitespace (skip leading spaces), then require whitespace
        // after the keyword. "g" → null (local search); "g " → web mode.
        var q = query.AsSpan();
        var start = 0;
        while (start < q.Length && char.IsWhiteSpace(q[start]))
            start++;
        if (start >= q.Length)
            return null;
        var remaining = q[start..];

        // 拆出首 token — 必须有空白分隔符，否则视为普通本地搜索。
        var spaceIdx = remaining.IndexOfAny(' ', '\t');
        if (spaceIdx < 0)
            return null;

        var keywordSpan = remaining[..spaceIdx];
        var restSpan = remaining[(spaceIdx + 1)..];

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
        return urlTemplate.Replace("{q}", SuggestionUrlEncoder.UrlEncode(queryTerms));
    }

    /// <summary>
    /// 识别"明显的网址"并返回可直接打开的 URL（小问题 Q3）。
    /// 命中：带 scheme 的（http://…）、域名形态（example.com[:port][/path…]，补 https://）、
    /// localhost（补 http://）。拒绝：含空白、版本号（3.14 / 1.2.3，末段须为 ≥2 个字母的
    /// TLD）、Windows 路径（C:\x）、主机名无点（server）——这些继续走引擎搜索。
    /// </summary>
    public static string? TryGetDirectUrl(string? text)
    {
        if (string.IsNullOrWhiteSpace(text))
            return null;
        var t = text.Trim();
        if (t.Length > 2048)
            return null;
        foreach (var c in t)
            if (char.IsWhiteSpace(c) || c == '\0')
                return null;

        // 带 scheme：格式宽松校验后原样打开，浏览器负责剩下的解析。
        var schemeEnd = t.IndexOf("://", StringComparison.Ordinal);
        if (schemeEnd > 0)
        {
            var scheme = t[..schemeEnd];
            var ok = scheme.Length >= 2
                && char.IsAsciiLetter(scheme[0])
                && scheme.All(c => char.IsAsciiLetterOrDigit(c) || c is '+' or '-' or '.');
            return ok ? t : null;
        }

        // localhost 是唯一无点也直接打开的主机名（开发者习惯）。
        if (t.Equals("localhost", StringComparison.OrdinalIgnoreCase)
            || t.StartsWith("localhost:", StringComparison.OrdinalIgnoreCase)
            || t.StartsWith("localhost/", StringComparison.OrdinalIgnoreCase))
            return "http://" + t;

        // 域名形态：host[:port] 后可跟 /path、?query、#fragment。
        var authorityEnd = t.IndexOfAny(['/', '?', '#']);
        var authority = authorityEnd < 0 ? t : t[..authorityEnd];
        var lastColon = authority.LastIndexOf(':');
        var host = lastColon < 0 ? authority : authority[..lastColon];
        if (lastColon >= 0)
        {
            var port = authority[(lastColon + 1)..];
            if (port.Length == 0 || !ushort.TryParse(port, out _))
                return null;
        }

        var labels = host.Split('.');
        if (labels.Length < 2)
            return null;
        foreach (var label in labels)
        {
            if (label.Length is < 1 or > 63)
                return null;
            if (!char.IsLetterOrDigit(label[0]) || !char.IsLetterOrDigit(label[^1]))
                return null;
            foreach (var c in label)
                if (!char.IsLetterOrDigit(c) && c != '-')
                    return null;
        }

        // 末段必须是 ≥2 个字母的 TLD：挡住 "3.14"、"1.2.3" 这类版本号/数字串。
        var tld = labels[^1];
        if (tld.Length < 2 || !tld.All(char.IsLetter))
            return null;

        return "https://" + t;
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
