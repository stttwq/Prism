using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 命名管道客户端 + 后端进程守护。
/// 第四步：search / execute / reveal；断线时抛 IOException，由调用方决定是否重连。
/// </summary>
public sealed class PipeClient : ISearchClient, IDisposable
{
    private const string PipeName = "prism-core"; // 完整名 \\.\pipe\prism-core
    private const int ProtocolVersion = 1;

    private Process? _backend;
    private NamedPipeClientStream? _stream;
    private StreamReader? _reader;
    private StreamWriter? _writer;
    private readonly SemaphoreSlim _ioLock = new(1, 1);

    private System.Threading.Timer? _watchdog;
    private int _consecutivePingFailures;
    private bool _wasConnected;

    /// <summary>
    /// 后端连接状态变化通知。true=已连上，false=断开/重连失败。
    /// 由 watchdog 和首次连接触发，供上层（App）更新 UI 状态。
    /// </summary>
    public event Action<bool>? ConnectionChanged;

    /// <summary>实际使用的后端可执行文件路径，供诊断显示。</summary>
    public string BackendPath { get; private set; } = "";

    /// <summary>管道是否已连接。</summary>
    public bool IsConnected => _stream is { IsConnected: true };

    /// <summary>拉起后端（若未运行）并连接管道。</summary>
    public async Task StartAsync(CancellationToken ct = default)
    {
        EnsureBackendRunning();
        await ConnectAsync(TimeSpan.FromSeconds(10), ct).ConfigureAwait(false);
        StartWatchdog();
    }

    /// <summary>
    /// 启动后台 watchdog：每 15 秒 ping 一次后端，连续 2 次失败则重连/重启 core。
    /// 解决"退出后重开 core 未运行"——core 崩溃或启动失败时自动恢复，无需等用户触发搜索。
    /// </summary>
    private void StartWatchdog()
    {
        _wasConnected = true;
        _watchdog?.Dispose();
        _watchdog = new System.Threading.Timer(
            callback: _ => _ = WatchdogTickAsync(),
            state: null,
            dueTime: TimeSpan.FromSeconds(15),
            period: TimeSpan.FromSeconds(15));
    }

    private async Task WatchdogTickAsync()
    {
        // watchdog 不持 UI 线程；所有管道操作经 _ioLock 串行化。
        try
        {
            await _ioLock.WaitAsync().ConfigureAwait(false);
            try
            {
                if (_stream is not { IsConnected: true })
                {
                    // 管道已断——直接进入重连路径。
                    await ReconnectCoreAsync().ConfigureAwait(false);
                    return;
                }

                try
                {
                    // 复用 SendAsync（已配对读写，取消安全）做一次 ping。
                    var resp = await SendAsync(new { type = "ping" }, CancellationToken.None)
                        .ConfigureAwait(false);
                    _ = resp.GetProperty("version").GetString();
                    _consecutivePingFailures = 0;
                    NotifyConnection(true);
                }
                catch
                {
                    _consecutivePingFailures++;
                    if (_consecutivePingFailures >= 2)
                    {
                        await ReconnectCoreAsync().ConfigureAwait(false);
                        _consecutivePingFailures = 0;
                    }
                }
            }
            finally
            {
                _ioLock.Release();
            }
        }
        catch
        {
            // watchdog 自身异常不应崩溃进程。
        }
    }

    /// <summary>重连后端：释放旧管道，必要时重新拉起 core，再连新管道。</summary>
    private async Task ReconnectCoreAsync()
    {
        DisposeStreamOnly();
        try
        {
            EnsureBackendRunning();
            await ConnectAsync(TimeSpan.FromSeconds(10), CancellationToken.None).ConfigureAwait(false);
            NotifyConnection(true);
        }
        catch
        {
            NotifyConnection(false);
        }
    }

    private void NotifyConnection(bool connected)
    {
        if (connected == _wasConnected) return;
        _wasConnected = connected;
        ConnectionChanged?.Invoke(connected);
    }

