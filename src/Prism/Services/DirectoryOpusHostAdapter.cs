using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Text;
using System.Xml;
using System.Xml.Linq;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// Directory Opus 外部命令运行时。生产实现定位 <c>dopusrt.exe</c> 并用参数数组调用；
/// 单测注入 fake，保证无 Opus 的 CI 可跑。
/// </summary>
public interface IDirectoryOpusRuntime
{
    /// <summary>dopusrt 可执行路径；找不到时为 null。</summary>
    string? ResolveDopusRtPath(string? dopusProcessPath);

    /// <summary>
    /// 运行 dopusrt。参数必须已是结构化列表（调用方负责 quoting 边界），
    /// 实现层用 <see cref="ProcessStartInfo.ArgumentList"/> 传递，禁止 <c>cmd /c</c> 拼接。
    /// </summary>
    DirectoryOpusCommandResult Run(
        string dopusRtPath,
        IReadOnlyList<string> arguments,
        TimeSpan timeout);
}

public sealed record DirectoryOpusCommandResult(
    bool TimedOut,
    int ExitCode,
    string StandardOutput,
    string StandardError,
    bool ExecutableMissing = false)
{
    public static DirectoryOpusCommandResult Missing { get; } =
        new(false, -1, "", "", ExecutableMissing: true);
}

/// <summary>
/// Directory Opus 13.23 adapter：官方 <c>dopusrt</c> 外部命令，无注入。
/// 能力位：ReadFolder | NavigateFolder | RevealInHost。
/// </summary>
public sealed class DirectoryOpusHostAdapter : IHostAdapter
{
    public const HostCapability SupportedCapabilities =
        HostCapability.ReadFolder | HostCapability.NavigateFolder | HostCapability.RevealInHost;

