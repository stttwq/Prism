using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 命名管道客户端 + 后端进程守护。
/// 第四步：search / execute / reveal；断线时抛 IOException，由调用方决定是否重连。
///
/// 双通道（审计 C1）：搜索/状态类请求走 <see cref="_query"/>，交互式动作
/// （execute / reveal / run_action，可能弹属性页或复制确认对话框，合法等待任意久）
/// 走独立的 <see cref="_action"/> 连接。broker 侧 <c>serve()</c> 本就是每连接
/// 一个 task 的多客户端架构（且有 4 个 armed listener），前端多开一条连接零改动。
///
/// 语义边界：独立连接解的是"动作阻塞搜索"。"动作阻塞动作"（broker 侧唯一 STA
/// worker + 本类每通道串行）仍然存在，归审计 S2b，不在本次范围内。
/// </summary>
public sealed class PipeClient : ISearchClient, IDisposable
{
    private const string PipeName = "prism-core"; // 完整名 \\.\pipe\prism-core
    private const int ProtocolVersion = 1;

    private Process? _backend;
    private JobObjectGuard? _jobGuard;

    /// <summary>搜索/状态通道：全部带读超时，watchdog 只判它的活。</summary>
    private readonly PipeChannel _query;

    /// <summary>
    /// 动作通道：无读超时（交互式对话框），按需懒连接，不参与 watchdog 判活。
    /// 建连超时给得比搜索通道宽松——它不在每击键路径上，宁可多等也别退回搜索通道。
    /// </summary>
    private readonly PipeChannel _action;

    private System.Threading.Timer? _watchdog;
    private int _consecutiveFailures;
    private bool _wasConnected;

    /// <summary>
    /// AUDIT-2026-08-18 C-D9: Dispose 已开始。watchdog tick 与 EnsureBackendRunning
    /// 双重检查此标志，防止 Dispose 杀掉 broker 之后在途 tick 又拉起一个新孤儿。
    /// </summary>
    private volatile bool _disposed;

    /// <summary>
    /// AUDIT-2026-08-18 C-D9: 测试用进程启动器——注入后 EnsureBackendRunning 不再
    /// 真正 Process.Start（测试里绝不能拉起真实 prism-core）。返回 null 走既有
    /// "启动失败"IOException 路径。
    /// </summary>
    internal Func<ProcessStartInfo, Process?>? ProcessStarterForTest { get; set; }

    /// <summary>
    /// 后端连接状态变化通知。true=已连上，false=断开/重连失败。
    /// 由 watchdog 和首次连接触发，供上层（App）更新 UI 状态。
    /// </summary>
    public event Action<bool>? ConnectionChanged;

    /// <summary>实际使用的后端可执行文件路径，供诊断显示。</summary>
    public string BackendPath { get; private set; } = "";

    /// <summary>管道是否已连接（指搜索通道——上层的"后端可用"就是它）。</summary>
    public bool IsConnected => _query.IsConnected;

    /// <summary>AUDIT-2026-08-18 R-A5: 测试用——首连失败后 watchdog 是否已 arm。</summary>
    internal bool IsWatchdogArmed => _watchdog is not null;

    /// <summary>AUDIT-2026-08-18 R-A8: 测试用——查询通道当前服务端 PID；未连接为 null。</summary>
    internal int? QueryServerProcessId => _query.ServerProcessId;

    /// <summary>AUDIT-2026-08-18 R-A8: 测试用——是否已接管某个后端进程对象。</summary>
    internal bool HasBackendProcess => _backend is not null;

    // K0 T9：命令能力总开关。internal static 既是测试注入点也是紧急关闭手段
    //（与 ActionReadTimeout 同款先例）。设计 P7 明确 K0 不往 settings.json 加命令字段。
    // K1 陷阱：SendActionAsync 在动作通道未连接时回退到查询通道，ExecuteCommand 因此
    // 可能落在任一通道上，能力/PID/build_id/generation 的一致性检查必须针对实际承载
    // 通道做。K0 让两个通道各自回答 HasCommands，把这个检查在 K1 变成可行的。
    internal static bool AdvertiseCommandCapability { get; set; } = true;

    /// <summary>查询通道是否协商了 commands_v1 能力（命令功能整体开关）。</summary>
    public bool CommandsAvailable => _query.HasCommands;

    /// <summary>查询通道的命令目录代际（测试用；0 = 未拉取或未协商）。</summary>
    internal ulong? PipeCommandCatalogGeneration => _query.CommandCatalogGeneration;

    /// <summary>查询通道的 broker build_id（测试用；null = 未发送）。</summary>
    internal string? PipeBuildId => _query.BuildId;

    public PipeClient() : this(PipeName)
    {
    }

    /// <summary>测试用：注入临时管道名，避免碰真实 broker 管道。</summary>
    internal PipeClient(string pipeName)
    {
        _query = new PipeChannel(pipeName, "搜索通道", TimeSpan.FromMilliseconds(500));
        _action = new PipeChannel(pipeName, "动作通道", TimeSpan.FromSeconds(2));
    }

    /// <summary>
    /// 先尝试连接已存在的 broker（可能是上次 Prism 留下的存活孤儿，复用它而非拉新进程），
    /// 连不上才拉起新的 prism-core.exe。所有连接操作都在通道自己的 IO 锁内串行化，
    /// 防止 watchdog、搜索重连、首次启动三者互相 dispose 对方刚建好的 stream。
    ///
    /// 动作通道不在这里预连：它不在启动关键路径上，首次用到时懒连接即可，
    /// 也避免启动期多占一个 listener 槽位。
    ///
    /// AUDIT-2026-08-18 R-A5: watchdog 在连接之前启动——首连失败（broker 起不来/连不上）
    /// 时不再让进程陷入"只能重启 Prism"的死局，watchdog tick 的 EnsureBackendRunning
    /// 会在后台持续重试拉起 broker。
    /// </summary>
    public async Task StartAsync(CancellationToken ct = default)
    {
        StartWatchdog();
        try
        {
            await ConnectOrReconnectAsync(ct).ConfigureAwait(false);
        }
        catch
        {
            // F3（全仓检验 2026-08-25 第二轮）：连接失败以异常上抛，控制流到不了
            // 上一版补丁的 else 分支——_wasConnected 停留预置的 true，watchdog
            // 后台恢复连接时 NotifyConnection(true) 被 true==true 吞掉（托盘
            // 永久"后端未连接"、别名词集不重拉）。异常路径先把状态对齐再抛。
            _wasConnected = false;
            NotifyConnection(false);
            throw;
        }
        // 成功走 NotifyConnection（已连接时内部吞掉、无冗余事件；曾报过失败则
        // 发出恢复通知）。
        NotifyConnection(true);
        if (!_query.IsConnected)
            throw new IOException("无法连接到后端");
    }

    /// <summary>
    /// 搜索通道的统一连接入口：先试已有管道，连不上拉进程再连。
    /// <paramref name="forceReconnect"/> 供 watchdog 越过"已连上"快速返回：
    /// 连通但不应答的半死连接（IsConnected 仍为 true）必须强制重建。
    /// </summary>
    private Task ConnectOrReconnectAsync(CancellationToken ct, bool forceReconnect = false) =>
        _query.ConnectOrReconnectAsync(ct, forceReconnect, EnsureBackendRunning, AdoptExistingServer);

    /// <summary>broker 进程名（与 <see cref="LocateBackend"/> 搜索的 exe 名一致）。</summary>
    private const string BrokerProcessName = "prism-core";

    /// <summary>
    /// X2：管道服务端进程的可执行文件目录是否与本进程期望的安装目录不一致。
    /// 任一路径探测失败返回 false（保守视作一致，复用现状）。
    /// </summary>
    private static bool ServerDirectoryDiffers(Process proc)
    {
        try
        {
            var expected = LocateBackend();
            if (string.IsNullOrEmpty(expected))
                return false;
            var actual = proc.MainModule?.FileName;
            if (string.IsNullOrEmpty(actual))
                return false;
            return !string.Equals(
                System.IO.Path.GetFullPath(expected),
                System.IO.Path.GetFullPath(actual),
                StringComparison.OrdinalIgnoreCase);
        }
        catch
        {
            return false;
        }
    }

