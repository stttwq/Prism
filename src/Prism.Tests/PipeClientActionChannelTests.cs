using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 审计批次 3 C1：交互式动作走独立管道连接，绝不占住搜索通道。
/// 用两个 armed 的假 broker listener 复现"属性页开着继续打字"场景：
/// 动作请求发出后故意不应答（模拟对话框未关），此时搜索必须照常往返。
/// </summary>
public sealed class PipeClientActionChannelTests
{
    private static string TempPipeName() => $"prism-test-c1-{Guid.NewGuid():N}";

    private static NamedPipeServerStream CreateServer(string name)
        => new(name, PipeDirection.InOut, 4, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);

    /// <summary>
    /// 一个假 broker 连接处理器：握手后按行读请求。
    /// search 立即应答；execute/reveal/run_action 先登记再等测试放行。
    /// </summary>
    private static async Task ServeAsync(
        NamedPipeServerStream server,
        TaskCompletionSource actionArrived,
        Task actionRelease,
        CancellationToken ct)
    {
        try
        {
            await server.WaitForConnectionAsync(ct).ConfigureAwait(false);
            var utf8 = new UTF8Encoding(false);
            using var reader = new StreamReader(server, utf8);
            using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };

            var hello = await reader.ReadLineAsync(ct).ConfigureAwait(false);
            if (hello is null) return;
            await writer.WriteLineAsync("{\"type\":\"hello\",\"protocol\":1}".AsMemory(), ct).ConfigureAwait(false);

            while (!ct.IsCancellationRequested)
            {
                var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                if (line is null) return;
                using var doc = JsonDocument.Parse(line);
                var type = doc.RootElement.GetProperty("type").GetString();
                if (type == "search")
                {
                    await writer.WriteLineAsync(
                        "{\"type\":\"search\",\"query\":\"q\",\"items\":[]}".AsMemory(), ct).ConfigureAwait(false);
                }
                else
                {
                    // 动作：登记后挂住不应答，等测试显式放行（= 系统对话框关闭）。
                    actionArrived.TrySetResult();
                    await actionRelease.WaitAsync(ct).ConfigureAwait(false);
                    await writer.WriteLineAsync("{\"type\":\"ok\"}".AsMemory(), ct).ConfigureAwait(false);
                }
            }
        }
        catch (OperationCanceledException) { /* 测试收尾 */ }
        catch (IOException) { /* 客户端拆连接 */ }
    }

    [Fact]
    public async Task Pending_Action_Does_Not_Block_Search()
    {
        var name = TempPipeName();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var actionArrived = new TaskCompletionSource();
        var release = new TaskCompletionSource();

        // 两个 armed listener：一条给搜索通道，一条给动作通道（对齐 broker 的 4 个槽位）。
        using var server1 = CreateServer(name);
        using var server2 = CreateServer(name);
        var serving = new[]
        {
            ServeAsync(server1, actionArrived, release.Task, cts.Token),
            ServeAsync(server2, actionArrived, release.Task, cts.Token),
        };

        using var client = new PipeClient(name);
        await client.ConnectInnerAsync(TimeSpan.FromSeconds(5), cts.Token); // 搜索通道
        Assert.True(client.IsConnected);

        // 交互式动作：请求送达假 broker 后永不应答，直到 release 放行。
        var action = client.ExecuteAsync(new ActionTarget("path", @"C:\demo.txt"), "demo", cts.Token);
        await actionArrived.Task.WaitAsync(TimeSpan.FromSeconds(10), cts.Token);
        Assert.False(action.IsCompleted);

        // 关键断言：动作在途期间搜索照常往返（修复前这里会一直等到动作结束）。
        var sw = Stopwatch.StartNew();
        var response = await client.SearchAsync("q", 10, cts.Token).WaitAsync(TimeSpan.FromSeconds(5), cts.Token);
        sw.Stop();
        Assert.Equal("q", response.Query);
        Assert.True(sw.Elapsed < TimeSpan.FromSeconds(3), $"搜索被动作阻塞：{sw.Elapsed}");
        Assert.False(action.IsCompleted); // 动作仍挂着，说明两者确实在不同连接上

        release.SetResult();
        await action.WaitAsync(TimeSpan.FromSeconds(10), cts.Token);

        cts.Cancel();
        await Task.WhenAll(serving);
    }

    /// <summary>
    /// 动作通道连不上（这里用"只有一个 listener，且已被搜索通道占用"制造）时，
    /// 退回搜索通道发送——动作不能因为第二条连接建不起来就彻底失败。
    /// </summary>
    [Fact]
    public async Task Action_Falls_Back_To_Query_Channel_When_Second_Connection_Unavailable()
    {
        var name = TempPipeName();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var actionArrived = new TaskCompletionSource();
        var release = new TaskCompletionSource();
        release.SetResult(); // 立即应答，只验证退化路径能走通

        // 单实例管道：搜索通道连上之后就再没有可用槽位。
        using var server = new NamedPipeServerStream(
            name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var serving = ServeAsync(server, actionArrived, release.Task, cts.Token);

        using var client = new PipeClient(name);
        await client.ConnectInnerAsync(TimeSpan.FromSeconds(5), cts.Token);

        await client.ExecuteAsync(new ActionTarget("path", @"C:\demo.txt"), "demo", cts.Token)
            .WaitAsync(TimeSpan.FromSeconds(15), cts.Token);
        Assert.True(actionArrived.Task.IsCompleted);

        cts.Cancel();
        await serving;
    }
}