    /// <summary>连接命名管道，UTF-8 无 BOM，按行(\n)收发。</summary>
    private async Task ConnectAsync(TimeSpan timeout, CancellationToken ct)
    {
        DisposeStreamOnly();

        var stream = new NamedPipeClientStream(
            ".", PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);

        await stream.ConnectAsync((int)timeout.TotalMilliseconds, ct).ConfigureAwait(false);

        var utf8 = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
        _stream = stream;
        _reader = new StreamReader(stream, utf8);
        _writer = new StreamWriter(stream, utf8) { AutoFlush = false, NewLine = "\n" };

        var hello = await SendAsync(
            new { type = "hello", protocol = ProtocolVersion }, ct).ConfigureAwait(false);
        if (!hello.TryGetProperty("type", out var type)
            || type.GetString() != "hello"
            || !hello.TryGetProperty("protocol", out var protocol)
            || !protocol.TryGetInt32(out var version)
            || version != ProtocolVersion)
        {
            DisposeStreamOnly();
            throw new IOException("Broker protocol version mismatch");
        }
    }

    /// <summary>发送 ping，返回后端版本号。</summary>
    public async Task<string> PingAsync(CancellationToken ct = default)
    {
        var resp = await SendAsync(new { type = "ping" }, ct).ConfigureAwait(false);
        return resp.GetProperty("version").GetString() ?? "";
    }

    /// <summary>即时搜索，返回后端原始结果（不含前端"展示更多"行）。</summary>
    public async Task<SearchResponse> SearchAsync(string query, int max = 100, CancellationToken ct = default)
    {
        return await SearchAsync(query, max, SearchContext.Default, ct).ConfigureAwait(false);
    }

    public async Task<SearchResponse> SearchAsync(
        string query,
        int max,
        SearchContext context,
        CancellationToken ct = default)
    {
        var resp = await SendAsync(SearchPayload(query, max, context), ct).ConfigureAwait(false);
        return ParseSearchResponse(resp, query);
    }

    /// <summary>
    /// 组装 search 请求。`root` 只在真正限定当前目录时出现：范围为全局时字段整体缺失，
    /// 与加入 root 之前的线上格式逐字节一致，也保证「UI 说全局」与「后端搜全局」不会背离。
    /// </summary>
    internal static Dictionary<string, object?> SearchPayload(string query, int max, SearchContext context)
    {
        var payload = new Dictionary<string, object?>
        {
            ["type"] = "search",
            ["query"] = query,
            ["max"] = max,
        };
        if (context.Filters.Count > 0)
        {
            payload["filters"] = context.Filters.Select(filter => new
            {
                field = filter.Field,
                value = filter.Value,
            }).ToArray();
        }
        if (!string.IsNullOrWhiteSpace(context.Root))
            payload["root"] = context.Root;
        // G5: only an explicit non-default mode goes on the wire. A global search payload
        // therefore stays byte-identical to the pre-G5 format, so an older broker that
        // never learned `mode` behaves exactly as before.
        if (!string.Equals(context.Mode, SearchContext.AllMode, StringComparison.Ordinal))
            payload["mode"] = context.Mode;
        return payload;
    }

    internal static SearchResponse ParseSearchResponse(JsonElement resp, string query)
    {
        var echo = resp.TryGetProperty("query", out var q) ? q.GetString() ?? query : query;
        var indexing = resp.TryGetProperty("is_indexing", out var ix) && ix.ValueKind == JsonValueKind.True;
        var indexError = resp.TryGetProperty("index_error", out var error)
            && error.ValueKind == JsonValueKind.String
            ? error.GetString()
            : null;
        var truncated = resp.TryGetProperty("is_truncated", out var truncatedValue)
            && truncatedValue.ValueKind == JsonValueKind.True;
        ulong? generation = resp.TryGetProperty("index_generation", out var generationValue)
            && generationValue.TryGetUInt64(out var parsedGeneration)
            ? parsedGeneration
            : null;
        var items = new List<SearchResult>();
        if (resp.TryGetProperty("items", out var arr) && arr.ValueKind == JsonValueKind.Array)
        {
            foreach (var el in arr.EnumerateArray())
                items.Add(ParseResult(el));
        }
        return new SearchResponse(
            echo,
            items,
            indexing,
            indexError,
            truncated,
            generation,
            ParseProgress(resp),
            ReadOptionalString(resp, "pinyin_status"),
            ReadOptionalString(resp, "history_status"),
            RootRejectionCodes.Parse(ReadOptionalString(resp, "root_rejection")),
            ReadOptionalString(resp, "root_message"));
    }