    // 只认主程序 dopus；dopusrt 是外部命令宿主，不能当作 Lister 前台。
    private static readonly HashSet<string> OpusProcessNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "dopus",
    };

    private readonly Func<bool> _isEnabled;
    private readonly INativeWindowQuery _windows;
    private readonly IHostProcessGuard _guard;
    private readonly IDirectoryOpusRuntime _runtime;
    private readonly TimeSpan _commandTimeout;

    public DirectoryOpusHostAdapter(
        Func<bool>? isEnabled = null,
        INativeWindowQuery? windows = null,
        IHostProcessGuard? guard = null,
        IDirectoryOpusRuntime? runtime = null,
        TimeSpan? commandTimeout = null)
    {
        _isEnabled = isEnabled ?? (() => true);
        _windows = windows ?? new Win32NativeWindowQuery();
        _guard = guard ?? new Win32HostProcessGuard();
        _runtime = runtime ?? new ProcessDirectoryOpusRuntime();
        _commandTimeout = commandTimeout ?? TimeSpan.FromSeconds(3);
    }

    public HostKind Kind => HostKind.DirectoryOpus;

    public bool IsEnabled => _isEnabled();

    public HostDetection Detect(IntPtr foregroundWindow)
    {
        if (!IsEnabled)
            return HostDetection.NotHost(Kind, HostFailureReason.AdapterDisabled);
        if (foregroundWindow == IntPtr.Zero || !_windows.IsAlive(foregroundWindow))
            return HostDetection.NotHost(Kind, HostFailureReason.HostGone);

        try
        {
            if (_guard.IsElevatedAboveCurrent(foregroundWindow))
                return HostDetection.NotHost(Kind, HostFailureReason.HostElevated);

            var processName = _windows.GetProcessName(foregroundWindow);
            if (string.IsNullOrEmpty(processName) || !OpusProcessNames.Contains(processName))
                return HostDetection.NotHost(Kind, HostFailureReason.NotThisHost);

            return HostDetection.Host(Kind, SupportedCapabilities);
        }
        catch
        {
            return HostDetection.NotHost(Kind, HostFailureReason.DetectFailed);
        }
    }

    public HostFolder GetFolder(IntPtr hostWindow)
    {
        if (!IsEnabled)
            return HostFolder.Failure(HostFailureReason.AdapterDisabled);
        if (hostWindow == IntPtr.Zero || !_windows.IsAlive(hostWindow))
            return HostFolder.Failure(HostFailureReason.HostGone);

        try
        {
            if (_guard.IsElevatedAboveCurrent(hostWindow))
                return HostFolder.Failure(HostFailureReason.HostElevated);

            var detection = Detect(hostWindow);
            if (!detection.IsHost)
                return HostFolder.Failure(detection.Reason == HostFailureReason.None
                    ? HostFailureReason.NotThisHost
                    : detection.Reason);

            var dopusPath = _windows.GetProcessPath(hostWindow);
            var dopusRt = _runtime.ResolveDopusRtPath(dopusPath);
            if (string.IsNullOrEmpty(dopusRt))
                return HostFolder.Failure(HostFailureReason.FolderUnavailable);

            var tempFile = Path.Combine(
                Path.GetTempPath(),
                "prism-dopus-" + Guid.NewGuid().ToString("N") + ".txt");
            try
            {
                // 官方 /info 接口：写出所有 Lister 当前路径。参数数组传递，含空格路径安全。
                var result = _runtime.Run(
                    dopusRt,
                    ["/info", tempFile + ",paths"],
                    _commandTimeout);

                if (result.ExecutableMissing || result.TimedOut || result.ExitCode != 0)
                    return HostFolder.Failure(HostFailureReason.FolderUnavailable);

                if (!File.Exists(tempFile))
                    return HostFolder.Failure(HostFailureReason.FolderUnavailable);

                var entries = ReadInfoPaths(tempFile);
                if (entries.Count == 0)
                    return HostFolder.Failure(HostFailureReason.FolderUnavailable);

                // 官方输出直接带 lister 句柄，优先精确匹配，不靠标题猜。
                var sameLister = entries.Where(entry => entry.Lister == hostWindow).ToArray();
                if (sameLister.Length > 0)
                    return ChooseActivePath(sameLister);

                // 没有可用句柄（旧格式或属性缺失）时退回原有标题消歧。
                if (entries.Any(entry => entry.Lister != IntPtr.Zero))
                    return HostFolder.Failure(HostFailureReason.FolderUnavailable);

                var paths = entries
                    .Where(entry => entry.IsActiveTab || entries.All(other => !other.IsActiveTab))
                    .Select(entry => entry.Path)
                    .Distinct(StringComparer.OrdinalIgnoreCase)
                    .ToArray();
                if (paths.Length == 1)
                    return HostFolder.Success(paths[0]);

                var title = _windows.GetWindowTitle(hostWindow) ?? "";
                var matched = paths
                    .Where(p => !string.IsNullOrEmpty(title)
                        && title.Contains(p, StringComparison.OrdinalIgnoreCase))
                    .ToArray();
                if (matched.Length == 1)
                    return HostFolder.Success(matched[0]);

                return HostFolder.Failure(HostFailureReason.FolderUnavailable);
            }
            finally
            {
                try { if (File.Exists(tempFile)) File.Delete(tempFile); }
                catch { /* ignore */ }
            }
        }
        catch (UnauthorizedAccessException)
        {
            return HostFolder.Failure(HostFailureReason.AccessDenied);
        }
        catch
        {
            return HostFolder.Failure(HostFailureReason.FolderUnavailable);
        }
    }

    /// <summary>
    /// 从同一个 Lister 的多条 <c>&lt;path&gt;</c> 里选出用户当前操作的那一条。
    ///
    /// Opus 双面板会为每一侧各输出一个带 <c>active_tab</c> 的记录，所以 <c>active_tab</c>
    /// 只能筛掉「非活动标签」，无法区分左右。真正的焦点侧由 <c>tab_state</c> 给出：
    /// source = <see cref="SourceTabState"/>，destination = 2。因此按
    /// 活动标签 → source 侧 → 唯一路径 逐层收敛，任何一层仍然有歧义就拒绝猜测。
    /// </summary>
    private static HostFolder ChooseActivePath(IReadOnlyList<OpusListerPath> sameLister)
    {
        // 1. 只保留每侧的活动标签；属性整体缺失时保留全部（旧版本兼容）。
        var activeTabs = sameLister.Where(entry => entry.IsActiveTab).ToArray();
        var candidates = activeTabs.Length > 0 ? activeTabs : sameLister.ToArray();

        // 2. 双面板：只有 source 侧是用户当前操作的目录。
        var source = candidates.Where(entry => entry.TabState == SourceTabState).ToArray();
        if (source.Length > 0)
            candidates = source;

        var distinct = candidates
            .Select(entry => entry.Path)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .ToArray();
        return distinct.Length == 1
            ? HostFolder.Success(distinct[0])
            : HostFolder.Failure(HostFailureReason.FolderUnavailable);
    }

    public HostNavigation NavigateOrFill(IntPtr hostWindow, HostNavigationRequest request)
    {
        if (!IsEnabled)
            return HostNavigation.Failure(HostFailureReason.AdapterDisabled);
        if (request.Intent == HostNavigationIntent.FillFileName)
            return HostNavigation.Failure(HostFailureReason.Unsupported);
        if (hostWindow == IntPtr.Zero || !_windows.IsAlive(hostWindow))
            return HostNavigation.Failure(HostFailureReason.HostGone);
        if (string.IsNullOrWhiteSpace(request.Path))
            return HostNavigation.Failure(HostFailureReason.ActionFailed);

        try
        {
            if (_guard.IsElevatedAboveCurrent(hostWindow))
                return HostNavigation.Failure(HostFailureReason.HostElevated);

            if (request.Intent is not (HostNavigationIntent.NavigateFolder or HostNavigationIntent.RevealInHost))
                return HostNavigation.Failure(HostFailureReason.Unsupported);

            var dopusPath = _windows.GetProcessPath(hostWindow);
            var dopusRt = _runtime.ResolveDopusRtPath(dopusPath);
            if (string.IsNullOrEmpty(dopusRt))
                return HostNavigation.Failure(HostFailureReason.ActionFailed);

            // 官方 Go 命令；路径作为独立参数，由 ArgumentList 负责转义。
            // NEWTAB=no 尽量复用现有窗口，而不是默认新开。
            var path = request.Path.Trim();
            var result = _runtime.Run(
                dopusRt,
                ["/cmd", "Go", path, "NEWTAB=no"],
                _commandTimeout);

            if (result.ExecutableMissing || result.TimedOut || result.ExitCode != 0)
                return HostNavigation.Failure(HostFailureReason.ActionFailed);

            return HostNavigation.Success;
        }
        catch
        {
            return HostNavigation.Failure(HostFailureReason.ActionFailed);
        }
    }

    /// <summary>
    /// 一条 <c>dopusrt /info &lt;file&gt;,paths</c> 记录。<see cref="Lister"/> 是官方输出里
    /// <c>lister="0xHHHHHH"</c> 解析出的窗口句柄，可直接与捕获的前台 HWND 比对，
    /// 不需要靠窗口标题猜测。
    /// </summary>
    /// <param name="Side">面板号：1=左/上，2=右/下。单面板 Lister 只有 side 1。</param>
    /// <param name="ActiveTab">
    /// <c>active_tab</c> 属性值，缺失为 0。只出现在**每个 side 各自的活动标签**上，
    /// 值等于该 side 号，所以它只能筛出「每侧的活动标签」，<b>不能</b>判断哪一侧有焦点。
    /// </param>
    /// <param name="TabState">
    /// <c>tab_state</c> 属性值，缺失为 0。双面板时用来区分 source(1) / destination(2)，
    /// 即用户当前操作的是哪一侧。<see cref="SourceTabState"/>。
    /// </param>
    internal readonly record struct OpusListerPath(
        IntPtr Lister,
        bool IsActiveLister,
        int Side,
        int ActiveTab,
        int TabState,
        string Path)
    {
        /// <summary>该记录是所在 side 的活动标签。</summary>
        public bool IsActiveTab => ActiveTab > 0;
    }

    /// <summary>
    /// Opus 双面板里 source（用户当前操作侧）的 <c>tab_state</c> 值；destination 为 2。
    /// 单面板 Lister 的活动标签同样是 source。
    /// </summary>
    internal const int SourceTabState = 1;

    /// <summary>
    /// 解析 dopusrt <c>/info</c> 的 XML 输出，例如：
    /// <code>&lt;path active_lister="1" active_tab="1" lister="0x1f087e" side="1" tab="0x1508c0"&gt;C:\Windows&lt;/path&gt;</code>
    /// 双面板时 <c>active_tab</c> 值（1 或 2）等于 <c>side</c> 值时该面板活动。
    /// 忽略无法解析、非盘符或空的路径；XML 本身损坏时返回空列表，由调用方降级。
    /// </summary>
    internal static IReadOnlyList<OpusListerPath> ReadInfoPaths(string file)
    {
        string content;
        try
        {
            content = File.ReadAllText(file, Encoding.UTF8);
        }
        catch
        {
            return Array.Empty<OpusListerPath>();
        }

        XElement root;
        try
        {
            root = ParseInfoDocument(content);
        }
        catch (XmlException)
        {
            return Array.Empty<OpusListerPath>();
        }

        var entries = new List<OpusListerPath>();
        foreach (var element in root.Descendants()
                     .Where(e => e.Name.LocalName.Equals("path", StringComparison.OrdinalIgnoreCase)))
        {
            var path = NormalizeOpusPath(element.Value);
            if (path is null) continue;
            entries.Add(new OpusListerPath(
                ParseHandleAttribute(element.Attribute("lister")?.Value),
                IsFlagSet(element.Attribute("active_lister")?.Value),
                ParseIntAttribute(element.Attribute("side")?.Value),
                ParseIntAttribute(element.Attribute("active_tab")?.Value),
                ParseIntAttribute(element.Attribute("tab_state")?.Value),
                path));
        }
        return entries;
    }

    /// <summary>
    /// dopusrt 可能只输出裸的 <c>&lt;path&gt;</c> 序列，也可能带自己的根元素和 XML 声明。
    /// 统一剥掉声明后包一层合成根，两种形态都能解析。
    /// </summary>
    private static XElement ParseInfoDocument(string content)
    {
        var body = content.Trim();
        while (body.StartsWith("<?", StringComparison.Ordinal))
        {
            var end = body.IndexOf("?>", StringComparison.Ordinal);
            if (end < 0) break;
            body = body[(end + 2)..].TrimStart();
        }
        return XElement.Parse("<prism-info>" + body + "</prism-info>", LoadOptions.PreserveWhitespace);
    }

    /// <summary>把 <c>0xHHHHHH</c> 句柄属性解析成 HWND；缺失或非法时返回 <see cref="IntPtr.Zero"/>。</summary>
    private static IntPtr ParseHandleAttribute(string? value)
    {
        var text = value?.Trim();
        if (string.IsNullOrEmpty(text)) return IntPtr.Zero;
        var isHex = text.StartsWith("0x", StringComparison.OrdinalIgnoreCase);
        if (isHex) text = text[2..];
        if (text.Length == 0) return IntPtr.Zero;
        var style = isHex ? NumberStyles.HexNumber : NumberStyles.Integer;
        return long.TryParse(text, style, CultureInfo.InvariantCulture, out var handle) && handle != 0
            ? new IntPtr(handle)
            : IntPtr.Zero;
    }

    private static bool IsFlagSet(string? value) =>
        value?.Trim() is "1" or "true" or "yes";

    /// <summary>把 <c>side</c> / <c>active_tab</c> / <c>tab_state</c> 这类小整数属性解析出来；缺失或非法为 0。</summary>
    private static int ParseIntAttribute(string? value)
    {
        var text = value?.Trim();
        return !string.IsNullOrEmpty(text)
            && int.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out var result)
            ? result
            : 0;
    }

    /// <summary>盘符绝对路径以外一律丢弃（`shell:`、库、FTP 站点等不是可索引 root）。</summary>
    private static string? NormalizeOpusPath(string raw)
    {
        var value = raw.Trim().Trim('"').Replace('/', '\\');
        if (value.Length < 2 || !char.IsAsciiLetter(value[0]) || value[1] != ':')
            return null;
        var trimmed = value.TrimEnd('\\');
        if (trimmed.Length == 2 && trimmed[1] == ':')
            trimmed += "\\";
        return trimmed;
    }

    /// <summary>
    /// 供测试与文档说明：把参数列表格式化成可读形式（不用于真实启动）。
    /// </summary>
    internal static string FormatArgsForDiagnostics(IReadOnlyList<string> args) =>
        string.Join(' ', args.Select(QuoteForDiagnostics));

    private static string QuoteForDiagnostics(string value) =>
        value.Length == 0 || value.Any(char.IsWhiteSpace) || value.Contains('"')
            ? "\"" + value.Replace("\"", "\\\"", StringComparison.Ordinal) + "\""
            : value;
}

