using Prism.Models;

namespace Prism.Services;

public sealed record SearchFilterOption(string Field, string Value);

public sealed record SearchContext(
    string Mode,
    string? Root,
    IReadOnlyList<SearchFilterOption> Filters,
    int SortVersion)
{
    public static SearchContext Default { get; } = new("all", null, [], 1);

    public bool IsEquivalentTo(SearchContext other) =>
        string.Equals(Mode, other.Mode, StringComparison.Ordinal)
        && string.Equals(Root, other.Root, StringComparison.Ordinal)
        && SortVersion == other.SortVersion
        && Filters.SequenceEqual(other.Filters);
}

public interface ISearchClient
{
    bool IsConnected { get; }
    Task StartAsync(CancellationToken ct = default);
    Task<SearchResponse> SearchAsync(
        string query,
        int max,
        SearchContext context,
        CancellationToken ct = default);
    Task ExecuteAsync(ActionTarget target, CancellationToken ct = default);
    Task RevealAsync(ActionTarget target, CancellationToken ct = default);
    Task<IReadOnlyList<ActionItem>> GetActionsAsync(ActionTarget target, CancellationToken ct = default);
    Task RunActionAsync(ActionTarget target, string action, CancellationToken ct = default);
}

public interface IDebounceTimer
{
    void Restart();
    void Stop();
}

public interface IDebounceTimerFactory
{
    IDebounceTimer Create(TimeSpan interval, Action callback);
}

public interface ISearchScheduler
{
    Task Delay(TimeSpan delay, CancellationToken ct = default);
}

public sealed class SearchScheduler : ISearchScheduler
{
    public Task Delay(TimeSpan delay, CancellationToken ct = default) => Task.Delay(delay, ct);
}
