using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// AUDIT-2026-08-18 R-A8: 孤儿 broker 复用前的收编。
/// - 握手成功后 ServerProcessId 必须指向真实管道服务端进程
/// - 非 prism-core 的管道服务端（本测试进程）保守复用：不收编也不杀
/// - JobObjectGuard.TryAdopt 端到端：外部进程收编后随 Job Dispose 被 OS 回收
/// </summary>
public sealed class OrphanBrokerAdoptionTests
{
    private static string TempPipeName() => $"prism-test-ra8-{Guid.NewGuid():N}";

    /// <summary>握手成功后服务端 PID 指向管道 owner（此处的 in-proc server = 测试进程）。</summary>
    [Fact]
    public async Task ServerProcessId_Points_At_The_Pipe_Owner()
    {
        var name = TempPipeName();
        using var server = new NamedPipeServerStream(
            name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var accepted = server.WaitForConnectionAsync();

        using var client = new PipeClient(name);
        var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(2), CancellationToken.None);

        await accepted;
        var utf8 = new UTF8Encoding(false);
        using var reader = new StreamReader(server, utf8);
        using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };
        await reader.ReadLineAsync(); // 丢弃客户端 hello
        await writer.WriteLineAsync("{\"type\":\"hello\",\"protocol\":1}");

        await connectTask;

        Assert.True(client.IsConnected);
        Assert.Equal(Environment.ProcessId, client.QueryServerProcessId);
    }

    /// <summary>
    /// 服务端名字不是 prism-core（这里是测试进程本身）：AdoptExistingServer 必须
    /// 保守复用（返回 true）且不接管进程对象——绝不能对未知进程收编或下杀手。
    /// </summary>
    [Fact]
    public async Task Adopt_Leaves_Non_Broker_Server_Untouched()
    {
        var name = TempPipeName();
        using var server = new NamedPipeServerStream(
            name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var accepted = server.WaitForConnectionAsync();

        using var client = new PipeClient(name);
        var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(2), CancellationToken.None);

        await accepted;
        var utf8 = new UTF8Encoding(false);
        using var reader = new StreamReader(server, utf8);
        using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };
        await reader.ReadLineAsync();
        await writer.WriteLineAsync("{\"type\":\"hello\",\"protocol\":1}");
        await connectTask;

        Assert.True(client.AdoptExistingServer(Environment.ProcessId), "非 broker 服务端应保守复用");
        Assert.False(client.HasBackendProcess, "不得接管未知进程的生命周期");
    }

    /// <summary>
    /// Bug 3：移除 KILL_ON_JOB_CLOSE 后，Job Dispose 不再杀关联进程。收编的外部进程
    /// 在 Job 句柄关闭后仍存活——这验证用户打开的应用不会随 Prism 退出被杀。
    /// </summary>
    [Fact]
    public void TryAdopt_External_Process_Survives_Job_Dispose()
    {
        using var victim = Process.Start(new ProcessStartInfo
        {
            FileName = "cmd.exe",
            Arguments = "/c ping -n 60 127.0.0.1 > nul",
            UseShellExecute = false,
            CreateNoWindow = true,
        });
        Assert.NotNull(victim);

        using (var guard = new JobObjectGuard())
        {
            Assert.True(guard.TryAdopt(victim!.Id), "同用户外部进程应收编成功");
        } // Dispose 关闭 Job 句柄——不再设 KILL_ON_JOB_CLOSE，victim 不被杀

        // 给 OS 一个短暂窗口确认它没被杀，然后断言仍存活。
        Assert.False(victim.WaitForExit(TimeSpan.FromSeconds(2)), "移除 KILL_ON_JOB_CLOSE 后收编进程应存活");

        try
        {
            if (!victim.HasExited)
                victim.Kill(entireProcessTree: true);
        }
        catch { /* 测试结束兜底清理 */ }
    }
}