    private static string? ReadOptionalString(JsonElement owner, string name) =>
        owner.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString()
            : null;

    /// <summary>
    /// 解析可选的 index_progress。整体缺失或字段缺失都不报错：旧后端不发这个字段，
    /// 首次安装也没有记录总量可估算。
    /// </summary>
    private static IndexProgress? ParseProgress(JsonElement resp)
    {
        if (!resp.TryGetProperty("index_progress", out var progress)
            || progress.ValueKind != JsonValueKind.Object)
        {
            return null;
        }

        return new IndexProgress(
            Scanned: ReadUInt64(progress, "scanned") ?? 0,
            TotalEstimate: ReadUInt64(progress, "total_estimate") ?? 0,
            VolumesTotal: ReadInt32(progress, "volumes_total"),
            VolumesDone: ReadInt32(progress, "volumes_done"),
            CurrentVolume: progress.TryGetProperty("current_volume", out var volume)
                && volume.ValueKind == JsonValueKind.String
                ? volume.GetString()
                : null);
    }

    private static ulong? ReadUInt64(JsonElement owner, string name) =>
        owner.TryGetProperty(name, out var value) && value.TryGetUInt64(out var parsed)
            ? parsed
            : null;

    private static int? ReadInt32(JsonElement owner, string name) =>
        owner.TryGetProperty(name, out var value) && value.TryGetInt32(out var parsed)
            ? parsed
            : null;

    /// <summary>打开文件/文件夹/程序。</summary>
    public async Task ExecuteAsync(ActionTarget target, CancellationToken ct = default)
    {
        await SendAsync(new { type = "execute", target = TargetPayload(target) }, ct).ConfigureAwait(false);
    }

    /// <summary>在资源管理器中定位文件。</summary>
    public async Task RevealAsync(ActionTarget target, CancellationToken ct = default)
    {
        await SendAsync(new { type = "reveal", target = TargetPayload(target) }, ct).ConfigureAwait(false);
    }

    /// <summary>
    /// 通知后端热重载网页引擎列表（步骤 8）。
    /// 字段名与前端 <see cref="WebEngine"/> / settings.json PascalCase 对齐，
    /// 后端 IPC 用 serde alias 接收。
    /// </summary>
    public async Task ReloadEnginesAsync(IReadOnlyList<WebEngine> engines, CancellationToken ct = default)
    {
        var payload = engines.Select(e => new
        {
            Keyword = e.Keyword,
            Name = e.Name,
            UrlTemplate = e.UrlTemplate,
        }).ToArray();
        await SendAsync(new { type = "reload_engines", engines = payload }, ct).ConfigureAwait(false);
    }

    public async Task UpdatePreferencesAsync(
        bool historyEnabled,
        bool pinyinEnabled,
        CancellationToken ct = default)
    {
        await SendAsync(new
        {
            type = "update_preferences",
            history_enabled = historyEnabled,
            pinyin_enabled = pinyinEnabled,
        }, ct).ConfigureAwait(false);
    }

    public async Task ClearHistoryAsync(CancellationToken ct = default)
    {
        await SendAsync(new { type = "clear_history" }, ct).ConfigureAwait(false);
    }

    /// <summary>请求某文件/文件夹的动作列表（→ 键动作面板）。</summary>
    public async Task<IReadOnlyList<ActionItem>> GetActionsAsync(ActionTarget target, CancellationToken ct = default)
    {
        var resp = await SendAsync(new { type = "actions", target = TargetPayload(target) }, ct).ConfigureAwait(false);
        var items = new List<ActionItem>();
        if (resp.TryGetProperty("items", out var arr) && arr.ValueKind == JsonValueKind.Array)
        {
            foreach (var el in arr.EnumerateArray())
                items.Add(ParseAction(el));
        }
        return items;
    }

    /// <summary>执行动作面板中的某一项。</summary>
    public async Task RunActionAsync(ActionTarget target, string action, CancellationToken ct = default)
    {
        await RunActionAsync(target, action, ActionArgs.Empty, ct).ConfigureAwait(false);
    }

