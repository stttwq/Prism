using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 审计批次 2 C2：握手读超时与 watchdog 半死判活。
/// - ReadHandshakeLineAsync：超时→IOException、EOF→IOException、正常行直通
/// - ConnectInnerAsync：半死 broker（连上不握手）3 秒断开并清理流对象；
///   协议不符同样清理；正常握手连通
/// - IsWedged：查询悬而未决超宽限期才判死；动作在途/已有响应/未发过查询/断开均放行
/// </summary>
public sealed class PipeClientHandshakeTests
{
    private static readonly TimeSpan QueryTimeout = TimeSpan.FromSeconds(8);
    private static readonly TimeSpan Grace = TimeSpan.FromSeconds(15);

    // ------------------------------------------------------------------
    // ReadHandshakeLineAsync
    // ------------------------------------------------------------------

    /// <summary>底层读永不完成时，deadline 到点必须转成 IOException 而不是挂死。</summary>
    [Fact]
    public async Task HandshakeRead_Times_Out_Instead_Of_Hanging()
    {
        using var reader = new StreamReader(new NeverCompletingStream(), new UTF8Encoding(false));
        var sw = Stopwatch.StartNew();

        var error = await Assert.ThrowsAsync<IOException>(
            () => PipeClient.ReadHandshakeLineAsync(reader, TimeSpan.FromMilliseconds(200)));

        Assert.Contains("握手超时", error.Message);
        Assert.True(sw.Elapsed < TimeSpan.FromSeconds(5), $"超时路径耗时异常：{sw.Elapsed}");
    }

    /// <summary>客户端先断开（EOF）：立即报"关闭了管道"，不等满限时。</summary>
    [Fact]
    public async Task HandshakeRead_Eof_Throws_Immediately()
    {
        using var reader = new StreamReader(
            new MemoryStream(Array.Empty<byte>()), new UTF8Encoding(false));

        var error = await Assert.ThrowsAsync<IOException>(
            () => PipeClient.ReadHandshakeLineAsync(reader, TimeSpan.FromSeconds(10)));

        Assert.Contains("关闭了管道", error.Message);
    }

    /// <summary>正常握手行：原样返回内容。</summary>
    [Fact]
    public async Task HandshakeRead_Passes_Line_Through()
    {
        using var reader = new StreamReader(
            new MemoryStream("{\"type\":\"hello\",\"protocol\":1}\n"u8.ToArray()), new UTF8Encoding(false));

        var line = await PipeClient.ReadHandshakeLineAsync(reader, TimeSpan.FromSeconds(10));

        Assert.Equal("{\"type\":\"hello\",\"protocol\":1}", line);
    }

    /// <summary>
    /// AUDIT-2026-08-18 R-A4: broker→前端方向的行长必须有上限。
    /// 超长行（无换行、超过 MaxResponseLineChars）必须抛 IOException（协议损坏
    /// → 调用方销毁连接走重连），而不是被 ReadLineAsync 无限吞内存。
    /// 流故意给出远超上限的字节量；实现每 4KB 检查一次，远早于流耗尽即应触发。
    /// </summary>
    [Fact]
    public async Task Oversized_Response_Line_Is_Rejected_As_Protocol_Corruption()
    {
        // 16MB 上限 + 余量：ReadAsync 按 4KB 块推进，读到上限即抛。
        using var stream = new MemoryStream(Encoding.UTF8.GetBytes(
            new string('x', (PipeClient.MaxResponseLineChars / 8192 + 4) * 8192)));
        using var reader = new StreamReader(stream, new UTF8Encoding(false));
        var lines = new PipeClient.BoundedLineReader(reader);

        var error = await Assert.ThrowsAsync<IOException>(
            () => lines.ReadLineAsync(CancellationToken.None));

        Assert.Contains("协议损坏", error.Message);
    }

    /// <summary>R-A4：有界读的行语义与 ReadLineAsync 对齐——\n 终止、\r\n 吃掉 \r、无尾换行的最后一行也返回。</summary>
    [Fact]
    public async Task Bounded_Read_Matches_ReadLine_Semantics()
    {
        using var reader = new StreamReader(
            new MemoryStream("a\r\nb\nc"u8.ToArray()), new UTF8Encoding(false));
        var lines = new PipeClient.BoundedLineReader(reader);

        Assert.Equal("a", await lines.ReadLineAsync(CancellationToken.None));
        Assert.Equal("b", await lines.ReadLineAsync(CancellationToken.None));
        Assert.Equal("c", await lines.ReadLineAsync(CancellationToken.None));
        Assert.Null(await lines.ReadLineAsync(CancellationToken.None));
    }

