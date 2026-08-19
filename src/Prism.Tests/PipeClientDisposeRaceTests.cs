using System.Diagnostics;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// AUDIT-2026-08-18 C-D9: PipeClient.Dispose 与 watchdog tick 的竞态。
/// Dispose 置 _disposed 后，EnsureBackendRunning（tick 重连的拉进程路径）
/// 必须直接返回——否则 Kill 之后一个迟到的 tick 又拉起新 broker 成孤儿。
/// </summary>
public sealed class PipeClientDisposeRaceTests
{
    private static string TempPipeName() => $"prism-test-cd9-{Guid.NewGuid():N}";

    [Fact]
    public void Dispose_Then_EnsureBackendRunning_Never_Starts_A_Process()
    {
        var envBackup = Environment.GetEnvironmentVariable("PRISM_CORE_EXE");
        // LocateBackend 第一优先读 PRISM_CORE_EXE；指到本测试程序集只为通过
        // File.Exists 检查——真正的启动被注入的 ProcessStarterForTest 拦截，
        // 绝不会真正拉起 prism-core。
        Environment.SetEnvironmentVariable(
            "PRISM_CORE_EXE", typeof(PipeClientDisposeRaceTests).Assembly.Location);
        try
        {
            var client = new PipeClient(TempPipeName());
            var starts = 0;
            client.ProcessStarterForTest = _ =>
            {
                starts++;
                return null; // 走既有"启动失败"路径
            };

            // Dispose 前：starter 被调用（拉起路径可达）。
            Assert.Throws<IOException>(() => client.EnsureBackendRunning());
            Assert.Equal(1, starts);

            client.Dispose();

            // Dispose 后：EnsureBackendRunning 必须立即返回，绝不再触碰 starter。
            client.EnsureBackendRunning();
            Assert.Equal(1, starts);

            // Dispose 幂等（退出路径可能被 App 与单测各调一次）。
            client.Dispose();
        }
        finally
        {
            Environment.SetEnvironmentVariable("PRISM_CORE_EXE", envBackup);
        }
    }

    /// <summary>Dispose 排干在途 tick 的等待必须有界——这里锚定 Dispose 本身限时返回。</summary>
    [Fact]
    public void Dispose_Returns_Even_When_Watchdog_Was_Never_Started()
    {
        var client = new PipeClient(TempPipeName());
        var sw = Stopwatch.StartNew();
        client.Dispose();
        sw.Stop();
        Assert.True(sw.Elapsed < TimeSpan.FromSeconds(5));
    }
}