    /// <summary>
    /// AUDIT-2026-08-18 R-A8: 连上已有管道后收编服务端进程。此前孤儿 broker 被复用时
    /// 不在新 Prism 的 Job Object 里——新 Prism 崩溃不带走它，孤儿逐代积累占管道。
    /// 规则：自己的 broker 直接认；确认是 prism-core 的孤儿尝试收编，收编失败杀旧拉新
    /// （返回 false 让通道丢弃连接走拉新路径）；名字不符（开发/测试的 mock server）或
    /// 探测异常时保守复用（与修复前行为一致），绝不动未知进程。internal 供测试锚定。
    /// </summary>
    internal bool AdoptExistingServer(int pid)
    {
        Process? proc = null;
        try
        {
            if (_backend is { HasExited: false } own && own.Id == pid)
                return true; // 本进程拉起的 broker，EnsureBackendRunning 已收编

            proc = Process.GetProcessById(pid);
            if (!proc.ProcessName.Equals(BrokerProcessName, StringComparison.OrdinalIgnoreCase))
            {
                // AUDIT-4 B10（2026-08-21）：复用返回前释放 Process 句柄——
                // GetProcessById 每次都新开句柄，不 Dispose 会延迟回收。
                proc.Dispose();
                return true; // 不是 broker 的管道服务端（mock/测试）——照常复用
            }

            // L 批次（FRESH-AUDIT-3-2026-08-20）：RDP 双会话时另一会话的 broker
            // 不是孤儿——跨会话收编必失败，但绝不能 Kill 它（那会打掉别人会话的
            // 搜索）。保守复用，跨会话连不上时由 watchdog 走拉新路径。
            // 跨会话检查不杀（同前）；本进程会话号只读一次缓存。
            using var current = Process.GetCurrentProcess();
            if (proc.SessionId != current.SessionId)
            {
                proc.Dispose(); // AUDIT-4 B10：同上，复用返回前释放句柄。
                return true;
            }

            // X2（全仓检验 2026-08-25 第二轮）：换目录覆盖安装——旧目录的 broker 还
            // 活着并占着管道名时，直接收编会让新前端继续跑旧二进制 + 旧目录数据
            //（settings/history/aliases 与新目录分裂两套，直到重启）。服务端 exe
            // 目录与本进程期望的安装目录不一致时杀旧拉新；在途对话框守卫与 M1 同款
            //（动作通道有在途交互请求时先留着，下轮重连再收）。路径探测失败
            //（权限/已退出/平台不符）保守复用，维持修复前行为。
            if (ServerDirectoryDiffers(proc))
            {
                if (_action.HasPendingSlowRead)
                {
                    proc.Dispose();
                    return true;
                }
                try { proc.Kill(); } catch { /* 已退出 */ }
                proc.Dispose();
                return false;
            }

            _jobGuard ??= new JobObjectGuard();
            if (_jobGuard.TryAdopt(pid))
            {
                // 复审 M1（2026-08-21）：收编成功后，若此前还认着一只**自己的**
                /// 活 broker（连接打到了孤儿实例的窗口期），旧实例已无用途——
                // 不杀会变成第二只无主孤儿占着 listener（KILL_ON_JOB_CLOSE 已移除，
                // 没有任何机制会带走它）。只杀本体不杀树（Bug 3：用户应用是子进程）。
                if (_backend is { HasExited: false } previous && previous.Id != pid)
                {
                    // 审计 2026-08-25（中）：动作通道有在途交互请求（属性页/复制
                    // 确认/删除确认对话框挂在旧 broker 的 STA worker 上）时不能杀
                    // ——用户眼前的系统对话框会随之消失、在途动作误报失败。留它
                    // 活着：已在 Job Object 里随本进程退出统一回收，对话完成后
                    // 动作通道的响应照常送达；无在途请求时照旧立即杀。
                    if (!_action.HasPendingSlowRead)
                    {
                        try { previous.Kill(); } catch { /* 已退出 */ }
                    }
                }
                try { _backend?.Dispose(); } catch { /* 已退出 */ }
                _backend = proc; // 收编成功：接管生命周期（Dispose 时随 Job 一起回收），不得提前 Dispose
                return true;
            }

            // 收编失败（权限/已死/已在别的 Job 且被拒）：孤儿占着管道名，新 Prism 死后
            // 它仍会残留——杀掉它让通道走拉新路径（新进程必进本 Job）。
            // 复审 M1（2026-08-21）：只杀本体不杀树——孤儿 broker 的子进程是
            // 经它打开的用户应用（Bug 3 同款语义），杀树会带走用户的程序。
            try
            {
                proc.Kill();
            }
            catch { /* 已退出/无权限：管道侧自然拉新 */ }
            finally
            {
                proc.Dispose(); // AUDIT-4 B10：Kill 后同样释放句柄。
            }
            return false;
        }
        catch
        {
            // GetProcessById/OpenProcess/Kill 任一失败：保守复用，搜索可用性优先。
            proc?.Dispose();
            return true;
        }
    }

    /// <summary>
    /// 启动后台 watchdog：首次 3 秒后检查，之后每 15 秒一次。
    /// 判活从纯 IsConnected 升级为"连通 + 最近查询请求有响应"（审计 C2）：
    /// 管道半死（连着但不应答）也要触发重连。
    /// 管道断开连续 2 次才触发重连，避免单次抖动误判；半死判定是纯时间运算
    /// （已含 QueryReadTimeout + 一个周期宽限），单次即触发。
    /// </summary>
    private void StartWatchdog()
    {
        _wasConnected = true;
        _watchdog?.Dispose();
        _watchdog = new System.Threading.Timer(
            callback: _ => _ = WatchdogTickAsync(),
            state: null,
            dueTime: TimeSpan.FromSeconds(3),
            period: TimeSpan.FromSeconds(15));
    }

    private async Task WatchdogTickAsync()
    {
        // C-D9: Dispose 已开始就不再干活——在途 tick 排干由 Dispose(waitHandle) 保证，
        // 这里是快速出口（排干窗口内新 tick 不会再有）。
        if (_disposed)
            return;

        var forceReconnect = false;
        if (_query.IsConnected)
        {
            var wedged = IsWedged(
                isConnected: true,
                hasPendingSlowRead: _query.HasPendingSlowRead,
                lastQuerySentTicks: _query.LastQuerySentTicks,
                lastResponseTicks: _query.LastResponseTicks,
                now: DateTime.UtcNow,
                queryTimeout: QueryReadTimeout,
                grace: TimeSpan.FromSeconds(15));
            if (!wedged)
            {
                _consecutiveFailures = 0;
                NotifyConnection(true);
                return;
            }
            // 连通但最近一次查询请求悬而未决且远超读超时+宽限——视同半死，
            // 强制重建（ConnectOrReconnectAsync 的快速返回只认 IsConnected，需 force 越过）。
            forceReconnect = true;
        }
        else
        {
            _consecutiveFailures++;
            if (_consecutiveFailures < 2)
                return;

            _consecutiveFailures = 0;
        }

        // 管道断了/半死——走统一重连路径（持锁 + 重入保护）。
        // 重连令牌带上限：_ioLock 可能被一个带超时的慢请求占着（最多约 8 秒），
        // tick 决不能在锁上无限堆叠。
        using var reconnectCts = new CancellationTokenSource(TimeSpan.FromSeconds(10));
        try
        {
            await ConnectOrReconnectAsync(reconnectCts.Token, forceReconnect).ConfigureAwait(false);
            NotifyConnection(true);
        }
        catch
        {
            // 审计 2026-08-25（中）：这里的失败可能只是 10s CTS 到期——_ioLock 被
            // 用户路径的慢重连（最长约 13s）占着，连接本身可能已被那条路径修好。
            // 不核实就 NotifyConnection(false) 会让 UI 停在"正在重连"，随后
            // StartAsync 成功路径的赋值又吞掉恢复通知（F1 同症状的新复发路径）。
            // 按实际连接状态汇报；真断连仍是 false。
            NotifyConnection(_query.IsConnected);
        }
    }

    /// <summary>
    /// 半死判定（审计 C2）：管道连通，但存在一次"带读超时"的查询请求发出后
    /// 超过 读超时+宽限 仍无任何新响应，且当前没有可合法等待任意久的动作请求在途。
    /// 正常路径下查询读超时自身会销毁流（IsConnected 变 false 走普通断开分支），
    /// 这里兜的是超时未生效/响应彻底停止的残余场景。
    /// </summary>
    internal static bool IsWedged(
        bool isConnected,
        bool hasPendingSlowRead,
        long lastQuerySentTicks,
        long lastResponseTicks,
        DateTime now,
        TimeSpan queryTimeout,
        TimeSpan grace)
    {
        if (!isConnected || hasPendingSlowRead || lastQuerySentTicks <= 0)
            return false;
        if (lastQuerySentTicks <= lastResponseTicks)
            return false; // 该请求之后已有响应到达
        var sentAt = new DateTime(lastQuerySentTicks, DateTimeKind.Utc);
        return now - sentAt > queryTimeout + grace;
    }

    private void NotifyConnection(bool connected)
    {
        if (connected == _wasConnected) return;
        _wasConnected = connected;
        ConnectionChanged?.Invoke(connected);
    }

    /// <summary>
    /// 连接搜索通道（UTF-8 无 BOM，按行 \n 收发）并完成握手。
    /// internal 供测试直接验证握手超时/清理路径（走 StartAsync 会拉起真实 broker）。
    /// </summary>
    internal Task ConnectInnerAsync(TimeSpan timeout, CancellationToken ct) =>
        _query.ConnectLockedAsync(timeout, ct);

    /// <summary>
    /// 单行最大字符数（AUDIT-2026-08-18 R-A4）。broker 入站已有 1MB 有界读、
    /// indexer 响应 8MB；此前唯独 broker→前端方向用无上限的 ReadLineAsync，
    /// 任意本地进程若能写管道即可灌超长行撑爆前端内存。超限按协议损坏处理
    /// （IOException → 销毁连接走重连），与传输层失败同路径。
    /// </summary>
    internal const int MaxResponseLineChars = 16 * 1024 * 1024;