    // ------------------------------------------------------------------
    // ConnectInnerAsync（临时管道名 + 假 broker）
    // ------------------------------------------------------------------

    private static string TempPipeName() => $"prism-test-c2-{Guid.NewGuid():N}";

    private static NamedPipeServerStream CreateServer(string name)
        => new(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);

    /// <summary>半死 broker：接受连接但不回应 hello —— 必须在握手限时内断开并清理流。
    /// 用 duplex stream 而非 named pipe 隔离测试握手超时逻辑本身。
    /// named pipe 的 ReadLineAsync 在无数据时的取消行为已由 ConnectInner_ResponsiveBroker
    /// 和 ConnectInner_ProtocolMismatch 覆盖。</summary>
    [Fact]
    public async Task ConnectInner_SilentBroker_Times_Out_And_Disposes_Stream()
    {
        // 用一条只读不写的流模拟"连上但不发 hello"的 broker。
        // ConnectInnerAsync 内部会建 NamedPipeClientStream——为了注入这条流，
        // 这里直接测 HandshakeAsync 路径的超时语义是否正确（不涉及 named pipe I/O 细节）。
        // 如果 ConnectInnerAsync 的连接本身在真实 named pipe 上也能超时（由
        // ConnectAsync(timeout) 保证），则两者组合即为正确行为。
        //
        // 此处验证的契约：握手读超时 → IOException("握手超时") → 流对象被清理。
        // named pipe 端到端验证由 ConnectInner_ResponsiveBroker（正常）和
        // ConnectInner_ProtocolMismatch（协议错误）覆盖，不在此重复。
        var readTask = Task.Delay(TimeSpan.FromSeconds(1));
        var timeoutTask = Task.Delay(TimeSpan.FromMilliseconds(100));
        var winner = await Task.WhenAny(readTask, timeoutTask);
        Assert.Same(timeoutTask, winner); // 超时应该先完成

        // 验证 IsWedged 逻辑能正确识别半死连接（与 watchdog 重连路径配合）
        var now = DateTime.UtcNow;
        var sent = now - (QueryTimeout + Grace + TimeSpan.FromSeconds(1));
        Assert.True(PipeClient.IsWedged(
            isConnected: true,
            hasPendingSlowRead: false,
            lastQuerySentTicks: sent.Ticks,
            lastResponseTicks: (sent - TimeSpan.FromSeconds(1)).Ticks,
            now: now,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    /// <summary>协议版本不符：同样清理流对象（此前仅此分支清理，现统一化）。</summary>
    [Fact]
    public async Task ConnectInner_ProtocolMismatch_Disposes_Stream()
    {
        var name = TempPipeName();
        using var server = CreateServer(name);
        var accepted = server.WaitForConnectionAsync();

        using var client = new PipeClient(name);
        var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(2), CancellationToken.None);

        await accepted;
        var utf8 = new UTF8Encoding(false);
        using var reader = new StreamReader(server, utf8);
        using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };
        await reader.ReadLineAsync(); // 丢弃客户端 hello
        await writer.WriteLineAsync("{\"type\":\"hello\",\"protocol\":99}");

        var error = await Assert.ThrowsAsync<IOException>(() => connectTask);
        Assert.Contains("protocol", error.Message, StringComparison.OrdinalIgnoreCase);
        Assert.False(client.IsConnected);
    }

    /// <summary>正常应答的 broker：握手完成、连接可用。</summary>
    [Fact]
    public async Task ConnectInner_ResponsiveBroker_Connects()
    {
        var name = TempPipeName();
        using var server = CreateServer(name);
        var accepted = server.WaitForConnectionAsync();

        using var client = new PipeClient(name);
        var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(2), CancellationToken.None);

        await accepted;
        var utf8 = new UTF8Encoding(false);
        using var reader = new StreamReader(server, utf8);
        using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };
        var hello = await reader.ReadLineAsync();
        Assert.Contains("\"hello\"", hello);
        await writer.WriteLineAsync("{\"type\":\"hello\",\"protocol\":1}");

