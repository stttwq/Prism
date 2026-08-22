using System.Runtime.InteropServices;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Windows.Threading;

namespace Prism.Services;

/// <summary>
/// 单实例守护：命名互斥锁保证全局只有一个 Prism.exe；第二个实例检测到已有实例后，
/// 通过命名管道向前者发送 show 指令唤出搜索窗，然后立即退出。
///
/// 互斥锁在 OnStartup 最早期获取，确保不会启动第二个 prism-core.exe / 注册第二个热键。
/// 第一个实例持有锁后启动前台监听管道，等待后续实例的唤出信号。
/// </summary>
internal sealed class SingleInstance : IDisposable
{
    private const string MutexName = @"Local\PrismSingleInstance";
    private const string PipeName = "prism-foreground";
    private const string ShowCommand = "show";

    private const int ERROR_ALREADY_EXISTS = 183;

    private readonly string _mutexName;
    private readonly string _pipeName;

    private Mutex? _mutex;
    private bool _ownsMutex;
    private CancellationTokenSource? _listenCts;
    private Task? _listenTask;
    /// <summary>L24：连续监听失败计数，用于短退避到慢退避的升级；成功连接后清零。</summary>
    private int _listenFailures;

    /// <summary>
    /// M2（全仓复审 2026-08-22）：测试注入构造。生产用默认全局名；测试注入
    /// 随机名，避免与真机运行的 Prism.exe 抢同一个 Local\ 互斥锁——此前
    /// SingleInstanceDumbClientTests 在任何装了 Prism 的机器上必败（12ms 即输），
    /// 让「dotnet 全绿」门在最需要跑它的机器上失效。
    /// </summary>
    internal SingleInstance(string? mutexName = null, string? pipeName = null)
    {
        _mutexName = mutexName ?? MutexName;
        _pipeName = pipeName ?? PipeName;
    }

    /// <summary>
    /// 尝试获取命名互斥锁。返回 true 表示这是首个实例（调用方应继续启动）；
    /// 返回 false 表示已有实例在运行（调用方应唤出前者后退出）。
    /// </summary>
    public bool TryAcquire()
    {
        _mutex = new Mutex(initiallyOwned: true, name: _mutexName, createdNew: out _ownsMutex);
        return _ownsMutex;
    }

    /// <summary>向已运行的实例发送 show 指令，唤出其搜索窗口。</summary>
    public void SignalExistingInstance()
    {
        try
        {
            using var client = new NamedPipeClientStream(
                ".", _pipeName, PipeDirection.Out, PipeOptions.Asynchronous);
            client.Connect((int)TimeSpan.FromSeconds(3).TotalMilliseconds);
            using var writer = new StreamWriter(client, new UTF8Encoding(false))
            {
                AutoFlush = true,
                NewLine = "\n",
            };
            writer.WriteLine(ShowCommand);
        }
        catch
        {
            // 已有实例可能正在启动或已退出；静默忽略，第二个进程照常退出。
        }
    }

    /// <summary>
    /// 启动前台管道监听，收到 show 指令时在 UI 线程回调唤出窗口。
    /// 仅首个实例调用；应在获取互斥锁之后、主界面就绪之后启动。
    /// </summary>
    public void StartForegroundListener(Dispatcher dispatcher, Action onShowRequested)
    {
        _listenCts = new CancellationTokenSource();
        var token = _listenCts.Token;
        _listenTask = Task.Run(() => ListenLoop(dispatcher, onShowRequested, token), token);
    }

    private async Task ListenLoop(Dispatcher dispatcher, Action onShowRequested, CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            NamedPipeServerStream? server = null;
            try
            {
                // FirstPipeInstance 声明独占：我们已持有单实例互斥锁，
                // 正常情况下不存在第二个监听者。
                server = new NamedPipeServerStream(
                    _pipeName, PipeDirection.In, 1,
                    PipeTransmissionMode.Byte,
                    PipeOptions.Asynchronous | PipeOptions.FirstPipeInstance);
                await server.WaitForConnectionAsync(ct).ConfigureAwait(false);
                _listenFailures = 0;

                // AUDIT-2026-08-18 C-D8: 读加 2s 超时。哑客户端（连上不发数据）
                // 此前会无限期占死这个监听槽，之后所有双开唤出全部失灵。
                // 超时后 Dispose server（取消挂起的 overlapped I/O），循环继续。
                using var readCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
                readCts.CancelAfter(TimeSpan.FromSeconds(2));
                string? line;
                using (var reader = new StreamReader(server, new UTF8Encoding(false)))
                {
                    try
                    {
                        line = await reader.ReadLineAsync(readCts.Token).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException) when (!ct.IsCancellationRequested)
                    {
                        // 读超时：哑连接，直接丢弃本连接（finally Dispose）。
                        line = null;
                    }
                }
                if (line is not null && line.Trim() == ShowCommand)
                {
                    // fire-and-forget：在 UI 线程唤出窗口，不阻塞管道监听循环。
                    _ = dispatcher.BeginInvoke(new Action(onShowRequested),
                        DispatcherPriority.Normal);
                }
            }
            catch (OperationCanceledException)
            {
                break;
            }
            catch
            {
                // 单个连接/构造异常不应终止监听循环，但也不能零退避空转烧满一核。
                // L24（全仓复审 2026-08-22）：FirstPipeInstance 的管道名释回不是
                // 瞬时的，上一连接 Dispose 后立刻重建可能撞 IOException——先短退避
                // （50ms×10）让循环顶快速重建监听槽；连续失败耗尽才落回 1s 慢退避。
                // 慢退避期间监听槽是空的，双开唤出会撞进 SignalExistingInstance
                // 的静默 catch，用户按启动器毫无反应。
                var delay = _listenFailures < 10
                    ? TimeSpan.FromMilliseconds(50)
                    : TimeSpan.FromSeconds(1);
                _listenFailures++;
                try
                {
                    await Task.Delay(delay, ct).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
            }
            finally
            {
                try { server?.Dispose(); } catch { /* ignore */ }
            }
        }
    }

    public void Dispose()
    {
        try { _listenCts?.Cancel(); } catch { /* ignore */ }
        try { _listenTask?.Wait(TimeSpan.FromSeconds(2)); } catch { /* ignore */ }
        _listenCts?.Dispose();

        if (_ownsMutex && _mutex is not null)
        {
            try { _mutex.ReleaseMutex(); } catch { /* ignore */ }
        }
        _mutex?.Dispose();
    }
}
