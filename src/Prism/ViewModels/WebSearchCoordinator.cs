using Prism.Models;
using Prism.Services;

namespace Prism.ViewModels;

/// <summary>
/// P4-RESEARCH P4c: 网页联想的发起与身份治理，从 SearchViewModel 抽出。
/// 职责：每轮联想一个递增序号 + 取消令牌；迟到/被取代的响应在两处身份核查
/// （工作线程返回后、marshal 回 UI 后）被丢弃；结果行组装（BuildWebRows）。
/// UI 写入永远经注入的 <paramref name="marshalToUi"/>（生产 = Dispatcher.BeginInvoke
/// 回 UI 线程，测试 = 同步执行），coordinator 自身不依赖 Dispatcher。
/// </summary>
public sealed class WebSearchCoordinator
{
    private readonly ISuggestionService? _suggestions;
    private readonly Action<Action> _marshalToUi;

    private CancellationTokenSource? _cts;
    private int _seq;

    public WebSearchCoordinator(ISuggestionService? suggestions, Action<Action> marshalToUi)
    {
        _suggestions = suggestions;
        _marshalToUi = marshalToUi;
    }

    /// <summary>当前联想序号（测试可观测）。</summary>
    public int Sequence => _seq;

    /// <summary>
    /// 取消在途联想（新搜索/关闭面板时调用）。只 Cancel 不 Dispose：
    /// Task.Run 续体在返回后还会读 IsCancellationRequested 做身份核查，
    /// Dispose 会制造竞态；无定时器的 CTS 交给 GC 即可。
    /// </summary>
    public void CancelPending()
    {
        var cts = Interlocked.Exchange(ref _cts, null);
        try { cts?.Cancel(); } catch { /* ignore */ }
    }

    /// <summary>
    /// 发起一轮联想。<paramref name="stillCurrent"/> 在应用前重查（搜索序号 +
    /// 当前查询是否仍是最新），<paramref name="apply"/> 在 UI 线程收到最终候选。
    /// 联想失败静默放弃（调用方保持直接结果不动）。
    /// </summary>
    public void FetchSuggestions(
        WebModeResult webMode,
        Func<bool> stillCurrent,
        Action<IReadOnlyList<SuggestionItem>> apply)
    {
        var suggSeq = ++_seq;
        var cts = new CancellationTokenSource();
        _cts = cts;

        _ = Task.Run(async () =>
        {
            IReadOnlyList<SuggestionItem> suggestions;
            try
            {
                suggestions = await _suggestions!.GetSuggestionsAsync(
                    webMode.EngineName, webMode.QueryTerms, cts.Token).ConfigureAwait(false);
            }
            catch
            {
                // 联想失败静默保留直接结果，不显示干扰性错误。
                return;
            }

            // 第一道身份核查：工作线程返回后，用户可能已输入新查询。
            if (suggSeq != _seq) return;
            if (cts.IsCancellationRequested) return;
            if (!stillCurrent()) return;

            // 回 UI 线程应用（GetSuggestionsAsync 的续体在线程池上）。
            _marshalToUi(() =>
            {
                // 第二道身份核查：marshal 排队期间用户可能又敲了键。
                if (suggSeq != _seq) return;
                if (!stillCurrent()) return;
                apply(suggestions);
            });
        }, cts.Token);
    }

    /// <summary>
    /// 网页模式结果行：首行直接提交 + 联想行。行身份（RowKey）按槽位而非内容确定——
    /// 内容每按一键都变，但第 N 行始终是第 N 行，ResultList 才能原地更新而不重建容器
    /// （重建 = 图标空一帧 = 逐键闪烁）。
    /// </summary>
    public static List<SearchResult> BuildWebRows(
        SearchResult directResult,
        IReadOnlyList<SuggestionItem> suggestions,
        WebModeResult webMode)
    {
        var rows = new List<SearchResult>(suggestions.Count + 1) { directResult };
        for (var i = 0; i < suggestions.Count; i++)
        {
            var s = suggestions[i];
            rows.Add(new SearchResult(
                Kind: "web",
                Title: s.Text,
                Subtitle: s.Url,
                ExecuteId: s.Url,
                MatchSpans: BuildWebMatchSpans(s.Text, webMode.QueryTerms))
            {
                Target = new ActionTarget("web", s.Url),
                RowKey = $"web:sugg:{webMode.EngineName}:{i}",
            });
        }
        return rows;
    }

    /// <summary>网页模式标题中查询词的 UTF-16 匹配区间。</summary>
    public static int[] BuildWebMatchSpans(string title, string terms)
    {
        if (string.IsNullOrEmpty(terms))
            return [];
        var idx = title.IndexOf(terms, StringComparison.OrdinalIgnoreCase);
        if (idx < 0)
            return [];
        // 转换为 UTF-16 code unit 偏移（与 ipc::match_spans 约定一致）。
        var start = title[..idx].Length;
        return [start, terms.Length];
    }
}
