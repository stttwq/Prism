using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// P4-RESEARCH P4c: WebSearchCoordinator 的身份治理与行组装。
/// - 新一轮联想发起后，旧一轮的迟到响应在两道身份核查处被丢弃
/// - 结果应用永远经过注入的 marshal（可观测）
/// - BuildWebRows 的行身份按槽位稳定（防 ResultList 重建闪烁的契约）
/// </summary>
public sealed class WebSearchCoordinatorTests
{
    private sealed class ControlledSuggestionService : ISuggestionService
    {
        public TaskCompletionSource<IReadOnlyList<SuggestionItem>> Pending { get; private set; } =
            new(TaskCreationOptions.RunContinuationsAsynchronously);

        public List<(string Engine, string Terms)> Calls { get; } = [];

        public Task<IReadOnlyList<SuggestionItem>> GetSuggestionsAsync(
            string engine, string query, CancellationToken ct)
        {
            Calls.Add((engine, query));
            Pending = new TaskCompletionSource<IReadOnlyList<SuggestionItem>>(
                TaskCreationOptions.RunContinuationsAsynchronously);
            return Pending.Task;
        }
    }

    private static WebModeResult Mode(string terms = "kw") =>
        new("b", "Bing", "https://www.bing.com/search?q={0}", terms, IsBuiltIn: true);

    /// <summary>新一轮发起后，旧一轮即使成功返回也必须被丢弃；新一轮正常应用。</summary>
    [Fact]
    public async Task Stale_Suggestions_Are_Dropped_When_A_Newer_Fetch_Started()
    {
        var service = new ControlledSuggestionService();
        var applied = new List<IReadOnlyList<SuggestionItem>>();
        var coordinator = new WebSearchCoordinator(service, action => action());

        coordinator.FetchSuggestions(
            Mode("old"), stillCurrent: () => true,
            apply: items => applied.Add(items));
        await Eventually(() => service.Calls.Count == 1);
        var round1 = service.Pending;
        Assert.Equal(1, coordinator.Sequence);

        // 用户敲了新键：新一轮 fetch bump 序号。
        coordinator.FetchSuggestions(
            Mode("new"), stillCurrent: () => true,
            apply: items => applied.Add(items));
        await Eventually(() => service.Calls.Count == 2);
        var round2 = service.Pending;
        Assert.Equal(2, coordinator.Sequence);

        // 旧请求先返回：身份不符，必须被丢弃。
        round1.SetResult([new SuggestionItem("old-late", "https://old")]);
        await Task.Delay(50);
        Assert.Empty(applied);

        // 新请求返回：应用。
        round2.SetResult([new SuggestionItem("new-fresh", "https://new")]);
        await Eventually(() => applied.Count == 1);
        Assert.Equal("new-fresh", applied[0][0].Text);
    }

    /// <summary>stillCurrent 在应用前重查：查询已变（回调返回 false）时丢弃，即使 seq 未变。</summary>
    [Fact]
    public async Task Stale_Query_Is_Dropped_By_StillCurrent_Guard()
    {
        var service = new ControlledSuggestionService();
        var applied = false;
        var coordinator = new WebSearchCoordinator(service, action => action());
        var current = true;

        coordinator.FetchSuggestions(
            Mode(), stillCurrent: () => current, apply: _ => applied = true);
        await Eventually(() => service.Calls.Count == 1);

        current = false; // 模拟用户已输入新查询
        service.Pending.SetResult([new SuggestionItem("x", "https://x")]);
        await Task.Delay(50);

        Assert.False(applied, "查询已变化，迟到联想不得应用");
    }

    /// <summary>结果应用必须经注入的 marshal（生产是 Dispatcher.BeginInvoke，这里观测调用）。</summary>
    [Fact]
    public async Task Apply_Always_Goes_Through_The_Injected_Marshal()
    {
        var service = new ControlledSuggestionService();
        var marshaled = 0;
        var applied = 0;
        var coordinator = new WebSearchCoordinator(service, action =>
        {
            marshaled++;
            action();
        });

        coordinator.FetchSuggestions(Mode(), stillCurrent: () => true, apply: _ => applied++);
        await Eventually(() => service.Calls.Count == 1);
        service.Pending.SetResult([new SuggestionItem("x", "https://x")]);

        await Eventually(() => applied == 1);
        Assert.Equal(1, marshaled);
    }

    /// <summary>行身份契约：第 N 行的 RowKey 恒为 web:sugg:engine:N，与内容无关。</summary>
    [Fact]
    public void BuildWebRows_Keys_Rows_By_Slot_Not_Content()
    {
        var direct = new SearchResult("web", "在 Bing 中搜索：kw", "https://u", "https://u", [])
        {
            RowKey = "web:direct:Bing",
        };
        var rows = WebSearchCoordinator.BuildWebRows(
            direct,
            [new SuggestionItem("a", "https://a"), new SuggestionItem("b", "https://b")],
            Mode());

        Assert.Equal(3, rows.Count);
        Assert.Equal("web:direct:Bing", rows[0].RowKey);
        Assert.Equal("web:sugg:Bing:0", rows[1].RowKey);
        Assert.Equal("web:sugg:Bing:1", rows[2].RowKey);
        Assert.Equal("a", rows[1].Title);
    }

    private static async Task Eventually(Func<bool> condition)
    {
        for (var i = 0; i < 200 && !condition(); i++)
            await Task.Delay(5);
        Assert.True(condition());
    }
}
