using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// K0 C10-C11：PipeClient 命令功能握手 + SearchPayload 字节级对齐。
/// </summary>
public sealed class PipeClientCommandFeatureTests
{
    private static string TempPipeName() => $"prism-test-cmdfeat-{Guid.NewGuid():N}";

    private static NamedPipeServerStream CreateServer(string name)
        => new(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);

    /// <summary>C10：握手时 client 发 capabilities 含 commands_v1；broker 回 features 含 commands_v1 → CommandsAvailable=true。</summary>
    [Fact]
    public async Task Handshake_Negotiates_Commands_V1()
    {
        var name = TempPipeName();
        using var server = CreateServer(name);
        var accepted = server.WaitForConnectionAsync();

        using var client = new PipeClient(name);
        var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(5), CancellationToken.None);

        await accepted;
        var utf8 = new UTF8Encoding(false);
        using var reader = new StreamReader(server, utf8);
        using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };

        var hello = await reader.ReadLineAsync();
        Assert.NotNull(hello);
        Assert.Contains("hello", hello);
        // capabilities 必须在 client hello 里
        using var helloDoc = JsonDocument.Parse(hello!);
        Assert.True(helloDoc.RootElement.TryGetProperty("capabilities", out var caps));
        Assert.Contains("commands_v1",
            caps.EnumerateArray().Select(c => c.GetString()));

        await writer.WriteLineAsync(
            """{"type":"hello","protocol":1,"features":["commands_v1"],"command_catalog_generation":7,"build_id":"test-build"}""");

        await connectTask;

        Assert.True(client.CommandsAvailable);
        Assert.Equal(7ul, client.PipeCommandCatalogGeneration);
        Assert.Equal("test-build", client.PipeBuildId);
    }

    /// <summary>C10b：broker 不回 features → CommandsAvailable=false（旧 broker 兼容）。</summary>
    [Fact]
    public async Task Handshake_Without_Features_Not_Available()
    {
        var name = TempPipeName();
        using var server = CreateServer(name);
        var accepted = server.WaitForConnectionAsync();

        using var client = new PipeClient(name);
        var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(5), CancellationToken.None);

        await accepted;
        var utf8 = new UTF8Encoding(false);
        using var reader = new StreamReader(server, utf8);
        using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };
        await reader.ReadLineAsync(); // 丢弃 client hello
        await writer.WriteLineAsync("""{"type":"hello","protocol":1}""");

        await connectTask;

        Assert.False(client.CommandsAvailable);
        Assert.Null(client.PipeCommandCatalogGeneration);
        Assert.Null(client.PipeBuildId);
    }

    /// <summary>C10c：AdvertiseCommandCapability=false → client hello 不含 capabilities。</summary>
    [Fact]
    public async Task Advertise_Off_Hides_Capabilities()
    {
        var original = PipeClient.AdvertiseCommandCapability;
        PipeClient.AdvertiseCommandCapability = false;
        try
        {
            var name = TempPipeName();
            using var server = CreateServer(name);
            var accepted = server.WaitForConnectionAsync();

            using var client = new PipeClient(name);
            var connectTask = client.ConnectInnerAsync(TimeSpan.FromSeconds(5), CancellationToken.None);

            await accepted;
            var utf8 = new UTF8Encoding(false);
            using var reader = new StreamReader(server, utf8);
            using var writer = new StreamWriter(server, utf8) { AutoFlush = true, NewLine = "\n" };

            var hello = await reader.ReadLineAsync();
            Assert.NotNull(hello);
            using var helloDoc = JsonDocument.Parse(hello!);
            Assert.False(helloDoc.RootElement.TryGetProperty("capabilities", out _),
                "AdvertiseCommandCapability=false 时 hello 不应含 capabilities");

            await writer.WriteLineAsync("""{"type":"hello","protocol":1}""");
            await connectTask;

            Assert.False(client.CommandsAvailable);
        }
        finally
        {
            PipeClient.AdvertiseCommandCapability = original;
        }
    }

    /// <summary>C11：commandsAvailable=false（未协商）+ 无 CommandContext → payload 不含 command_context（字节级对齐旧格式）。</summary>
    [Fact]
    public void SearchPayload_No_Commands_Byte_Identical_To_Legacy()
    {
        var ctx = SearchContext.Default;
        var payload = PipeClient.SearchPayload("test", 50, ctx, commandsAvailable: false);

        Assert.False(payload.ContainsKey("command_context"));
        // 基本字段在
        Assert.Equal("search", payload["type"]);
        Assert.Equal("test", payload["query"]);
        Assert.Equal(50, payload["max"]);
    }

    /// <summary>C11b：commandsAvailable=true + CommandContext 有值 → payload 含 command_context。</summary>
    [Fact]
    public void SearchPayload_With_Command_Context_Includes_It()
    {
        var cmdCtx = new CommandSearchContext(
            CurrentFolder: @"C:\Users",
            HostKind: "explorer",
            HostCapabilities: new[] { "reveal" });
        var ctx = new SearchContext(
            SearchContext.AllMode, null, [], 1, cmdCtx);

        var payload = PipeClient.SearchPayload("test", 50, ctx, commandsAvailable: true);

        Assert.True(payload.ContainsKey("command_context"));
        var cmd = (Dictionary<string, object?>)payload["command_context"]!;
        Assert.Equal(@"C:\Users", cmd["current_folder"]);
        Assert.Equal("explorer", cmd["host_kind"]);
        Assert.NotNull(cmd["host_capabilities"]);
    }

    /// <summary>C11c：commandsAvailable=true 但 CommandContext=null → payload 不含 command_context。</summary>
    [Fact]
    public void SearchPayload_Available_But_No_Context_Omits_It()
    {
        var ctx = SearchContext.Default;
        var payload = PipeClient.SearchPayload("test", 50, ctx, commandsAvailable: true);

        Assert.False(payload.ContainsKey("command_context"));
    }

    /// <summary>C11d：commandsAvailable=true + 空 CommandContext（全 null/空）→ 不含 command_context（空对象不如不加）。</summary>
    [Fact]
    public void SearchPayload_Empty_Command_Context_Omits_It()
    {
        var cmdCtx = new CommandSearchContext(null, null, Array.Empty<string>());
        var ctx = new SearchContext(SearchContext.AllMode, null, [], 1, cmdCtx);

        var payload = PipeClient.SearchPayload("test", 50, ctx, commandsAvailable: true);

        Assert.False(payload.ContainsKey("command_context"));
    }
}
