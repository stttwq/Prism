using System.Net.Http;
using System.Text;
using System.Text.Json;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 可注入的 HTTP transport（G8 联想测试地基）。生产环境用 <see cref="SystemHttpTransport"/>；
/// 测试用 <c>FakeHttpTransport</c> 注入固定夹具和故障场景。
/// </summary>
public interface IHttpTransport
{
    /// <summary>
    /// 发送 GET 请求，返回 (状态码类别, 响应体字节)。
    /// 实现必须限制重定向次数、连接/总超时和响应读取字节。
    /// </summary>
    Task<HttpResponse> GetAsync(string url, CancellationToken ct);
}

/// <summary>HTTP 响应结果。</summary>
/// <param name="StatusCategory">状态码类别：2xx 成功；其他视为失败，返回空联想。</param>
/// <param name="Body">响应体（已限制字节上限）。</param>
/// <param name="CharSet">响应声明的字符集（来自 Content-Type）；为 null 时按 UTF-8 解码。</param>
public sealed record HttpResponse(int StatusCategory, byte[] Body, string? CharSet = null);

/// <summary>
/// 生产 HTTP transport：使用单例 <see cref="HttpClient"/>。
/// 限制重定向 3 次、响应读取 64KB、连接 2s/总超时 800ms。
/// </summary>
public sealed class SystemHttpTransport : IHttpTransport
{
    private static readonly HttpClient Client = CreateClient();

    private static HttpClient CreateClient()
    {
        var handler = new SocketsHttpHandler
        {
            AllowAutoRedirect = true,
            MaxAutomaticRedirections = 3,
            PooledConnectionLifetime = TimeSpan.FromMinutes(2),
        };
        return new HttpClient(handler)
        {
            Timeout = TimeSpan.FromMilliseconds(900), // 联想 800ms 放弃 + 余量
            MaxResponseContentBufferSize = 64 * 1024,  // 64KB 上限
        };
    }

    public async Task<HttpResponse> GetAsync(string url, CancellationToken ct)
    {
        var resp = await Client.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);
        var category = (int)resp.StatusCode / 100;
        var charset = resp.Content.Headers.ContentType?.CharSet;
        // 限制实际读取字节，即使 MaxResponseContentBufferSize 设置也显式截断。
        using var stream = await resp.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        var buf = new byte[64 * 1024];
        var read = 0;
        int n;
        while (read < buf.Length && (n = await stream.ReadAsync(buf.AsMemory(read), ct).ConfigureAwait(false)) > 0)
            read += n;
        return new HttpResponse(category, buf[..read], charset);
    }
}

/// <summary>
/// 联想服务实现（G8）。单例，持有可注入 HTTP transport。
/// 每个内置引擎有固定的 adapter 解析响应；自定义引擎不进入此路径。
/// </summary>
public sealed class SuggestionService : ISuggestionService
{
    private const int MaxSuggestions = 5;
    private const int MaxSuggestionLength = 128;
    private static readonly TimeSpan Timeout = TimeSpan.FromMilliseconds(800);

    private readonly IHttpTransport _http;

    public SuggestionService(IHttpTransport? http = null)
    {
        _http = http ?? new SystemHttpTransport();
    }

    public async Task<IReadOnlyList<SuggestionItem>> GetSuggestionsAsync(
        string engine,
        string query,
        CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(query))
            return [];

        var adapter = GetAdapter(engine);
        if (adapter is null)
            return [];

        var url = adapter.BuildRequestUrl(query);
        if (string.IsNullOrEmpty(url))
            return [];

        try
        {
            using var linkedCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            linkedCts.CancelAfter(Timeout);

            var resp = await _http.GetAsync(url, linkedCts.Token).ConfigureAwait(false);
            if (resp.StatusCategory != 2)
                return [];

            var text = (TryResolveEncoding(resp.CharSet) ?? Encoding.UTF8).GetString(resp.Body);
            var suggestions = adapter.Parse(text);
            return Bound(suggestions, adapter, query);
        }
        catch
        {
            // 断网、超时、取消、解析失败：静默返回空，不显示干扰性错误。
            return [];
        }
    }

    /// <summary>验证、截断每条联想，并构造完整 URL。</summary>
    private static IReadOnlyList<SuggestionItem> Bound(
        List<string> suggestions,
        ISuggestionAdapter adapter,
        string query)
    {
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var list = new List<SuggestionItem>(MaxSuggestions);
        foreach (var s in suggestions)
        {
            if (list.Count >= MaxSuggestions)
                break;
            var text = s.Trim();
            if (string.IsNullOrEmpty(text) || text.Length > MaxSuggestionLength)
                continue;
            if (!seen.Add(text))
                continue;
            var url = WebModeDetector.BuildUrl(adapter.UrlTemplate, text);
            list.Add(new SuggestionItem(text, url));
        }
        return list;
    }

    /// <summary>按引擎名选择 adapter。未知引擎返回 null（自定义引擎不进此路径）。</summary>
    private static ISuggestionAdapter? GetAdapter(string engineName) =>
        engineName switch
        {
            "Bing" => new BingSuggestionAdapter(),
            "百度" => new BaiduSuggestionAdapter(),
            "Google" => new GoogleSuggestionAdapter(),
            _ => null,
        };

    /// <summary>
    /// 把响应声明的 charset（如 "gbk"、"gb2312"、"utf-8"）解析为 <see cref="Encoding"/>。
    /// 百度联想 API 返回 GBK 编码，若一律按 UTF-8 解码会产生乱码（如「知乎」→「之火」）。
    /// 无法识别的 charset 返回 null，调用方回退 UTF-8。
    /// </summary>
    private static Encoding? TryResolveEncoding(string? charSet)
    {
        if (string.IsNullOrWhiteSpace(charSet))
            return null;
        // .NET Core/.NET 5+ 默认只内建 UTF 系列；GBK/GB2312 需注册 CodePagesEncodingProvider。
        // 一次性注册，幂等。
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        try
        {
            // 把 http charset 名映射到 .NET 编码名。gb2312 在 .NET 上可用 "gb2312"，
            // 但 GBK 是其超集，统一取 "gbk" 以覆盖全部汉字。
            var name = charSet.Trim();
            if (name.Equals("gb2312", StringComparison.OrdinalIgnoreCase))
                name = "gbk";
            return Encoding.GetEncoding(name);
        }
        catch
        {
            // 未知 charset（.NET 无对应 CodePage）回退 UTF-8。
            return null;
        }
    }
}