    /// <summary>执行动作面板中的某一项，携带动作参数（destination/new_name）。</summary>
    public async Task RunActionAsync(ActionTarget target, string action, ActionArgs args, CancellationToken ct = default)
    {
        object payload;
        if (args is { Destination: null, NewName: null })
        {
            payload = new { type = "run_action", target = TargetPayload(target), action };
        }
        else
        {
            object? argsObj = (args.Destination, args.NewName) switch
            {
                (not null, null) => new { destination = args.Destination },
                (null, not null) => new { new_name = args.NewName },
                (not null, not null) => new { destination = args.Destination, new_name = args.NewName },
                _ => null,
            };
            payload = new { type = "run_action", target = TargetPayload(target), action, args = argsObj };
        }
        await SendAsync(payload, ct).ConfigureAwait(false);
    }

    /// <summary>
    /// G5：把枚举 token 换成 broker 已复核的句柄。
    ///
    /// 窗口不走 execute：激活受 Windows 前台规则约束，只能由前台进程（本进程）完成，
    /// 所以 broker 只复核并交出句柄。
    /// </summary>
    public async Task<WindowHandleInfo> ResolveWindowAsync(
        ActionTarget target,
        CancellationToken ct = default)
    {
        var resp = await SendAsync(
            new { type = "resolve_window", target = TargetPayload(target) },
            ct).ConfigureAwait(false);
        return ParseWindowHandle(resp);
    }

    /// <summary>G5：激活成功后回报，由 broker 写窗口历史。</summary>
    public async Task RecordWindowSwitchAsync(ActionTarget target, CancellationToken ct = default)
    {
        await SendAsync(
            new { type = "record_window_switch", target = TargetPayload(target) },
            ct).ConfigureAwait(false);
    }

    internal static WindowHandleInfo ParseWindowHandle(JsonElement resp)
    {
        if (!resp.TryGetProperty("handle", out var handleValue)
            || !handleValue.TryGetUInt64(out var handle)
            || handle == 0)
        {
            throw new InvalidOperationException("后端未返回可用的窗口句柄");
        }
        var pid = resp.TryGetProperty("pid", out var pidValue) && pidValue.TryGetUInt32(out var parsedPid)
            ? parsedPid
            : 0u;
        return new WindowHandleInfo(
            (IntPtr)handle,
            pid,
            ReadOptionalString(resp, "title") ?? "",
            resp.TryGetProperty("is_minimized", out var minimized)
                && minimized.ValueKind == JsonValueKind.True);
    }

    internal static object TargetPayload(ActionTarget target) => new
    {
        kind = target.Kind,
        value = target.Value,
    };

    private static ActionItem ParseAction(JsonElement el)
    {
        var id = el.TryGetProperty("id", out var i) ? i.GetString() ?? "" : "";
        var label = el.TryGetProperty("label", out var l) ? l.GetString() ?? "" : "";
        var glyph = el.TryGetProperty("icon_glyph", out var g) ? g.GetString() ?? "" : "";
        var submenu = el.TryGetProperty("has_submenu", out var hs) && hs.ValueKind == JsonValueKind.True;
        var header = el.TryGetProperty("is_section_header", out var sh) && sh.ValueKind == JsonValueKind.True;
        return new ActionItem(id, label, glyph, submenu, header);
    }

    /// <summary>
    /// 发送一条请求，读取一行响应并解析为 JSON。串行化以保证请求/响应配对。
    /// 重要：一旦请求写出，必须把对应响应读完，绝不能因 CancellationToken 中途放弃读——
    /// 否则管道里会残留旧响应，下一次 Search 会读到上一次的结果（表现为高亮/列表错位）。
    /// 取消只作用于"等锁"和"业务层丢弃结果"；ViewModel 用 seq 丢弃过期 UI 更新。
    /// </summary>
    private async Task<JsonElement> SendAsync(object request, CancellationToken ct)
    {
        if (_writer is null || _reader is null)
            throw new InvalidOperationException("管道尚未连接");

        await _ioLock.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            try
            {
                var json = JsonSerializer.Serialize(request);
                // 写出后必须完成配对读，故读写使用 None，避免取消留下孤儿响应。
                await _writer.WriteLineAsync(json.AsMemory(), CancellationToken.None).ConfigureAwait(false);
                await _writer.FlushAsync(CancellationToken.None).ConfigureAwait(false);

                var line = await _reader.ReadLineAsync(CancellationToken.None).ConfigureAwait(false)
                    ?? throw new IOException("后端在返回响应前关闭了管道");

                // 配对完成后再兑现取消，让上层丢弃结果而不破坏管道。
                ct.ThrowIfCancellationRequested();

                using var doc = JsonDocument.Parse(line);
                var root = doc.RootElement;
                if (root.GetProperty("type").GetString() == "error")
                {
                    // 业务错误：管道仍可用，只把消息抛给调用方展示。
                    var msg = root.TryGetProperty("message", out var m) ? m.GetString() : "未知错误";
                    throw new InvalidOperationException("后端返回错误：" + msg);
                }
                return root.Clone();
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch (InvalidOperationException)
            {
                // 业务 error，保留连接。
                throw;
            }
            catch
            {
                // 传输层失败：释放管道，IsConnected 变 false，上层可重连。
                DisposeStreamOnly();
                throw;
            }
        }
        finally
        {
            _ioLock.Release();
        }
    }

