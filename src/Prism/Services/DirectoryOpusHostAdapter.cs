using System.Diagnostics;
using System.IO;
using System.Text;
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

                var paths = ReadInfoPaths(tempFile);
                if (paths.Count == 0)
                    return HostFolder.Failure(HostFailureReason.FolderUnavailable);

                if (paths.Count == 1)
                    return HostFolder.Success(paths[0]);

                // 多 Lister：尝试用窗口标题包含关系消歧；消歧失败宁可不猜。
                var title = _windows.GetWindowTitle(hostWindow) ?? "";
                var matched = paths
                    .Where(p => !string.IsNullOrEmpty(title)
                        && title.Contains(p, StringComparison.OrdinalIgnoreCase))
                    .Distinct(StringComparer.OrdinalIgnoreCase)
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
    /// 解析 dopusrt /info paths 输出。每行一个路径；忽略空行与非绝对盘符路径。
    /// </summary>
    internal static IReadOnlyList<string> ReadInfoPaths(string file)
    {
        var lines = File.ReadAllLines(file, Encoding.UTF8);
        var paths = new List<string>();
        foreach (var raw in lines)
        {
            var line = raw.Trim().Trim('"');
            if (line.Length == 0) continue;
            // 有些版本输出 tab 分隔的附加字段，只取第一列。
            var tab = line.IndexOf('\t');
            if (tab > 0) line = line[..tab].Trim().Trim('"');
            line = line.Replace('/', '\\');
            if (line.Length < 2 || !char.IsAsciiLetter(line[0]) || line[1] != ':')
                continue;
            var trimmed = line.TrimEnd('\\');
            if (trimmed.Length == 2 && trimmed[1] == ':')
                trimmed += "\\";
            paths.Add(trimmed);
        }
        return paths;
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
                try { process.Kill(entireProcessTree: true); } catch { /* ignore */ }
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
