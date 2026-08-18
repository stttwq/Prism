using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;

namespace Prism.Services;

/// <summary>
/// Long-polls the privileged indexer on a dedicated connection. This connection is
/// never shared with the broker request/response stream.
/// </summary>
public interface IIndexGenerationClient : IDisposable
{
    event Action<ulong>? GenerationChanged;
    void SetActive(bool active);
}

public sealed class IndexerGenerationClient : IIndexGenerationClient
{
    public const string FullPipeName = @"\\.\pipe\prism-indexer-v1";
    private const string PipeName = "prism-indexer-v1";
    /// <summary>
    /// 与 prism-core 的 <c>INDEXER_PROTOCOL</c>（<c>src/prism-core/src/lib.rs</c>，当前为 2）
    /// 保持一致。这是"单一真源"的镜像——Rust 侧是源头，C# 侧手动同步并在测试中锚定。
    /// </summary>
    private const int ProtocolVersion = 2;

    private readonly object _sync = new();
    private CancellationTokenSource? _loopCts;
    private Task? _loop;
    private long _activationId;
    private bool _disposed;

    public event Action<ulong>? GenerationChanged;

    public void SetActive(bool active)
    {
        lock (_sync)
        {
            if (_disposed) return;

            if (!active)
            {
                _activationId++;
                _loopCts?.Cancel();
                _loopCts?.Dispose();
                _loopCts = null;
                _loop = null;
                return;
            }

            if (_loop is { IsCompleted: false }) return;

            _loopCts?.Dispose();
            _loopCts = new CancellationTokenSource();
            var activationId = ++_activationId;
            _loop = RunAsync(activationId, _loopCts.Token);
        }
    }

    private async Task RunAsync(long activationId, CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            try
            {
                await PollConnectionAsync(activationId, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                return;
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine("[Prism] indexer generation connection: " + ex.Message);
                try
                {
                    await Task.Delay(TimeSpan.FromSeconds(1), ct).ConfigureAwait(false);
                }
                catch (OperationCanceledException) when (ct.IsCancellationRequested)
                {
                    return;
                }
            }
        }
    }

    private async Task PollConnectionAsync(long activationId, CancellationToken ct)
    {
        using var stream = new NamedPipeClientStream(
            ".", PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        await stream.ConnectAsync(2_000, ct).ConfigureAwait(false);

        var utf8 = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
        using var reader = new StreamReader(stream, utf8, leaveOpen: true);
        using var writer = new StreamWriter(stream, utf8, leaveOpen: true)
        {
            AutoFlush = false,
            NewLine = "\n",
        };

        var hello = await ExchangeAsync(
            writer, reader, new { type = "hello", protocol = ProtocolVersion }, ct).ConfigureAwait(false);
        EnsureResponseType(hello, "hello");
        if (!hello.TryGetProperty("protocol", out var protocol)
            || !protocol.TryGetInt32(out var version)
            || version != ProtocolVersion)
        {
            throw new IOException("Indexer protocol version mismatch");
        }

        var status = await ExchangeAsync(
            writer, reader, new { type = "status" }, ct).ConfigureAwait(false);
        EnsureResponseType(status, "status");
        var generation = ReadGeneration(status);
        // A change can land after the visible search but before this dedicated
        // connection reads status. Refresh once so that activation race cannot
        // leave the open result list stale until a second filesystem event.
        if (IsCurrentActivation(activationId, ct))
            GenerationChanged?.Invoke(generation);

        while (!ct.IsCancellationRequested)
        {
            var response = await ExchangeAsync(
                writer,
                reader,
                new { type = "wait_generation", after = generation, timeout_ms = 30_000 },
                ct).ConfigureAwait(false);
            EnsureResponseType(response, "generation");
            var next = ReadGeneration(response);
            if (next > generation && IsCurrentActivation(activationId, ct))
            {
                generation = next;
                GenerationChanged?.Invoke(generation);
            }
        }
    }

    private static async Task<JsonElement> ExchangeAsync(
        StreamWriter writer,
        StreamReader reader,
        object request,
        CancellationToken ct)
    {
        var json = JsonSerializer.Serialize(request);
        await writer.WriteLineAsync(json.AsMemory(), ct).ConfigureAwait(false);
        await writer.FlushAsync(ct).ConfigureAwait(false);
        var line = await reader.ReadLineAsync(ct).ConfigureAwait(false)
            ?? throw new IOException("Indexer closed the pipe before responding");

        using var document = JsonDocument.Parse(line);
        var root = document.RootElement;
        if (root.TryGetProperty("type", out var type)
            && string.Equals(type.GetString(), "error", StringComparison.Ordinal))
        {
            var message = root.TryGetProperty("message", out var value)
                ? value.GetString()
                : "Unknown indexer error";
            throw new IOException(message);
        }
        return root.Clone();
    }

    private static void EnsureResponseType(JsonElement response, string expected)
    {
        if (!response.TryGetProperty("type", out var type)
            || !string.Equals(type.GetString(), expected, StringComparison.Ordinal))
        {
            throw new IOException($"Unexpected indexer response; expected {expected}");
        }
    }

    private bool IsCurrentActivation(long activationId, CancellationToken ct)
    {
        lock (_sync)
        {
            return !_disposed && !ct.IsCancellationRequested && activationId == _activationId;
        }
    }

    private static ulong ReadGeneration(JsonElement response)
    {
        if (!response.TryGetProperty("generation", out var value)
            || !value.TryGetUInt64(out var generation))
        {
            throw new IOException("Indexer response omitted generation");
        }
        return generation;
    }

    public void Dispose()
    {
        lock (_sync)
        {
            if (_disposed) return;
            _disposed = true;
            _activationId++;
            _loopCts?.Cancel();
            _loopCts?.Dispose();
            _loopCts = null;
            _loop = null;
        }
    }
}