    internal static SearchResult ParseResult(JsonElement el)
    {
        var kind = el.TryGetProperty("kind", out var k) ? k.GetString() ?? "" : "";
        var title = el.TryGetProperty("title", out var t) ? t.GetString() ?? "" : "";
        var subtitle = el.TryGetProperty("subtitle", out var s) ? s.GetString() ?? "" : "";
        var id = el.TryGetProperty("execute_id", out var e) ? e.GetString() ?? "" : "";
        var spans = Array.Empty<int>();
        if (el.TryGetProperty("match_spans", out var ms) && ms.ValueKind == JsonValueKind.Array)
        {
            var list = new List<int>();
            foreach (var n in ms.EnumerateArray())
                if (n.TryGetInt32(out var v)) list.Add(v);
            spans = list.ToArray();
        }
        SearchMatchMetadata? metadata = null;
        if (el.TryGetProperty("match_metadata", out var match)
            && match.ValueKind == JsonValueKind.Object
            && match.TryGetProperty("class", out var matchClass)
            && match.TryGetProperty("position", out var position)
            && match.TryGetProperty("score", out var score)
            && matchClass.TryGetInt32(out var parsedClass)
            && position.TryGetInt32(out var parsedPosition)
            && score.TryGetInt32(out var parsedScore))
        {
            metadata = new SearchMatchMetadata(parsedClass, parsedPosition, parsedScore);
        }
        ActionTarget? target = null;
        if (el.TryGetProperty("target", out var targetValue)
            && targetValue.ValueKind == JsonValueKind.Object
            && targetValue.TryGetProperty("kind", out var targetKind)
            && targetKind.ValueKind == JsonValueKind.String
            && targetValue.TryGetProperty("value", out var targetPayload)
            && targetPayload.ValueKind == JsonValueKind.String)
        {
            target = new ActionTarget(targetKind.GetString() ?? "", targetPayload.GetString() ?? "");
        }
        return new SearchResult(kind, title, subtitle, id, spans)
        {
            MatchMetadata = metadata,
            Target = target,
        };
    }

