using System.IO.Pipes;
using System.Text;
using System.Windows.Threading;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// AUDIT-2026-08-18 C-D8: 单实例管道的哑连接必须在 2s 读超时后被断开，
/// 不能占死监听槽让后续双开唤出全部失灵。
/// </summary>
public sealed class SingleInstanceDumbClientTests
{
    [Fact]
    public void Dumb_Client_Is_Dropped_And_Subsequent_Show_Still_Works()
    {
        Exception? error = null;
        var thread = new Thread(() =>
        {
            try { Run(); }
            catch (Exception e) { error = e; }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join(TimeSpan.FromSeconds(30));
        Assert.Null(error);
    }

    private static void Run()
    {
        // M2（全仓复审 2026-08-22）：注入随机 mutex/pipe 名——此前测试用生产全局名
        // Local\PrismSingleInstance / prism-foreground，在任何装着 Prism 的机器上
        // 与真进程抢锁必败（12ms 即输），也让哑客户端连上真实例的监听管道。
        var token = Guid.NewGuid().ToString("N");
        using var si = new SingleInstance(
            $@"Local\PrismTest_{token}",
            $"prism-test-{token}");
        Assert.True(si.TryAcquire());

        var shown = new ManualResetEventSlim(false);
        var dispatcher = Dispatcher.CurrentDispatcher;
        si.StartForegroundListener(dispatcher, () => shown.Set());

        // 1) 哑客户端：连上、不发任何数据、保持打开。
        var dumb = new NamedPipeClientStream(
            ".", $"prism-test-{token}", PipeDirection.Out, PipeOptions.Asynchronous);
        dumb.Connect(3000);

        // 2) 等 2s 读超时 + 余量：监听循环应已丢弃哑连接。
        Thread.Sleep(3500);

        // 3) 正常客户端发送 show —— 必须仍能唤出（泵消息让 BeginInvoke 执行）。
        using (var client = new NamedPipeClientStream(
            ".", $"prism-test-{token}", PipeDirection.Out, PipeOptions.Asynchronous))
        {
            client.Connect(3000);
            using var writer = new StreamWriter(client, new UTF8Encoding(false))
            {
                AutoFlush = true,
                NewLine = "\n",
            };
            writer.WriteLine("show");
        }

        var frame = new DispatcherFrame();
        var timeout = DateTime.UtcNow.AddSeconds(8);
        var pump = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
        pump.Tick += (_, _) =>
        {
            if (shown.IsSet || DateTime.UtcNow > timeout)
            {
                pump.Stop();
                frame.Continue = false;
            }
        };
        pump.Start();
        Dispatcher.PushFrame(frame);
        pump.Stop();

        Assert.True(shown.IsSet, "哑连接被超时断开后，后续 show 信号必须仍能唤出");
        dumb.Dispose();
    }
}
