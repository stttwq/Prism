using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 在线联想服务（G8）。WPF 用户会话单例，持有可取消 HTTP client。
/// 默认关闭，只在内置引擎（Bing/百度/Google）生效。
/// broker 和 indexer 不发联想请求。
/// </summary>
public interface ISuggestionService
{
    /// <summary>
    /// 发送联想请求。800ms 超时；取消/超时/断网/解析失败返回空列表，不抛异常。
    /// 返回的候选已经过验证和截断（最多 5 条）。
    /// </summary>
    /// <param name="engine">内置引擎名（"Bing" / "百度" / "Google"）。</param>
    /// <param name="query">查询词（已去掉关键词前缀）。</param>
    /// <param name="ct">取消令牌：查询变化时取消旧请求。</param>
    Task<IReadOnlyList<SuggestionItem>> GetSuggestionsAsync(
        string engine,
        string query,
        CancellationToken ct);
}

/// <summary>单条联想结果。</summary>
/// <param name="Text">联想文本。</param>
/// <param name="Url">选中后打开的完整 URL（由引擎 URL 模板 + 联想文本构造）。</param>
public sealed record SuggestionItem(string Text, string Url);