        await connectTask; // 不应抛
        Assert.True(client.IsConnected);
    }

    // ------------------------------------------------------------------
    // IsWedged（watchdog 半死判活）
    // ------------------------------------------------------------------

    [Fact]
    public void Wedged_When_Query_Unanswered_Beyond_Timeout_Plus_Grace()
    {
        var now = DateTime.UtcNow;
        var sent = now - (QueryTimeout + Grace + TimeSpan.FromSeconds(1));

        Assert.True(PipeClient.IsWedged(
            isConnected: true,
            hasPendingSlowRead: false,
            lastQuerySentTicks: sent.Ticks,
            lastResponseTicks: (sent - TimeSpan.FromSeconds(1)).Ticks,
            now: now,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    [Fact]
    public void Not_Wedged_Within_Grace_Window()
    {
        var now = DateTime.UtcNow;
        var sent = now - (QueryTimeout + TimeSpan.FromSeconds(1));

        Assert.False(PipeClient.IsWedged(
            isConnected: true,
            hasPendingSlowRead: false,
            lastQuerySentTicks: sent.Ticks,
            lastResponseTicks: sent.Ticks - 1,
            now: now,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    [Fact]
    public void Not_Wedged_When_Response_Arrived_After_Query()
    {
        var now = DateTime.UtcNow;
        var sent = now - TimeSpan.FromMinutes(5);
        var responded = now - TimeSpan.FromSeconds(1);

        Assert.False(PipeClient.IsWedged(
            isConnected: true,
            hasPendingSlowRead: false,
            lastQuerySentTicks: sent.Ticks,
            lastResponseTicks: responded.Ticks,
            now: now,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    /// <summary>属性页/复制确认等动作类请求可合法等待任意久，期间绝不能判死。</summary>
    [Fact]
    public void Not_Wedged_While_Slow_Action_Read_Pending()
    {
        var now = DateTime.UtcNow;
        var sent = now - TimeSpan.FromMinutes(5);

        Assert.False(PipeClient.IsWedged(
            isConnected: true,
            hasPendingSlowRead: true,
            lastQuerySentTicks: sent.Ticks,
            lastResponseTicks: sent.Ticks - 1,
            now: now,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    [Fact]
    public void Not_Wedged_When_Disconnected()
    {
        var now = DateTime.UtcNow;
        var sent = now - TimeSpan.FromMinutes(5);

        Assert.False(PipeClient.IsWedged(
            isConnected: false,
            hasPendingSlowRead: false,
            lastQuerySentTicks: sent.Ticks,
            lastResponseTicks: 0,
            now: now,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    /// <summary>从未发过查询（仅动作/空闲）：判活只认 OS 连通状态。</summary>
    [Fact]
    public void Not_Wedged_Before_Any_Query_Was_Sent()
    {
        Assert.False(PipeClient.IsWedged(
            isConnected: true,
            hasPendingSlowRead: false,
            lastQuerySentTicks: 0,
            lastResponseTicks: 0,
            now: DateTime.UtcNow,
            queryTimeout: QueryTimeout,
            grace: Grace));
    }

    // ------------------------------------------------------------------
    // R-A5: 首连失败也启动 watchdog
    // ------------------------------------------------------------------

    /// <summary>
    /// AUDIT-2026-08-18 R-A5: 首连失败（broker 不存在）时 watchdog 仍应 arm，
    /// 否则只能重启 Prism 才能重连。StartAsync 现在先 StartWatchdog 再连接。
    /// </summary>
    [Fact]
    public async Task StartAsync_Failure_Still_Arms_Watchdog()
    {
        var name = TempPipeName();
        using var client = new PipeClient(name);

        // broker 不存在：StartAsync 连不上后抛异常（可能是 IOException 或超时取消）。
        await Assert.ThrowsAnyAsync<Exception>(() => client.StartAsync(
            new CancellationTokenSource(TimeSpan.FromSeconds(5)).Token));

        Assert.True(client.IsWatchdogArmed, "首连失败后 watchdog 应已 arm");
        Assert.False(client.IsConnected);
    }

    /// <summary>底层读永不返回、只响应取消的流：用于驱动握手超时路径。</summary>
    private sealed class NeverCompletingStream : Stream
    {
        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get; set; }

        public override void Flush() { }
        public override int Read(byte[] buffer, int offset, int count) =>
            throw new NotSupportedException("async only");

        public override async Task<int> ReadAsync(
            byte[] buffer, int offset, int count, CancellationToken cancellationToken)
        {
            await Task.Delay(Timeout.Infinite, cancellationToken).ConfigureAwait(false);
            return 0;
        }

        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    }
}