/// <summary>联想 adapter 接口：固定 endpoint + 解析逻辑。</summary>
internal interface ISuggestionAdapter
{
    string UrlTemplate { get; }
    string BuildRequestUrl(string query);
    List<string> Parse(string responseBody);
}

/// <summary>Bing 联想 adapter：使用 Bing 搜索建议 API。</summary>
internal sealed class BingSuggestionAdapter : ISuggestionAdapter
{
    public string UrlTemplate => "https://www.bing.com/search?q={q}";

    public string BuildRequestUrl(string query) =>
        "https://api.bing.com/qsonhs.aspx?type=cb&q=" + SuggestionUrlEncoder.UrlEncode(query);

    public List<string> Parse(string body)
    {
        var results = new List<string>();
        try
        {
            using var doc = JsonDocument.Parse(body);
            // AS.Alternatives 数组，每项含 "Txt" 字段
            if (doc.RootElement.TryGetProperty("AS", out var asProp)
                && asProp.TryGetProperty("Alternatives", out var alts)
                && alts.ValueKind == JsonValueKind.Array)
            {
                foreach (var alt in alts.EnumerateArray())
                {
                    if (alt.TryGetProperty("Txt", out var txt) && txt.ValueKind == JsonValueKind.String)
                        results.Add(txt.GetString() ?? "");
                }
            }
        }
        catch { /* 解析失败返回空 */ }
        return results;
    }
}

/// <summary>百度联想 adapter：使用百度搜索建议 API。</summary>
internal sealed class BaiduSuggestionAdapter : ISuggestionAdapter
{
    public string UrlTemplate => "https://www.baidu.com/s?wd={q}";

    public string BuildRequestUrl(string query) =>
        "https://suggestion.baidu.com/su?wd=" + SuggestionUrlEncoder.UrlEncode(query) + "&action=opensearch&ie=utf-8";

    public List<string> Parse(string body)
    {
        var results = new List<string>();
        try
        {
            using var doc = JsonDocument.Parse(body);
            // 百度返回 ["query", ["sugg1", "sugg2", ...]]
            if (doc.RootElement.ValueKind == JsonValueKind.Array
                && doc.RootElement.GetArrayLength() >= 2)
            {
                var arr = doc.RootElement[1];
                if (arr.ValueKind == JsonValueKind.Array)
                {
                    foreach (var item in arr.EnumerateArray())
                    {
                        if (item.ValueKind == JsonValueKind.String)
                            results.Add(item.GetString() ?? "");
                    }
                }
            }
        }
        catch { /* 解析失败返回空 */ }
        return results;
    }
}

/// <summary>Google 联想 adapter：使用 Google 搜索建议 API。</summary>
internal sealed class GoogleSuggestionAdapter : ISuggestionAdapter
{
    public string UrlTemplate => "https://www.google.com/search?q={q}";

    public string BuildRequestUrl(string query) =>
        "https://suggestqueries.google.com/complete/search?client=firefox&q=" + SuggestionUrlEncoder.UrlEncode(query);

    public List<string> Parse(string body)
    {
        var results = new List<string>();
        try
        {
            using var doc = JsonDocument.Parse(body);
            // Google Firefox 客户端返回 ["query", ["sugg1", "sugg2", ...]]
            if (doc.RootElement.ValueKind == JsonValueKind.Array
                && doc.RootElement.GetArrayLength() >= 2)
            {
                var arr = doc.RootElement[1];
                if (arr.ValueKind == JsonValueKind.Array)
                {
                    foreach (var item in arr.EnumerateArray())
                    {
                        if (item.ValueKind == JsonValueKind.String)
                            results.Add(item.GetString() ?? "");
                    }
                }
            }
        }
        catch { /* 解析失败返回空 */ }
        return results;
    }
}

/// <summary>仅给 ISuggestionAdapter 内部使用的编码快捷方式。</summary>
internal static class SuggestionUrlEncoder
{
    public static string UrlEncode(string s)
    {
        if (string.IsNullOrEmpty(s))
            return "";
        var bytes = Encoding.UTF8.GetBytes(s);
        var sb = new StringBuilder(s.Length * 3);
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
