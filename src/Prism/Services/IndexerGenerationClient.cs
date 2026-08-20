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

    /// <summary>
    /// M5（审计3 2026-08-20）：单次请求的读超时。wait_generation 服务端最多 30s
    /// 回包，31s 预算只吞死连接、不误伤正常长轮询。
    /// </summary>
    private static readonly TimeSpan ReadTimeout = TimeSpan.FromSeconds(31);

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
        // G3（FRESH-AUDIT-2）：有界逐行读——此前裸 ReadLineAsync 无上限，
        // 任何能写这条管道的进程都可灌超长行撑爆前端内存（对齐 R-A4 的 broker 方向）。
        var lineReader = new PipeClient.BoundedLineReader(reader);

        var hello = await ExchangeAsync(
            stream, writer, lineReader, new { type = "hello", protocol = ProtocolVersion }, ct).ConfigureAwait(false);
        EnsureResponseType(hello, "hello");
        if (!hello.TryGetProperty("protocol", out var protocol)
            || !protocol.TryGetInt32(out var version)
            || version != ProtocolVersion)
        {
            throw new IOException("Indexer protocol version mismatch");
        }

        var status = await ExchangeAsync(
            stream, writer, lineReader, new { type = "status" }, ct).ConfigureAwait(false);
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
                stream,
                writer,
                lineReader,
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
        NamedPipeClientStream stream,
        StreamWriter writer,
        PipeClient.BoundedLineReader lineReader,
        object request,
        CancellationToken ct)
    {
        var json = JsonSerializer.Serialize(request);
        await writer.WriteLineAsync(json.AsMemory(), ct).ConfigureAwait(false);
        await writer.FlushAsync(ct).ConfigureAwait(false);

        // M5（审计3 2026-08-20）：读超时用 Task.WhenAny 竞速而非 CancellationToken——
        // ReadLineAsync(token) 在 NamedPipeClientStream 上不能可靠取消挂起的
        // overlapped I/O（.NET 已知限制，照抄 PipeChannel.HandshakeAsync 的模式）。
        // 半死的 indexer（连上但不再响应）此前会让 _loop 永久挂起，
        // SetActive(true) 见循环未完成直接 return 不换新连接，文件变更后
        // 结果列表静默陈旧直到重启。超时即 Dispose 底层 stream（CancelIoEx
        // 让挂起读立即返回），异常交 RunAsync 的 1s 退避重连。
        var readTask = lineReader.ReadLineAsync(CancellationToken.None);
        var timeoutTask = Task.Delay(ReadTimeout, ct);
        string? line;
        if (readTask == await Task.WhenAny(readTask, timeoutTask).ConfigureAwait(false))
        {
            line = await readTask.ConfigureAwait(false);
        }
        else
        {
            // 先判取消（延迟任务因 ct 完成时走正常的取消路径，由 using 释放流），
            // 否则按超时处理：销毁底层流让挂起的 ReadLineAsync 立即返回。
            ct.ThrowIfCancellationRequested();
            stream.Dispose();
            throw new IOException(
                $"Indexer generation connection timed out after {Math.Round(ReadTimeout.TotalSeconds)}s");
        }

        if (line is null)
            throw new IOException("Indexer closed the pipe before responding");

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