    /// <summary>
    /// 有界逐行读：语义对齐 StreamReader.ReadLineAsync（按 \n 分行、吃 \r\n、
    /// EOF 返回 null），超过 <see cref="MaxResponseLineChars"/> 抛 IOException。
    /// StreamReader.ReadAsync 一旦返回就把整块字符消费掉了——换行之后同块的
    /// 尾巴必须留在本实例里供下一次 ReadLine 使用，所以有界读必须是有状态的
    /// 每连接一个对象（挂在 PipeChannel 上，随流一起销毁重建）。
    /// </summary>
    internal sealed class BoundedLineReader
    {
        private readonly StreamReader _reader;
        private readonly char[] _pending = new char[4096];
        private int _pendingStart, _pendingCount;

        // G3（FRESH-AUDIT-2）：读块缓冲与行构造器提为字段——每击键一行响应，
        // 此前每次 ReadLineAsync 都新分配 4KB char[] + StringBuilder。
        // 实例挂在 PipeChannel 上、随流销毁重建，调用由 _ioLock 串行化，无重入。
        private readonly char[] _read = new char[4096];
        private readonly System.Text.StringBuilder _line = new();

        public BoundedLineReader(StreamReader reader) => _reader = reader;

        public async Task<string?> ReadLineAsync(CancellationToken ct)
        {
            _line.Clear();
            var line = _line;
            var buffer = _read;
            while (true)
            {
                // 先用掉上次读完换行后剩下的尾巴。
                if (_pendingCount > 0)
                {
                    var split = Array.IndexOf(_pending, '\n', _pendingStart, _pendingCount);
                    if (split >= 0)
                    {
                        line.Append(_pending, _pendingStart, split - _pendingStart);
                        _pendingCount -= split - _pendingStart + 1;
                        _pendingStart = split + 1;
                        return TrimCarriageReturn(line);
                    }
                    line.Append(_pending, _pendingStart, _pendingCount);
                    _pendingStart = _pendingCount = 0;
                }
                CheckCap(line);
                int read = await _reader.ReadAsync(buffer.AsMemory(), ct).ConfigureAwait(false);
                if (read == 0)
                    return line.Length == 0 ? null : TrimCarriageReturn(line);
                var newline = Array.IndexOf(buffer, '\n', 0, read);
                if (newline >= 0)
                {
                    line.Append(buffer, 0, newline);
                    // 换行后的同块尾巴留给下一次调用。
                    var tail = read - newline - 1;
                    if (tail > 0)
                    {
                        Array.Copy(buffer, newline + 1, _pending, 0, tail);
                        _pendingStart = 0;
                        _pendingCount = tail;
                    }
                    return TrimCarriageReturn(line);
                }
                line.Append(buffer, 0, read);
                CheckCap(line);
            }
        }

        private static string TrimCarriageReturn(System.Text.StringBuilder line)
        {
            var result = line.ToString();
            return result.EndsWith('\r') ? result[..^1] : result;
        }

        private static void CheckCap(System.Text.StringBuilder line)
        {
            if (line.Length > MaxResponseLineChars)
                throw new IOException($"响应行超过 {MaxResponseLineChars} 字符上限——协议损坏，销毁连接");
        }
    }