    /// <summary>确保后端进程在运行；未运行则定位可执行文件并启动。</summary>
    private void EnsureBackendRunning()
    {
        if (_backend is { HasExited: false })
            return;

        var exe = LocateBackend()
            ?? throw new FileNotFoundException(
                "未找到 prism-core.exe。开发期请先执行：cargo build --manifest-path src/prism-core/Cargo.toml");
        BackendPath = exe;

        var psi = new ProcessStartInfo
        {
            FileName = exe,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        _backend = Process.Start(psi)
            ?? throw new IOException("启动 prism-core.exe 失败");
    }

    /// <summary>
    /// 定位后端可执行文件：
    /// 1) 环境变量 PRISM_CORE_EXE；
    /// 2) 与 Prism.exe 同目录（发布形态）；
    /// 3) 开发期回退到 prism-core/target/&lt;profile&gt;。
    ///
    /// 开发期的 profile **跟随本程序的构建配置**（Release 构建找 release，
    /// Debug 构建找 debug），不再固定 debug 优先。历史上这里是
    /// `{ "debug", "release" }` 且只判存在性，导致 target\debug 里一个陈旧的
    /// exe 会永久遮蔽刚编出来的 release，症状是「改了代码却跑的是旧二进制」。
    /// 同 profile 找不到时才回退另一个，并且此时取 **更新的那个**，避免再次
    /// 被过期产物钉住。
    /// </summary>
    private static string? LocateBackend()
    {
        const string exeName = "prism-core.exe";

        var env = Environment.GetEnvironmentVariable("PRISM_CORE_EXE");
        if (!string.IsNullOrEmpty(env) && File.Exists(env))
            return env;

        var baseDir = AppContext.BaseDirectory;
        var sideBySide = Path.Combine(baseDir, exeName);
        if (File.Exists(sideBySide))
            return sideBySide;

#if DEBUG
        const string preferred = "debug";
        const string fallback = "release";
#else
        const string preferred = "release";
        const string fallback = "debug";
#endif

        var dir = new DirectoryInfo(baseDir);
        while (dir is not null)
        {
            // 同一层里既可能是 <dir>/prism-core/target，也可能是 <dir>/src/prism-core/target。
            foreach (var root in new[]
                     {
                         Path.Combine(dir.FullName, "prism-core", "target"),
                         Path.Combine(dir.FullName, "src", "prism-core", "target"),
                     })
            {
                var preferredPath = Path.Combine(root, preferred, exeName);
                if (File.Exists(preferredPath))
                    return preferredPath;

                var fallbackPath = Path.Combine(root, fallback, exeName);
                if (File.Exists(fallbackPath))
                    return fallbackPath;
            }
            dir = dir.Parent;
        }
        return null;
    }

    private void DisposeStreamOnly()
    {
        try { _writer?.Dispose(); } catch { /* ignore */ }
        try { _reader?.Dispose(); } catch { /* ignore */ }
        try { _stream?.Dispose(); } catch { /* ignore */ }
        _writer = null;
        _reader = null;
        _stream = null;
    }

    public void Dispose()
    {
        _watchdog?.Dispose();
        DisposeStreamOnly();

        try
        {
            if (_backend is { HasExited: false })
                _backend.Kill(entireProcessTree: true);
        }
        catch { /* ignore */ }
        _backend?.Dispose();
        _ioLock.Dispose();
    }
}

/// <summary>
/// 首建进度快照（G9）。整体可缺失；各字段也可单独缺失——首次安装没有旧缓存可估算
/// 记录总量，此时只有卷计数可用。
/// </summary>
/// <param name="Scanned">已扫描记录数，0 表示未知。</param>
/// <param name="TotalEstimate">记录总量估算，0 表示未知。</param>
/// <param name="VolumesTotal">待建卷总数。</param>
/// <param name="VolumesDone">已建完并已可搜的卷数。</param>
/// <param name="CurrentVolume">正在建索引的卷挂载点。</param>
public sealed record IndexProgress(
    ulong Scanned,
    ulong TotalEstimate,
    int? VolumesTotal,
    int? VolumesDone,
    string? CurrentVolume)
{
    /// <summary>可解释进度文案。无卷计数时回落到"请稍候"由调用方决定。</summary>
    public string? Describe()
    {
        if (VolumesTotal is not > 0 || VolumesDone is null) return null;
        var scope = CurrentVolume is { Length: > 0 }
            ? $"，正在扫描 {CurrentVolume}"
            : "";
        return $"已完成 {VolumesDone}/{VolumesTotal} 个磁盘{scope}";
    }
}

/// <summary>
/// search 响应：回显 query + 结果列表。
///
/// <paramref name="IsIndexing"/> 表示索引尚未建完，可能完全未就绪（无结果），
/// 也可能部分就绪（已有结果但仍在补充，G9 的 ready &amp;&amp; building）。它与
/// <paramref name="IsTruncated"/> 是两种不同的"不完整"：后者只表示"命中数超过 max"。
/// </summary>
/// <param name="RootRejection">
/// 后端拒绝了本次请求携带的 root：<paramref name="Items"/> 已经是全局搜索结果，
/// 前端据此回到全局范围并提示。为 null 表示本次搜索没有 root 问题。
/// </param>
public sealed record SearchResponse(
    string Query,
    IReadOnlyList<SearchResult> Items,
    bool IsIndexing,
    string? IndexError,
    bool IsTruncated = false,
    ulong? IndexGeneration = null,
    IndexProgress? IndexProgress = null,
    string? PinyinStatus = null,
    string? HistoryStatus = null,
    RootRejection? RootRejection = null,
    string? RootMessage = null);