/// <summary>用 ProcessStartInfo.ArgumentList 调用 dopusrt，避免 shell 转义漏洞。</summary>
public sealed class ProcessDirectoryOpusRuntime : IDirectoryOpusRuntime
{
    public string? ResolveDopusRtPath(string? dopusProcessPath)
    {
        if (!string.IsNullOrWhiteSpace(dopusProcessPath))
        {
            var dir = Path.GetDirectoryName(dopusProcessPath);
            if (!string.IsNullOrEmpty(dir))
            {
                var sibling = Path.Combine(dir, "dopusrt.exe");
                if (File.Exists(sibling)) return sibling;
            }
        }

        foreach (var root in new[]
                 {
                     Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles),
                     Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86),
                 })
        {
            if (string.IsNullOrEmpty(root)) continue;
            var candidate = Path.Combine(root, "GPSoftware", "Directory Opus", "dopusrt.exe");
            if (File.Exists(candidate)) return candidate;
        }

        return null;
    }

    public DirectoryOpusCommandResult Run(
        string dopusRtPath,
        IReadOnlyList<string> arguments,
        TimeSpan timeout)
    {
        if (string.IsNullOrWhiteSpace(dopusRtPath) || !File.Exists(dopusRtPath))
            return DirectoryOpusCommandResult.Missing;

        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = dopusRtPath,
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            foreach (var arg in arguments)
                psi.ArgumentList.Add(arg);

            using var process = Process.Start(psi);
            if (process is null)
                return new DirectoryOpusCommandResult(false, -1, "", "failed to start");

            var stdoutTask = process.StandardOutput.ReadToEndAsync();
            var stderrTask = process.StandardError.ReadToEndAsync();
            if (!process.WaitForExit((int)Math.Clamp(timeout.TotalMilliseconds, 1, 60_000)))
            {
                // L17（全仓复审 2026-08-22）：不杀进程树——dopusrt 不派生用户进程，
                // entireProcessTree:true 是全仓唯一一处杀树，与 PipeClient 的 Bug 3
                // 政策（broker 打开的用户应用不能陪葬）相悖；被杀进程的管道读取
                // 任务也要观察掉，不能弃管成 unobserved exception。
                try { process.Kill(); } catch { /* ignore */ }
                try
                {
                    stdoutTask.Wait(TimeSpan.FromSeconds(1));
                    stderrTask.Wait(TimeSpan.FromSeconds(1));
                }
                catch { /* ignore */ }
                return new DirectoryOpusCommandResult(true, -1, "", "timed out");
            }

            // Ensure readers finish after exit.
            stdoutTask.Wait(TimeSpan.FromSeconds(1));
            stderrTask.Wait(TimeSpan.FromSeconds(1));
            return new DirectoryOpusCommandResult(
                false,
                process.ExitCode,
                stdoutTask.IsCompletedSuccessfully ? stdoutTask.Result : "",
                stderrTask.IsCompletedSuccessfully ? stderrTask.Result : "");
        }
        catch (Exception ex)
        {
            return new DirectoryOpusCommandResult(false, -1, "", ex.GetType().Name);
        }
    }
}