    /// <summary>
    /// 读握手响应行，带超时（纯逻辑测试用，不涉及真实管道）。超时/EOF 都转成
    /// IOException 上抛。生产路径走 <see cref="PipeChannel.HandshakeAsync"/>（named pipe
    /// 需要 Dispose 促使挂死的读返回，不能单靠 CancellationToken）。
    /// </summary>
    internal static async Task<string> ReadHandshakeLineAsync(StreamReader reader, TimeSpan timeout)
    {
        using var deadline = new CancellationTokenSource(timeout);
        string? line;
        try
        {
            line = await new BoundedLineReader(reader).ReadLineAsync(deadline.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (deadline.IsCancellationRequested)
        {
            throw new IOException($"后端握手超时（{Math.Round(timeout.TotalSeconds)} 秒未收到 hello）");
        }
        return line ?? throw new IOException("后端在握手时关闭了管道");
    }

    /// <summary>查询类请求的读超时：覆盖搜索（毫秒级）+ 索引写锁最坏停顿（数秒），到点销毁流并触发重连自愈。</summary>
    private static readonly TimeSpan QueryReadTimeout = TimeSpan.FromSeconds(8);

    /// <summary>发送 ping，返回后端版本号。</summary>
    public async Task<string> PingAsync(CancellationToken ct = default)
    {
        var resp = await SendAsync(new { type = "ping" }, ct, QueryReadTimeout).ConfigureAwait(false);
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
        var resp = await SendAsync(
            SearchPayload(query, max, context, CommandsAvailable), ct, QueryReadTimeout).ConfigureAwait(false);
        return ParseSearchResponse(resp, query);
    }

    /// <summary>
    /// 组装 search 请求。`root` 只在真正限定当前目录时出现：范围为全局时字段整体缺失，
    /// 与加入 root 之前的线上格式逐字节一致，也保证「UI 说全局」与「后端搜全局」不会背离。
    /// </summary>
    internal static Dictionary<string, object?> SearchPayload(
        string query, int max, SearchContext context, bool commandsAvailable = false)
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
        // K0 T10.5：仅在通道已协商 commands_v1 且 CommandContext 非 null 时加 command_context。
        // 其余情况 payload 逐字节等于旧格式（P2 字节级锚点）。
        if (commandsAvailable && context.CommandContext is { } cmdCtx)
        {
            var cmd = new Dictionary<string, object?>();
            if (!string.IsNullOrWhiteSpace(cmdCtx.CurrentFolder))
                cmd["current_folder"] = cmdCtx.CurrentFolder;
            if (!string.IsNullOrWhiteSpace(cmdCtx.HostKind))
                cmd["host_kind"] = cmdCtx.HostKind;
            if (cmdCtx.HostCapabilities.Count > 0)
                cmd["host_capabilities"] = cmdCtx.HostCapabilities.ToArray();
            // 只有有内容时才加 command_context（空对象不如不加——逐字节对齐旧格式）。
            if (cmd.Count > 0)
                payload["command_context"] = cmd;
        }
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
        // K0 T10.8：cacheable 字段缺失 → true（与 broker skip_serializing_if=is_true 配对）。
        var cacheable = resp.TryGetProperty("cacheable", out var cacheableValue)
            ? cacheableValue.ValueKind != JsonValueKind.False
            : true;
        ulong? commandCatalogGeneration =
            resp.TryGetProperty("command_catalog_generation", out var cmdGenValue)
            && cmdGenValue.TryGetUInt64(out var parsedCmdGen)
                ? parsedCmdGen
                : null;
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
            ReadOptionalString(resp, "root_message"),
            cacheable,
            commandCatalogGeneration);
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

    /// <summary>打开文件/文件夹/程序。query 随动作上报（查询记忆），null 线路上等价于缺失。</summary>
    public async Task ExecuteAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
    {
        await SendActionAsync(new { type = "execute", target = TargetPayload(target), query }, ct).ConfigureAwait(false);
    }

    /// <summary>在资源管理器中定位文件。query 含义同 <see cref="ExecuteAsync"/>。</summary>
    public async Task RevealAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
    {
        await SendActionAsync(new { type = "reveal", target = TargetPayload(target), query }, ct).ConfigureAwait(false);
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
        await SendAsync(new { type = "reload_engines", engines = payload }, ct, QueryReadTimeout).ConfigureAwait(false);
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
        }, ct, QueryReadTimeout).ConfigureAwait(false);
    }

    public async Task ClearHistoryAsync(CancellationToken ct = default)
    {
        await SendAsync(new { type = "clear_history" }, ct, QueryReadTimeout).ConfigureAwait(false);
    }

    /// <summary>请求某文件/文件夹的动作列表（→ 键动作面板）。</summary>
    public async Task<IReadOnlyList<ActionItem>> GetActionsAsync(ActionTarget target, CancellationToken ct = default)
    {
        var resp = await SendAsync(new { type = "actions", target = TargetPayload(target) }, ct, QueryReadTimeout).ConfigureAwait(false);
        var items = new List<ActionItem>();
        if (resp.TryGetProperty("items", out var arr) && arr.ValueKind == JsonValueKind.Array)
        {
            foreach (var el in arr.EnumerateArray())
                items.Add(ParseAction(el));
        }
        return items;
    }

    /// <summary>执行动作面板中的某一项。</summary>
    public async Task RunActionAsync(ActionTarget target, string action, string? query = null, CancellationToken ct = default)
    {
        await RunActionAsync(target, action, ActionArgs.Empty, query, ct).ConfigureAwait(false);
    }

    /// <summary>执行动作面板中的某一项，携带动作参数（destination/new_name）与查询文本。</summary>
    public async Task RunActionAsync(
        ActionTarget target,
        string action,
        ActionArgs args,
        string? query = null,
        CancellationToken ct = default)
    {
        object payload;
        if (args is { Destination: null, NewName: null })
        {
            payload = new { type = "run_action", target = TargetPayload(target), action, query };
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
            payload = new { type = "run_action", target = TargetPayload(target), action, args = argsObj, query };
        }
        await SendActionAsync(payload, ct).ConfigureAwait(false);
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
            ct,
            QueryReadTimeout).ConfigureAwait(false);
        return ParseWindowHandle(resp);
    }

    /// <summary>G5：激活成功后回报，由 broker 写窗口历史。query 为窗口模式的当前查询文本。</summary>
    public async Task RecordWindowSwitchAsync(ActionTarget target, string? query = null, CancellationToken ct = default)
    {
        await SendAsync(
            new { type = "record_window_switch", target = TargetPayload(target), query },
            ct,
            QueryReadTimeout).ConfigureAwait(false);
    }

    /// <summary>别名系统：整体替换目标的词表（空词表 = 解绑）。</summary>
    public async Task AliasSetAsync(
        ActionTarget target,
        IReadOnlyList<string> words,
        CancellationToken ct = default)
    {
        var resp = await SendAsync(
            new { type = "alias_set", target = TargetPayload(target), words },
            ct,
            QueryReadTimeout).ConfigureAwait(false);
        ThrowIfAliasRejected(resp);
    }

    /// <summary>别名系统：解绑目标（幂等）。</summary>
    public async Task AliasDeleteAsync(ActionTarget target, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            new { type = "alias_delete", target = TargetPayload(target) },
            ct,
            QueryReadTimeout).ConfigureAwait(false);
        ThrowIfAliasRejected(resp);
    }

    /// <summary>别名系统：设置页列表（绑定时间倒序）。</summary>
    public async Task<IReadOnlyList<AliasEntry>> AliasListAsync(CancellationToken ct = default)
    {
        var resp = await SendAsync(new { type = "alias_list" }, ct, QueryReadTimeout).ConfigureAwait(false);
        return ParseAliasList(resp);
    }

    /// <summary>
    /// 解析 .lnk 的目标路径（别名身份指向真实 exe）。失败/超时返回 null（调用方回退绑 .lnk）。
    /// 走 query 通道 + 8s 超时（与 AliasSetAsync 同档）；resolved 非 .lnk 或解析失败为 null。
    /// </summary>
    public async Task<string?> ResolveLnkAsync(ActionTarget target, CancellationToken ct = default)
    {
        try
        {
            var resp = await SendAsync(
                new { type = "resolve_lnk", target = TargetPayload(target) },
                ct,
                QueryReadTimeout).ConfigureAwait(false);
            if (resp.TryGetProperty("resolved", out var resolved)
                && resolved.ValueKind == JsonValueKind.String)
            {
                return resolved.GetString();
            }
            return null;
        }
        catch
        {
            // 失败/超时/断连：回退绑 .lnk（现状行为兜底）。
            return null;
        }
    }

    /// <summary>alias_applied 回执：非空 message 是业务失败（连接仍可用）。</summary>
    private static void ThrowIfAliasRejected(JsonElement resp)
    {
        if (resp.TryGetProperty("message", out var message)
            && message.ValueKind == JsonValueKind.String
            && !string.IsNullOrEmpty(message.GetString()))
        {
            throw new InvalidOperationException("后端返回错误：" + message.GetString());
        }
    }

    /// <summary>
    /// K0 T9：拉取命令目录（查询通道，已协商连接）。收到 error 时不抛致命异常，
    /// 返回 null 表示「命令功能整体不可用」——调用方据此标记降级。
    /// </summary>
    internal async Task<(ulong Generation, IReadOnlyList<CommandDescriptor> Items)?>
        CommandListAsync(CancellationToken ct = default)
    {
        try
        {
            var resp = await SendAsync(
                new { type = "command_list" }, ct, QueryReadTimeout).ConfigureAwait(false);
            if (resp.TryGetProperty("type", out var type)
                && type.GetString() == "error")
            {
                return null;
            }
            var generation = resp.TryGetProperty("generation", out var gen)
                && gen.TryGetUInt64(out var parsedGen)
                    ? parsedGen
                    : 0u;
            var items = new List<CommandDescriptor>();
            if (resp.TryGetProperty("items", out var arr) && arr.ValueKind == JsonValueKind.Array)
            {
                foreach (var el in arr.EnumerateArray())
                {
                    var descriptor = CommandDescriptor.Parse(el);
                    if (descriptor is not null)
                        items.Add(descriptor);
                }
            }
            return (generation, items);
        }
        catch
        {
            // 传输层失败/超时/断连：命令功能整体不可用。
            return null;
        }
    }

    /// <summary>
    /// K1：执行命令。发 execute_command 请求，broker 按 owner 分派：
    /// broker-owned → broker 直接执行（返回 status）；
    /// ui-owned → broker 回 ui_command，本方法据此回传 command id 供前端执行。
    /// 返回值：ui_command 时为 command id；否则 null（broker 已自行执行）。
    /// </summary>
    internal async Task<string?> ExecuteCommandAsync(
        CommandInvocationContext context,
        CancellationToken ct = default)
    {
        var payload = new Dictionary<string, object?>
        {
            ["type"] = "execute_command",
            ["context"] = new Dictionary<string, object?>
            {
                ["command_id"] = context.CommandId,
                ["source"] = context.Source,
                ["arguments"] = context.Arguments is { } args
                    ? new Dictionary<string, object?>
                    {
                        ["text"] = args.Text,
                        ["destination"] = args.Destination,
                        ["output_path"] = args.OutputPath,
                    }
                    : new Dictionary<string, object?>(),
                ["selection"] = context.Selection is { } sel
                    ? new Dictionary<string, object?>
                    {
                        ["target"] = sel.Target is { } t
                            ? new Dictionary<string, object?> { ["kind"] = t.Kind, ["value"] = t.Value }
                            : null,
                        ["title"] = sel.Title,
                        ["subtitle"] = sel.Subtitle,
                    }
                    : null,
                ["staged_paths"] = context.StagedPaths.ToArray(),
                ["current_folder"] = context.CurrentFolder,
                ["host_kind"] = context.HostKind,
                ["host_capabilities"] = context.HostCapabilities.ToArray(),
                // K4a：剪贴板文本仅在命令调用点读取（{clipboard} 取值源）。
                ["clipboard"] = ReadClipboardForCommand(),
            },
        };

        var resp = await SendAsync(payload, ct, QueryReadTimeout).ConfigureAwait(false);
        if (resp.TryGetProperty("type", out var type))
        {
            var typeStr = type.GetString();
            if (typeStr == "error")
            {
                var msg = resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
                throw new InvalidOperationException(msg);
            }
            if (typeStr == "ui_command"
                && resp.TryGetProperty("id", out var idEl))
            {
                return idEl.GetString();
            }
        }
        return null;
    }

    /// <summary>
    /// K4a：{clipboard} 取值源。调用点即读（UI 进程，broker 不读剪贴板）；
    /// 剪贴板被其他进程占用/非文本时返回 null（broker 侧展开为空串）。
    /// 8 KiB 字符上限与 broker validate 的 TEXT_MAX_BYTES 对齐，避免超限失败。
    /// </summary>
    private static string? ReadClipboardForCommand()
    {
        try
        {
            var text = System.Windows.Clipboard.GetText();
            if (string.IsNullOrEmpty(text))
            {
                return null;
            }
            return text.Length <= 8 * 1024 ? text : text[..(8 * 1024)];
        }
        catch (Exception)
        {
            // COMException（剪贴板锁）/OutOfMemory 等一律降级为无剪贴板。
            return null;
        }
    }

    /// <summary>
    /// K2 §4.6：设置或清除命令的快捷键绑定。combo=null=清除。
    /// 返回错误文案（空串=成功）。
    /// </summary>
    internal async Task<string> SetCommandShortcutAsync(
        string commandId, string? combo, CancellationToken ct = default)
    {
        var payload = new Dictionary<string, object?>
        {
            ["type"] = "set_command_shortcut",
            ["command_id"] = commandId,
            ["combo"] = combo,
        };
        var resp = await SendAsync(payload, ct, QueryReadTimeout).ConfigureAwait(false);
        if (resp.TryGetProperty("type", out var type))
        {
            var typeStr = type.GetString();
            if (typeStr == "error")
            {
                return resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
            }
            if (typeStr == "command_shortcut_applied")
            {
                return resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
            }
        }
        return "unexpected response";
    }

    /// <summary>
    /// K3 §4.5：保存用户命令。返回空串=成功，否则为错误文案。
    /// </summary>
    internal async Task<string> CommandSetAsync(
        UserCommandDefinition command, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            new { type = "command_set", command },
            ct, QueryReadTimeout).ConfigureAwait(false);
        if (resp.TryGetProperty("type", out var type))
        {
            var typeStr = type.GetString();
            if (typeStr == "error")
                return resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
            if (typeStr == "command_applied")
                return resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
        }
        return "unexpected response";
    }

    /// <summary>
    /// K3 §4.5：删除用户命令。幂等。返回空串=成功。
    /// </summary>
    internal async Task<string> CommandDeleteAsync(
        string commandId, CancellationToken ct = default)
    {
        var resp = await SendAsync(
            new { type = "command_delete", command_id = commandId },
            ct, QueryReadTimeout).ConfigureAwait(false);
        if (resp.TryGetProperty("type", out var type))
        {
            var typeStr = type.GetString();
            if (typeStr == "error")
                return resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
            if (typeStr == "command_applied")
                return resp.TryGetProperty("message", out var msgEl) ? msgEl.GetString() ?? "" : "";
        }
        return "unexpected response";
    }

    /// <summary>
    /// K3 §4.5：触发词命名空间校验。broker 查全集（引擎/别名/命令/保留字）。
    /// 返回 null=功能不可用（连接未协商或传输失败）。
    /// </summary>
    internal async Task<NamespaceValidationResult?> ValidateTriggerNamespaceAsync(
        string trigger, string owner, string? excludeCommandId = null,
        CancellationToken ct = default)
    {
        try
        {
            var payload = new Dictionary<string, object?>
            {
                ["type"] = "validate_trigger_namespace",
                ["trigger"] = trigger,
                ["owner"] = owner,
                ["exclude_command_id"] = excludeCommandId,
            };
            var resp = await SendAsync(payload, ct, QueryReadTimeout).ConfigureAwait(false);
            if (resp.TryGetProperty("type", out var type))
            {
                if (type.GetString() == "error")
                    return new NamespaceValidationResult(false,
                        new NamespaceConflictDto("error",
                            resp.TryGetProperty("message", out var m) ? m.GetString() ?? "" : ""));
                if (type.GetString() == "namespace_validation")
                    return NamespaceValidationResult.Parse(resp);
            }
            return null;
        }
        catch { return null; }
    }

    /// <summary>
    /// K3 §4.6：命令预览（dry-run）。返回执行时将用的最终字符串，不产生副作用。
    /// 返回 null=功能不可用。
    /// </summary>
    internal async Task<CommandPreviewResult?> CommandPreviewAsync(
        string commandId, string source, string? argumentsText,
        CancellationToken ct = default)
    {
        try
        {
            var payload = new Dictionary<string, object?>
            {
                ["type"] = "command_preview",
                ["context"] = new Dictionary<string, object?>
                {
                    ["command_id"] = commandId,
                    ["source"] = source,
                    ["arguments"] = argumentsText is not null
                        ? new Dictionary<string, object?> { ["text"] = argumentsText }
                        : new Dictionary<string, object?>(),
                    ["selection"] = null,
                    ["staged_paths"] = Array.Empty<string>(),
                    ["current_folder"] = null,
                    ["host_kind"] = "explorer",
                    ["host_capabilities"] = Array.Empty<string>(),
                    // K4a：预览与执行同函数（§4.6），预览也带真实剪贴板。
                    ["clipboard"] = ReadClipboardForCommand(),
                },
            };
            var resp = await SendAsync(payload, ct, QueryReadTimeout).ConfigureAwait(false);
            if (resp.TryGetProperty("type", out var type))
            {
                if (type.GetString() == "error")
                    return new CommandPreviewResult(false,
                        resp.TryGetProperty("message", out var m) ? m.GetString() ?? "" : "",
                        null, null, [], null);
                if (type.GetString() == "command_preview_result")
                    return CommandPreviewResult.Parse(resp);
            }
            return null;
        }
        catch { return null; }
    }

    /// <summary>
    /// K3 §4.8：导出用户命令。broker 返回持久化形态（UserCommandDefinition 列表）
    /// + exported_at。内置命令不导出。返回 null=功能不可用。
    /// </summary>
    internal async Task<(IReadOnlyList<UserCommandDefinition> Commands, string ExportedAt)?>
        CommandExportAsync(CancellationToken ct = default)
    {
        try
        {
            var resp = await SendAsync(
                new { type = "command_export" },
                ct, QueryReadTimeout).ConfigureAwait(false);
            if (resp.TryGetProperty("type", out var type))
            {
                if (type.GetString() == "error")
                    return null;
                if (type.GetString() == "command_export_result")
                {
                    var exportedAt = resp.TryGetProperty("exported_at", out var ea)
                        && ea.ValueKind == JsonValueKind.String
                        ? ea.GetString() ?? "" : "";
                    var commands = new List<UserCommandDefinition>();
                    if (resp.TryGetProperty("envelope", out var env)
                        && env.TryGetProperty("data", out var data)
                        && data.TryGetProperty("commands", out var arr)
                        && arr.ValueKind == JsonValueKind.Array)
                    {
                        foreach (var el in arr.EnumerateArray())
                        {
                            if (el.ValueKind == JsonValueKind.Object)
                            {
                                var cmd = ParseUserCommandDefinition(el);
                                if (cmd is not null) commands.Add(cmd);
                            }
                        }
                    }
                    return (commands, exportedAt);
                }
            }
            return null;
        }
        catch { return null; }
    }

    private static UserCommandDefinition? ParseUserCommandDefinition(JsonElement el)
    {
        var cmd = new UserCommandDefinition
        {
            Id = el.TryGetProperty("id", out var id) ? id.GetString() ?? "" : "",
            Title = el.TryGetProperty("title", out var t) ? t.GetString() ?? "" : "",
            Subtitle = el.TryGetProperty("subtitle", out var s) ? s.GetString() ?? "" : "",
            IconGlyph = el.TryGetProperty("icon_glyph", out var ig) ? ig.GetString() ?? "" : "",
            Danger = el.TryGetProperty("danger", out var d) ? d.GetString() ?? "normal" : "normal",
            Enabled = el.TryGetProperty("enabled", out var en) && en.ValueKind == JsonValueKind.True,
            Handler = el.TryGetProperty("handler", out var h) ? h.GetString() ?? "open_url" : "open_url",
        };

        // keywords
        if (el.TryGetProperty("keywords", out var kw) && kw.ValueKind == JsonValueKind.Array)
            foreach (var k in kw.EnumerateArray())
                if (k.ValueKind == JsonValueKind.String) cmd.Keywords.Add(k.GetString() ?? "");

        // handler_params
        if (el.TryGetProperty("handler_params", out var hp) && hp.ValueKind == JsonValueKind.Object)
            foreach (var p in hp.EnumerateObject())
                if (p.Value.ValueKind == JsonValueKind.String)
                    cmd.HandlerParams[p.Name] = p.Value.GetString() ?? "";

        // input
        if (el.TryGetProperty("input", out var inp) && inp.ValueKind == JsonValueKind.Object)
        {
            cmd.Input.Kind = inp.TryGetProperty("kind", out var ik) ? ik.GetString() ?? "none" : "none";
            cmd.Input.Required = inp.TryGetProperty("required", out var ir) && ir.ValueKind == JsonValueKind.True;
            cmd.Input.Prompt = inp.TryGetProperty("prompt", out var ip) ? ip.GetString() ?? "" : "";
        }

        // bindings
        if (el.TryGetProperty("bindings", out var bnd) && bnd.ValueKind == JsonValueKind.Object)
        {
            cmd.Bindings.RootSearch = ParseBindingSpec(bnd, "root_search");
            cmd.Bindings.Keyword = ParseBindingSpec(bnd, "keyword");
            cmd.Bindings.ActionPanel = ParseBindingSpec(bnd, "action_panel");
            cmd.Bindings.Staging = ParseBindingSpec(bnd, "staging");
            cmd.Bindings.Shortcut = ParseBindingSpec(bnd, "shortcut");
        }

        return cmd;
    }

    private static CommandBindingSpecDto? ParseBindingSpec(JsonElement parent, string name)
    {
        if (!parent.TryGetProperty(name, out var el) || el.ValueKind != JsonValueKind.Object)
            return null;
        return new CommandBindingSpecDto
        {
            Priority = el.TryGetProperty("priority", out var p) && p.TryGetInt32(out var pv) ? pv : 0,
            ShortcutCombo = el.TryGetProperty("shortcut_combo", out var sc) && sc.ValueKind == JsonValueKind.String
                ? sc.GetString() : null,
            Trigger = el.TryGetProperty("trigger", out var tr) && tr.ValueKind == JsonValueKind.String
                ? tr.GetString() : null,
            ShowInRootSearch = !el.TryGetProperty("show_in_root_search", out var srs)
                || srs.ValueKind != JsonValueKind.False,
        };
    }

    internal static IReadOnlyList<AliasEntry> ParseAliasList(JsonElement resp)
    {
        var items = new List<AliasEntry>();
        if (!resp.TryGetProperty("items", out var arr) || arr.ValueKind != JsonValueKind.Array)
            return items;
        foreach (var el in arr.EnumerateArray())
        {
            if (!el.TryGetProperty("target", out var targetValue)
                || targetValue.ValueKind != JsonValueKind.Object
                || !targetValue.TryGetProperty("kind", out var kind)
                || kind.ValueKind != JsonValueKind.String
                || !targetValue.TryGetProperty("value", out var value)
                || value.ValueKind != JsonValueKind.String)
            {
                continue;
            }
            var words = new List<string>();
            if (el.TryGetProperty("words", out var wordsValue) && wordsValue.ValueKind == JsonValueKind.Array)
            {
                foreach (var word in wordsValue.EnumerateArray())
                    if (word.ValueKind == JsonValueKind.String)
                        words.Add(word.GetString() ?? "");
            }
            var boundAt = el.TryGetProperty("bound_at_utc", out var bound)
                && bound.TryGetInt64(out var parsedBound)
                ? parsedBound
                : 0L;
            items.Add(new AliasEntry(
                new ActionTarget(kind.GetString() ?? "", value.GetString() ?? ""),
                words,
                boundAt));
        }
        return items;
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

        // K2 §4.2：命令段字段。缺字段时默认值 = 内置动作语义。
        var invocationKind = el.TryGetProperty("invocation_kind", out var ik)
            ? ik.GetString() ?? "builtin_action"
            : "builtin_action";
        var commandId = el.TryGetProperty("command_id", out var cid)
            && cid.ValueKind == JsonValueKind.String
                ? cid.GetString()
                : null;
        var isEnabled = !el.TryGetProperty("is_enabled", out var en) || en.ValueKind != JsonValueKind.False;
        var disabledReason = el.TryGetProperty("disabled_reason", out var dr)
            && dr.ValueKind == JsonValueKind.String
                ? dr.GetString()
                : null;

        return new ActionItem(id, label, glyph, submenu, header)
        {
            InvocationKind = invocationKind,
            CommandId = commandId,
            IsEnabled = isEnabled,
            DisabledReason = disabledReason,
        };
    }

    /// <summary>
    /// 查询/状态类请求：走搜索通道，一律带读超时。
    /// </summary>
    private Task<JsonElement> SendAsync(
        object request,
        CancellationToken ct,
        TimeSpan? readTimeout = null) =>
        _query.SendAsync(request, ct, readTimeout);

    /// <summary>
    /// AUDIT-2026-08-18 R-A6: 动作通道退化到搜索通道后的宽松读超时。
    /// 此前退化路径是无限期读且豁免 watchdog wedge 判定——broker 对该 run_action
    /// 永不应答时，后续搜索全堆在 _ioLock 后，UI 永远"搜索中"且无自愈。
    /// 60 秒覆盖属性页/复制确认等系统对话框的合理上限；超时走既有读超时路径
    /// （销毁流走重连，绝不重发——约束见 SendAsync 注释）。带读超时即意味着
    /// 该请求按查询计数，不再豁免 wedge 判定。internal set 供测试缩短。
    /// </summary>
    internal static TimeSpan FallbackActionReadTimeout { get; set; } = TimeSpan.FromSeconds(60);

    /// <summary>
    /// FRESH-AUDIT-2 F6: 动作通道自身的宽松读超时（5 分钟级）。
    /// 此前动作通道无超时且豁免 wedge 判定——broker 无对话框状态下死锁时，
    /// execute 永久挂起，后续动作在通道 _ioLock 上无限排队，无自愈。
    /// 5 分钟覆盖属性页/复制确认/大目录 zip 等合法长等待的合理上限；超时销毁
    /// 动作连接走重连（查询通道不受影响），绝不重发（删除/复制不得执行两次）。
    /// 超时≠未执行——mutation 类的文案由 ViewModel 按"结果未知"处理（F7）。
    /// internal set 供测试缩短。
    /// </summary>
    internal static TimeSpan ActionReadTimeout { get; set; } = TimeSpan.FromMinutes(5);

    /// <summary>
    /// 交互式动作类请求（execute / reveal / run_action，审计 C1）：走独立的动作通道，
    /// 无读超时——属性页/复制确认等系统对话框可以合法占住这条连接任意久，
    /// 期间搜索通道不受影响，每击键搜索照常。
    ///
    /// 退化路径：动作通道连不上（broker listener 槽位耗尽、管道瞬时 BUSY 等）时
    /// 退回搜索通道发送，行为等同修复前（动作阻塞搜索）而不是让动作彻底失败。
    /// 退回**只在确知请求尚未写出**时发生（<see cref="PipeNotConnectedException"/>），
    /// 协议无请求 id、按行严格配对，写出后失败绝不能重发——否则删除/复制会执行两次。
    /// </summary>
    private async Task<JsonElement> SendActionAsync(object request, CancellationToken ct)
    {
        try
        {
            // F6: 动作通道也带宽松超时（5 分钟）——无对话框死锁可自愈；
            // 对话框等合法长等待超过 5 分钟时按超时处理，文案由 F7 明示"结果未知"。
            return await _action.SendAsync(request, ct, ActionReadTimeout).ConfigureAwait(false);
        }
        catch (PipeNotConnectedException)
        {
            return await _query.SendAsync(request, ct, FallbackActionReadTimeout).ConfigureAwait(false);
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
            // K0 T10.3：命令行用 "command:" 前缀 + execute_id（命令 id）作 RowKey。
            // 默认 ContainerKey 含 Title，描述变更会让容器删旧插新，重演网页行
            // 图标闪烁缺陷。命令 id 是唯一稳定身份。
            RowKey = kind == "command" ? $"command:{id}" : null,
        };
    }

    /// <summary>
    /// 确保后端进程在运行；未运行则定位可执行文件并启动，并纳入 Job Object。
    /// internal 供 C-D9 测试直接锚定"Dispose 后不再拉进程"。
    /// </summary>
    internal void EnsureBackendRunning()
    {
        // C-D9: Dispose 进行中/已完成——绝不再拉新进程，否则 Kill 后又留下孤儿。
        if (_disposed)
            return;
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
        // 释放旧 Process 对象持有的进程句柄再启动新的：
        // broker 崩溃循环下 watchdog 每 15 秒重拉一次，不 Dispose 会持续累积句柄。
        try { _backend?.Dispose(); } catch { /* 已释放/已退出 */ }
        _backend = null;
        _backend = (ProcessStarterForTest ?? Process.Start)(psi)
            ?? throw new IOException("启动 prism-core.exe 失败");

        // 纳入 Job Object：Prism 崩溃/被杀时 OS 自动回收 broker，不留孤儿占管道。
        // 测试注入的 Process 对象没有真实句柄，跳过收编（测试只锚定启动次数）。
        if (ProcessStarterForTest is null)
        {
            _jobGuard ??= new JobObjectGuard();
            _jobGuard.Assign(_backend.Handle);
        }
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

    public void Dispose()
    {
        // G3（FRESH-AUDIT-2）：重活（排干 watchdog 最多 12s、拆连接、句柄回收）
        // 移后台线程，退出路径不阻塞 UI。
        // M7（全仓复审 2026-08-22）：Kill broker 改为**同步**先做——WPF 关闭不等待
        // ThreadPool，墓碑+后台队列的组合曾让进程在 DisposeCore 走到 Kill 之前就
        // 退出（watchdog 排干最长 12s 排在 Kill 之前）；而 KILL_ON_JOB_CLOSE 已被
        // JobObjectGuard 刻意移除，再无兜底 ⇒ 孤儿 broker 占着 \\.\pipe\prism-core。
        // 同步 Kill 很快（不杀树），watchdog tick 入口与 EnsureBackendRunning 都已
        // 检查 _disposed 墓碑，不会把杀掉的 broker 重新拉活。
        if (_disposed)
            return;
        _disposed = true;
        try
        {
            if (_backend is { HasExited: false })
                _backend.Kill();
        }
        catch { /* ignore */ }
        ThreadPool.QueueUserWorkItem(_ => DisposeCore());
    }

    private void DisposeCore()
    {
        // C-D9: 先立墓碑再拆 timer——tick 入口与 EnsureBackendRunning 都会检查它。
        // （墓碑已在 Dispose 里同步立好。）

        // C-D9: Timer.Dispose() 不等在途回调；用带等待句柄的 Dispose 排干在途 tick
        // （tick 内部重连有 10s 上限，等待 12s 封顶），保证此后 Kill 的 broker
        // 不会被一个迟到的 tick 重新拉活成孤儿。
        if (_watchdog is not null)
        {
            using var drained = new ManualResetEvent(false);
            _watchdog.Dispose(drained);
            drained.WaitOne(TimeSpan.FromSeconds(12));
            _watchdog = null;
        }

        // 两条连接都要拆（动作通道可能带着一个未完成的交互式动作，
        // DisposeStreamOnly 走 ThreadPool，不会让退出路径等 I/O）。
        _query.Dispose();
        _action.Dispose();

        try
        {
            // Bug 3: 只杀 broker 本身，不杀进程树——broker 经 ShellExecuteExW 打开的用户应用
            // 是 broker 的子进程，entireProcessTree:true 会把它们一起杀掉。KILL_ON_JOB_CLOSE
            // 已移除（JobObjectGuard 不再设该标志），此处也不再杀树，用户应用随 Prism 退出存活。
            // M7：Dispose 已同步 Kill 过；这里对仍存活的 broker 补一刀（幂等，正常为 no-op）。
            if (_backend is { HasExited: false })
                _backend.Kill();
        }
        catch { /* ignore */ }
        _backend?.Dispose();

        // 释放 Job Object：不再设 KILL_ON_JOB_CLOSE（Bug 3），Job Dispose 不杀任何进程。
        // Prism 正常退出时上面 Kill 已处理 broker；Prism 崩溃时 broker 作孤儿留存，
        // 下次启动由 AdoptExistingServer 收编或杀旧拉新。
        _jobGuard?.Dispose();
        _jobGuard = null;
    }

    /// <summary>
    /// 一条 broker 长连接：stream/reader/writer + 独立 IO 锁 + 握手 + 判活时戳。
    /// 搜索通道与动作通道各持一个实例（审计 C1），彼此不共享任何锁，
    /// 因此一条连接上的长时间交互不会阻塞另一条。
    /// </summary>
    private sealed class PipeChannel : IDisposable
    {
        /// <summary>握手读超时（审计 C2）：broker 的 hello 是连接内联同步生成的，没有合法慢握手；3 秒读不到即视为半死连接。</summary>
        private static readonly TimeSpan HandshakeReadTimeout = TimeSpan.FromSeconds(3);

        private readonly string _pipeName;

        /// <summary>诊断用通道名（出现在异常文案里，便于区分是哪条连接断了）。</summary>
        private readonly string _label;

        /// <summary>发送前"管道已断"时的快速重连超时（只连已有管道，从不拉进程）。</summary>
        private readonly TimeSpan _sendReconnectTimeout;

        private readonly SemaphoreSlim _ioLock = new(1, 1);
        private NamedPipeClientStream? _stream;
        private StreamReader? _reader;
        private BoundedLineReader? _lineReader;
        private StreamWriter? _writer;

        // ---- watchdog 判活（审计 C2）：连通 ≠ 健康，还要求最近的查询请求有响应 ----
        // 最近一次成功读到响应（含 hello 握手）的 UTC Ticks。
        private long _lastResponseTicks;
        // 最近一次发出"带读超时"的查询类请求的 UTC Ticks。
        private long _lastQuerySentTicks;
        // 正在等待"无读超时"响应的动作类请求数（属性页/复制确认可合法等待任意久，
        // 期间绝不能把连接判成半死去强制重建——那会杀掉在途动作交互）。
        private int _pendingSlowActionReads;

        public PipeChannel(string pipeName, string label, TimeSpan sendReconnectTimeout)
        {
            _pipeName = pipeName;
            _label = label;
            _sendReconnectTimeout = sendReconnectTimeout;
        }

        public bool IsConnected => _stream is { IsConnected: true };
        public long LastResponseTicks => Interlocked.Read(ref _lastResponseTicks);
        public long LastQuerySentTicks => Interlocked.Read(ref _lastQuerySentTicks);
        public bool HasPendingSlowRead => Volatile.Read(ref _pendingSlowActionReads) != 0;

        /// <summary>
        /// AUDIT-2026-08-18 R-A8: 当前连接的管道服务端进程 ID（握手成功后捕获，
        /// 断开清空）。用于识别"连上的是谁的 broker"——孤儿还是本进程拉起的。
        /// </summary>
        public int? ServerProcessId { get; private set; }

        // K0 T9：能力协商结果——Hello 回包解析后写入，断开清空（与 ServerProcessId 同处理）。
        /// <summary>本通道是否协商了 commands_v1 能力（Hello 回包 features 含之）。</summary>
        public bool HasCommands { get; private set; }
        /// <summary>broker 构建指纹（Hello 回包 build_id，目前被忽略）。</summary>
        public string? BuildId { get; private set; }
        /// <summary>命令目录代际（Hello 回包 command_catalog_generation）。</summary>
        public ulong? CommandCatalogGeneration { get; private set; }

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetNamedPipeServerProcessId(
            Microsoft.Win32.SafeHandles.SafePipeHandle handle, out uint serverProcessId);

        private static int? TryGetServerProcessId(NamedPipeClientStream stream)
        {
            try
            {
                return GetNamedPipeServerProcessId(stream.SafePipeHandle, out var pid) && pid != 0
                    ? (int)pid
                    : null;
            }
            catch
            {
                return null;
            }
        }

        /// <summary>
        /// 连接入口：先试已有管道，连不上再走 <paramref name="ensureBackend"/>（可为 null：
        /// 动作通道从不拉进程）后重连。_ioLock 串行化了所有调用者——第一个连上后，
        /// 后续拿到锁会看到已连接直接返回，不需要额外的重入标志。
        ///
        /// AUDIT-2026-08-18 R-A8: <paramref name="adoptExisting"/> 在连上已有管道后、
        /// 返回调用方前被调用（参数=服务端 PID）；返回 false 表示调用方拒绝这次复用
        /// （孤儿 broker 收编失败已清理），本方法丢弃连接走拉新路径。
        /// </summary>
        public async Task ConnectOrReconnectAsync(
            CancellationToken ct,
            bool forceReconnect,
            Action? ensureBackend,
            Func<int, bool>? adoptExisting = null)
        {
            await _ioLock.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                // 拿到锁后复查：可能另一个调用者已经在我们等锁期间建好了连接。
                if (!forceReconnect && IsConnected)
                    return;

                DisposeStreamOnly();

                // 第一优先：连已有管道。孤儿 broker 只要还活着，就复用它。
                if (await TryConnectPipeOnlyAsync(TimeSpan.FromSeconds(2), ct).ConfigureAwait(false))
                {
                    var serverPid = ServerProcessId;
                    if (serverPid is null || adoptExisting is null || adoptExisting(serverPid.Value))
                        return;

                    // 复用被拒（调用方已清理孤儿进程）：丢弃这条连接，走拉新路径。
                    DisposeStreamOnly();
                }

                if (ensureBackend is null)
                    throw new PipeNotConnectedException($"{_label}未连接");

                // 连不上才拉新进程，拉起后立即纳入 Job Object（Prism 崩溃时 OS 自动回收）。
                ensureBackend();
                await ConnectInnerAsync(TimeSpan.FromSeconds(10), ct).ConfigureAwait(false);
            }
            finally
            {
                _ioLock.Release();
            }
        }

        /// <summary>持锁版建连（测试直接调用；生产路径都经 ConnectOrReconnectAsync/SendAsync）。</summary>
        public async Task ConnectLockedAsync(TimeSpan timeout, CancellationToken ct)
        {
            await _ioLock.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                await ConnectInnerAsync(timeout, ct).ConfigureAwait(false);
            }
            finally
            {
                _ioLock.Release();
            }
        }

        /// <summary>尝试连接已有 broker 管道（不拉进程）。成功返回 true。</summary>
        private async Task<bool> TryConnectPipeOnlyAsync(TimeSpan timeout, CancellationToken ct)
        {
            try
            {
                await ConnectInnerAsync(timeout, ct).ConfigureAwait(false);
                return true;
            }
            catch
            {
                return false;
            }
        }

        /// <summary>
        /// 连接命名管道，UTF-8 无 BOM，按行(\n)收发。
        /// 调用方必须已持有 _ioLock。内部直接读写握手，不走 SendAsync（避免重入死锁）。
        /// </summary>
        private async Task ConnectInnerAsync(TimeSpan timeout, CancellationToken ct)
        {
            DisposeStreamOnly();

            var stream = new NamedPipeClientStream(
                ".", _pipeName, PipeDirection.InOut, PipeOptions.Asynchronous);

            await stream.ConnectAsync((int)timeout.TotalMilliseconds, ct).ConfigureAwait(false);

            var utf8 = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
            _stream = stream;
            _reader = new StreamReader(stream, utf8);
            _lineReader = new BoundedLineReader(_reader);
            _writer = new StreamWriter(stream, utf8) { AutoFlush = false, NewLine = "\n" };

            // 任何握手失败（超时/关闭/协议不符）都必须清掉半开的流对象，否则
            // IsConnected 可能仍为 true，后续请求挂在一条死管道上。
            try
            {
                await HandshakeAsync().ConfigureAwait(false);
                // R-A8: 握手成功即记录服务端 PID，供上层判定"连的是谁的 broker"。
                ServerProcessId = TryGetServerProcessId(stream);
            }
            catch
            {
                DisposeStreamOnly();
                throw;
            }
        }

        /// <summary>
        /// 握手直接读写——调用方已持 _ioLock，不能再调 SendAsync（它会再次 WaitAsync 导致死锁）。
        /// 此前 hello 读无超时，broker 半死（连上不握手）时 StartAsync/快速重连会
        /// 永久挂住并持有 _ioLock，冻结全部请求。
        ///
        /// 超时用 Task.WhenAny 竞速而非 CancellationToken：StreamReader.ReadLineAsync(token)
        /// 在 NamedPipeClientStream 上不能可靠取消挂起的 overlapped I/O（.NET 已知限制）。
        /// 超时后直接 DisposeStreamOnly——底层 stream 的 CancelIoEx 会让挂起的读立即返回。
        /// </summary>
        internal async Task HandshakeAsync()
        {
            // K0 T9：hello 发 capabilities。受 AdvertiseCommandCapability 静态开关控制——
            // 测试注入点兼紧急关闭手段（与 ActionReadTimeout 同款先例）。设计 P7 明确
            // K0 不往 settings.json 加命令字段（旧 WPF 全量保存会丢弃未知字段）。
            var helloPayload = new Dictionary<string, object?>
            {
                ["type"] = "hello",
                ["protocol"] = ProtocolVersion,
            };
            if (AdvertiseCommandCapability)
                helloPayload["capabilities"] = new[] { "commands_v1" };
            var json = JsonSerializer.Serialize(helloPayload);
            await _writer!.WriteLineAsync(json.AsMemory(), CancellationToken.None).ConfigureAwait(false);
            await _writer.FlushAsync(CancellationToken.None).ConfigureAwait(false);

            var readTask = _lineReader!.ReadLineAsync(CancellationToken.None);
            var timeoutTask = Task.Delay(HandshakeReadTimeout);
            // L 批次（FRESH-AUDIT-3-2026-08-20）：延迟任务完成后释放其计时器——
            // 读先完成时不再等 delay 自然到期。注意 Task.Dispose 对未完成任务
            // 会抛 InvalidOperationException，所以只在完成态释放。
            try
            {
            string? line;
            if (readTask == await Task.WhenAny(readTask, timeoutTask).ConfigureAwait(false))
            {
                // L39（全仓复审 2026-08-22）：await 而非 .Result——WhenAny 只保证
                // 完成，不保证成功；.Result 会把 IOException 包成 AggregateException，
                // StartAsync 的过滤层只认 IOException，真实的 broker 错误既不上报
                // 也不进日志，只剩一句泛化的「无法连接到后端」。
                line = await readTask.ConfigureAwait(false);
            }
            else
            {
                // 超时：销毁底层 stream 让挂起的 ReadLineAsync 立即返回（CancelIoEx），
                // 清理交给 ConnectInnerAsync 的 catch 块统一处理（此处已把字段清空）。
                DisposeStreamOnly();
                throw new IOException(
                    $"{_label}握手超时（{Math.Round(HandshakeReadTimeout.TotalSeconds)} 秒未收到 hello）");
            }

            if (line is null)
                throw new IOException("后端在握手时关闭了管道");

            using var doc = JsonDocument.Parse(line);
            var root = doc.RootElement;
            if (!root.TryGetProperty("type", out var type)
                || type.GetString() != "hello"
                || !root.TryGetProperty("protocol", out var protocol)
                || !protocol.TryGetInt32(out var version)
                || version != ProtocolVersion)
            {
                throw new IOException("Broker protocol version mismatch");
            }

            // K0 T9：解析 features / command_catalog_generation / build_id。
            // 旧 broker 回包不含这些 key → HasCommands=false（逐字节兼容）。
            HasCommands = root.TryGetProperty("features", out var features)
                && features.ValueKind == JsonValueKind.Array
                && features.EnumerateArray()
                    .Any(f => f.ValueKind == JsonValueKind.String
                              && f.GetString() == "commands_v1");
            if (root.TryGetProperty("command_catalog_generation", out var cmdGen)
                && cmdGen.TryGetUInt64(out var parsedGen))
                CommandCatalogGeneration = parsedGen;
            else
                CommandCatalogGeneration = null;
            if (root.TryGetProperty("build_id", out var buildId)
                && buildId.ValueKind == JsonValueKind.String)
                BuildId = buildId.GetString();
            else
                BuildId = null;

            // 握手成功即最近一次有效响应（watchdog 判活参考）。
            Interlocked.Exchange(ref _lastResponseTicks, DateTime.UtcNow.Ticks);
            }
            finally
            {
                if (timeoutTask.IsCompleted) timeoutTask.Dispose();
            }
        }

        /// <summary>
        /// 发送一条请求，读取一行响应并解析为 JSON。同通道内串行化以保证请求/响应配对
        /// （协议无请求 id，严格按行配对）。
        /// 重要：一旦请求写出，必须把对应响应读完，绝不能因 CancellationToken 中途放弃读——
        /// 否则管道里会残留旧响应，下一次 Search 会读到上一次的结果（表现为高亮/列表错位）。
        /// 取消只作用于"等锁"和"业务层丢弃结果"；ViewModel 用 seq 丢弃过期 UI 更新。
        /// 管道断开时在锁内做一次快速重连尝试，连不上抛 <see cref="PipeNotConnectedException"/>
        /// 快速失败——不拉进程（交给 watchdog），避免阻塞搜索路径。
        /// 读超时：查询类请求（搜索/状态/动作列表等）传入 <paramref name="readTimeout"/>，
        /// 超时取消配对读并整条销毁流——流被丢弃后不存在孤儿响应问题，
        /// 重连走 hello 握手重新同步。动作类请求可能弹交互式系统对话框（属性页/复制确认），
        /// 合法等待任意久，因此不传超时；它们走独立的动作通道，占住的只是自己那条连接。
        /// </summary>
        public async Task<JsonElement> SendAsync(
            object request,
            CancellationToken ct,
            TimeSpan? readTimeout)
        {
            await _ioLock.WaitAsync(ct).ConfigureAwait(false);
            try
            {
                // 管道断开时做一次快速重连（仅连已有管道，不拉进程）。
                // 连不上就快速失败，让用户看到错误而非卡住——watchdog 会在后台拉进程重连。
                if (_stream is not { IsConnected: true } || _writer is null || _reader is null)
                {
                    DisposeStreamOnly();
                    // 只尝试连已有 broker 管道，不拉进程——拉进程交给 StartAsync/watchdog。
                    await TryConnectPipeOnlyAsync(_sendReconnectTimeout, ct).ConfigureAwait(false);
                    if (_stream is not { IsConnected: true })
                        throw new PipeNotConnectedException($"{_label}未连接");
                }

                CancellationTokenSource? readDeadline = null;
                try
                {
                    var json = JsonSerializer.Serialize(request);
                    // 写出后必须完成配对读，故读写使用 None，避免取消留下孤儿响应。
                    await _writer!.WriteLineAsync(json.AsMemory(), CancellationToken.None).ConfigureAwait(false);
                    await _writer.FlushAsync(CancellationToken.None).ConfigureAwait(false);

                    // watchdog 判活（审计 C2）：记录"查询已发出"时刻；动作类长超时
                    // 请求（属性页/复制确认可合法等待用户数分钟）单独计数——判活与
                    // AdoptExistingServer 的"在途对话框不杀旧 broker"守卫都依赖它。
                    // F4（全仓检验 2026-08-25 第二轮）：F6 之后所有调用都带超时
                    //（动作通道 5 分钟、退化 60 秒），`readTimeout is null` 恒假，
                    // 计数成死码、守卫失效。改为按"读超时 ≥30 秒即可能在等用户面前
                    // 的系统对话框"计数（查询 500ms/握手 3s 不计，语义不变）。
                    var slowRead = readTimeout is null || readTimeout >= TimeSpan.FromSeconds(30);
                    if (slowRead)
                        Interlocked.Increment(ref _pendingSlowActionReads);
                    if (readTimeout is not null)
                        Interlocked.Exchange(ref _lastQuerySentTicks, DateTime.UtcNow.Ticks);

                    string? line;
                    try
                    {
                        if (readTimeout is { } timeout)
                        {
                            readDeadline = new CancellationTokenSource(timeout);
                        }
                        line = await _lineReader!.ReadLineAsync(readDeadline?.Token ?? CancellationToken.None).ConfigureAwait(false)
                            ?? throw new IOException("后端在返回响应前关闭了管道");
                    }
                    finally
                    {
                        if (slowRead)
                            Interlocked.Decrement(ref _pendingSlowActionReads);
                    }
                    Interlocked.Exchange(ref _lastResponseTicks, DateTime.UtcNow.Ticks);

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
                catch (OperationCanceledException) when (readDeadline?.IsCancellationRequested == true)
                {
                    // 读超时：broker 活着但不应答。销毁整条流让协议重新同步——
                    // 这不是可复用的连接，直接按传输层失败处理。
                    DisposeStreamOnly();
                    throw new IOException($"后端响应超时（{Math.Round(readTimeout!.Value.TotalSeconds)} 秒）");
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
                    // 传输层失败：释放管道，IsConnected 变 false，watchdog 会重连。
                    // 注意这里**不能**是 PipeNotConnectedException——请求已经写出，
                    // 换连接重发会导致动作执行两次。
                    DisposeStreamOnly();
                    throw;
                }
                finally
                {
                    readDeadline?.Dispose();
                }
            }
            finally
            {
                _ioLock.Release();
            }
        }

        private void DisposeStreamOnly()
        {
            // NamedPipeClientStream + StreamReader 在有 pending overlapped I/O 时
            // Dispose 可能阻塞调用线程等 I/O 归还。把三个对象的 Dispose 全丢到
            // ThreadPool——调用线程只清引用，绝不等 I/O 完成。
            var stream = _stream;
            var reader = _reader;
            var writer = _writer;
            _stream = null;
            _reader = null;
            _lineReader = null;
            _writer = null;
            ServerProcessId = null;
            // K0 T9：能力协商结果随连接清空（与 ServerProcessId 同处理）。
            HasCommands = false;
            BuildId = null;
            CommandCatalogGeneration = null;
            // 先关闭 stream（CancelIoEx），再关闭 reader/writer——顺序在 ThreadPool
            // 上执行不受调用线程影响。stream 先关让 reader 的 pending read 先被取消。
            ThreadPool.QueueUserWorkItem(_ =>
            {
                try { stream?.Dispose(); } catch { }
                try { reader?.Dispose(); } catch { }
                try { writer?.Dispose(); } catch { }
            });
        }

        public void Dispose()
        {
            DisposeStreamOnly();
            // G3: 退出路径上可能仍有排队等锁的发送者，Dispose 竞态会向它们抛
            // ObjectDisposedException——吞掉（进程即将退出，句柄由 OS 回收）。
            try { _ioLock.Dispose(); } catch (ObjectDisposedException) { }
        }
    }
}

/// <summary>
/// 建连失败：请求**尚未写出**，因此调用方可以安全地换一条连接重发（审计 C1 的
/// 动作通道→搜索通道退化路径）。写出之后的任何失败都用普通 <see cref="IOException"/>，
/// 绝不可重发。
/// </summary>
public sealed class PipeNotConnectedException : IOException
{
    public PipeNotConnectedException(string message) : base(message)
    {
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
    string? RootMessage = null,
    // K0 T10.7：末位带默认值 → 全部现有构造点（含测试）零改动。
    // Cacheable 与 broker 的 skip_serializing_if=is_true 配对：字段缺失 → true。
    bool Cacheable = true,
    ulong? CommandCatalogGeneration = null);
