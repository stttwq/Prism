using Prism.Models;
using Prism.Services;

namespace Prism.ViewModels;

/// <summary>
/// 当前目录 / 全局 范围状态机（G4 步骤 3）。不依赖 WPF，便于单测。
///
/// 规则（design.md Host Context State + prd Requirements）：
/// 每次呼出重新捕获前台 HWND 与 HostContext，**先无条件清空旧 root**；识别失败、宿主关闭、
/// 路径无效或不在索引中时保持 root 为空并留在全局，同时给出可见提示；`Ctrl+G` 只有在
/// 当前 root 仍然有效（宿主窗口存活且路径复验通过）时才能切回当前目录。
/// </summary>
public sealed class HostScopeController
{
    private readonly IReadOnlyList<IHostAdapter> _adapters;
    private readonly IRootValidator _validator;
    private readonly IHostWindowProbe _probe;

    public HostScopeController(
        IEnumerable<IHostAdapter>? adapters = null,
        IRootValidator? validator = null,
        IHostWindowProbe? probe = null)
    {
        _adapters = adapters?.ToArray() ?? DisabledHostAdapter.SupportMatrix;
        _validator = validator ?? new FileSystemRootValidator();
        _probe = probe ?? new Win32HostWindowProbe();
    }

    /// <summary>总开关，默认开启；关闭后不再尝试识别宿主。</summary>
    public bool CurrentDirectoryEnabled { get; private set; } = true;

    public HostContext Host { get; private set; } = HostContext.None;

    public SearchScope Scope { get; private set; } = SearchScope.Global;

    /// <summary>需要显示给用户的降级提示；空串表示无提示。</summary>
    public string Notice { get; private set; } = "";

    /// <summary>状态变化（供窗口刷新范围标签与搜索上下文）。</summary>
    public event Action? Changed;

    /// <summary>当前生效的 root；全局范围下为 null。</summary>
    public string? Root => Scope == SearchScope.CurrentDirectory ? Host.Root : null;

    /// <summary>只有存在可用 root 时才显示范围标签（点击即可切换）。</summary>
    public bool IsScopeLabelVisible => Host.HasUsableRoot;

    public string ScopeLabel => Scope == SearchScope.CurrentDirectory
        ? "当前目录：" + LeafName(Host.Root)
        : "全局";

    public string ScopeTooltip => Scope == SearchScope.CurrentDirectory
        ? $"仅搜索 {Host.Root} 及其子目录（Ctrl+G 切换到全局）"
        : Host.HasUsableRoot
            ? $"搜索全部卷（Ctrl+G 切回 {Host.Root}）"
            : "搜索全部卷";

    /// <summary>把当前范围写进搜索上下文；全局时 root 为 null。</summary>
    public SearchContext Apply(SearchContext context) => context with { Root = Root };

    /// <summary>设置页总开关。关闭时立即清空宿主上下文并回到全局。</summary>
    public void SetCurrentDirectoryEnabled(bool enabled)
    {
        if (CurrentDirectoryEnabled == enabled) return;
        CurrentDirectoryEnabled = enabled;
        if (!enabled)
        {
            Host = HostContext.Cleared(HostDetectionStatus.FeatureDisabled);
            Scope = SearchScope.Global;
            // 用户自己关掉的功能不需要降级提示。
            Notice = "";
        }
        Changed?.Invoke();
    }

    /// <summary>
    /// 每次呼出调用，传入呼出前保存的前台 HWND。返回本次的宿主上下文；
    /// 任何失败都返回无 root 的上下文，绝不沿用上一次目录。
    /// </summary>
    public HostContext Capture(IntPtr foregroundWindow)
    {
        Host = HostContext.None;
        Scope = SearchScope.Global;
        Notice = "";

        if (!CurrentDirectoryEnabled)
            return Settle(HostContext.Cleared(HostDetectionStatus.FeatureDisabled));
        if (foregroundWindow == IntPtr.Zero)
            return Settle(HostContext.Cleared(HostDetectionStatus.NoSupportedHost));
        if (!_probe.IsAlive(foregroundWindow))
            return Settle(HostContext.Cleared(HostDetectionStatus.HostGone));

        foreach (var adapter in _adapters)
        {
            if (!adapter.IsEnabled) continue;

            HostDetection detection;
            try
            {
                detection = adapter.Detect(foregroundWindow);
            }
            catch
            {
                return Settle(HostContext.Cleared(HostDetectionStatus.DetectFailed));
            }

            if (!detection.IsHost)
            {
                if (detection.Reason is HostFailureReason.NotThisHost
                    or HostFailureReason.AdapterDisabled)
                    continue;
                return Settle(HostContext.Cleared(StatusFor(detection.Reason)));
            }

            if (!detection.Capabilities.HasFlag(HostCapability.ReadFolder))
                return Settle(HostContext.Cleared(HostDetectionStatus.FolderUnavailable));

            HostFolder folder;
            try
            {
                folder = adapter.GetFolder(foregroundWindow);
            }
            catch
            {
                return Settle(HostContext.Cleared(HostDetectionStatus.FolderUnavailable));
            }

            if (!folder.IsSuccess)
            {
                var reason = folder.Reason == HostFailureReason.None
                    ? HostFailureReason.FolderUnavailable
                    : folder.Reason;
                return Settle(HostContext.Cleared(StatusFor(reason)));
            }

            var rejection = _validator.Validate(folder.Path, out var normalized);
            if (rejection is not null)
                return Settle(HostContext.Cleared(StatusFor(rejection.Value)));

            Host = new HostContext(
                detection.Kind,
                foregroundWindow,
                normalized,
                HostDetectionStatus.Detected);
            Scope = SearchScope.CurrentDirectory;
            Notice = "";
            Changed?.Invoke();
            return Host;
        }

        // 前台不是受支持的宿主：正常全局搜索，不打扰用户。
        return Settle(HostContext.Cleared(HostDetectionStatus.NoSupportedHost));
    }

    /// <summary>范围标签点击 / `Ctrl+G`。返回是否发生了切换。</summary>
    public bool ToggleScope()
    {
        if (Scope == SearchScope.CurrentDirectory)
        {
            Scope = SearchScope.Global;
            Notice = "";
            Changed?.Invoke();
            return true;
        }

        if (!CurrentDirectoryEnabled)
        {
            Notice = "当前目录搜索已在设置中关闭";
            Changed?.Invoke();
            return false;
        }
        if (!Host.HasUsableRoot)
        {
            Notice = "没有可用的当前目录，保持全局搜索";
            Changed?.Invoke();
            return false;
        }
        if (!Revalidate())
            return false;

        Scope = SearchScope.CurrentDirectory;
        Notice = "";
        Changed?.Invoke();
        return true;
    }

    /// <summary>
    /// 复验当前 root：宿主窗口存活且路径仍然有效。失败时清空上下文并回到全局。
    /// </summary>
    public bool Revalidate()
    {
        if (!Host.HasUsableRoot)
        {
            Invalidate(Host.Status == HostDetectionStatus.Detected
                ? HostDetectionStatus.HostGone
                : Host.Status);
            return false;
        }
        if (!_probe.IsAlive(Host.CapturedWindow))
        {
            Invalidate(HostDetectionStatus.HostGone);
            return false;
        }
        var rejection = _validator.Validate(Host.Root, out _);
        if (rejection is not null)
        {
            Invalidate(StatusFor(rejection.Value));
            return false;
        }
        return true;
    }

    /// <summary>
    /// 外部（adapter 联动失败、后端 root_unavailable）报告 root 不可用：
    /// 清空 root 并回到全局，同时提示已回到全局搜索。
    /// </summary>
    public void Invalidate(HostDetectionStatus status)
    {
        Host = HostContext.Cleared(status);
        Scope = SearchScope.Global;
        Notice = NoticeFor(status);
        Changed?.Invoke();
    }

    public void Invalidate(RootRejection rejection) => Invalidate(StatusFor(rejection));

    public void Invalidate(HostFailureReason reason) => Invalidate(StatusFor(reason));

    /// <summary>
    /// Ctrl+Enter：在仍有效的原宿主中定位/导航选中结果。
    /// 即使当前 Scope 被用户切到全局，只要 Host 上下文仍在就尝试交回原宿主
    /// （语义是「原宿主定位」，不是「仅当前目录范围」）。
    /// 无可用宿主、开关关闭或能力不足时返回 <see cref="HostRevealResult.Unavailable"/>，
    /// 由上层 fallback 到 broker reveal；定位失败默认<strong>不清空</strong> root。
    /// </summary>
    public HostRevealResult TryRevealInHost(string path, bool isDirectory)
    {
        if (string.IsNullOrWhiteSpace(path))
            return HostRevealResult.Unavailable;
        if (!Host.HasUsableRoot || Host.CapturedWindow == IntPtr.Zero)
            return HostRevealResult.Unavailable;

        var window = Host.CapturedWindow;
        if (!_probe.IsAlive(window))
        {
            Invalidate(HostDetectionStatus.HostGone);
            return HostRevealResult.Failure(HostFailureReason.HostGone);
        }

        var adapter = FindEnabledAdapter(Host.Kind);
        if (adapter is null)
            return HostRevealResult.Unavailable;

        HostDetection detection;
        try
        {
            detection = adapter.Detect(window);
        }
        catch
        {
            return HostRevealResult.Failure(HostFailureReason.ActionFailed);
        }

        if (!detection.IsHost)
        {
            if (ShouldInvalidateHost(detection.Reason))
            {
                Invalidate(detection.Reason);
                return HostRevealResult.Failure(detection.Reason);
            }
            return HostRevealResult.Unavailable;
        }

        var intent = isDirectory
            ? HostNavigationIntent.NavigateFolder
            : HostNavigationIntent.RevealInHost;
        var required = isDirectory
            ? HostCapability.NavigateFolder
            : HostCapability.RevealInHost;
        if (!detection.Capabilities.HasFlag(required))
            return HostRevealResult.Unavailable;

        HostNavigation navigation;
        try
        {
            navigation = adapter.NavigateOrFill(
                window,
                new HostNavigationRequest(path.Trim(), isDirectory, intent));
        }
        catch
        {
            return HostRevealResult.Failure(HostFailureReason.ActionFailed);
        }

        if (navigation.Succeeded)
            return HostRevealResult.Success;

        var reason = navigation.Reason == HostFailureReason.None
            ? HostFailureReason.ActionFailed
            : navigation.Reason;
        if (ShouldInvalidateHost(reason))
            Invalidate(reason);
        return HostRevealResult.Failure(reason);
    }

    private IHostAdapter? FindEnabledAdapter(HostKind kind)
    {
        if (kind == HostKind.None) return null;
        foreach (var adapter in _adapters)
        {
            if (adapter.Kind == kind && adapter.IsEnabled)
                return adapter;
        }
        return null;
    }

    /// <summary>
    /// 定位失败 ≠ root 失效：只有宿主消失或提权才清空上下文。
    /// </summary>
    private static bool ShouldInvalidateHost(HostFailureReason reason) =>
        reason is HostFailureReason.HostGone or HostFailureReason.HostElevated;

    private HostContext Settle(HostContext cleared)
    {
        Host = cleared;
        Scope = SearchScope.Global;
        Notice = NoticeFor(cleared.Status);
        Changed?.Invoke();
        return Host;
    }

    private static HostDetectionStatus StatusFor(HostFailureReason reason) => reason switch
    {
        HostFailureReason.AdapterDisabled => HostDetectionStatus.AdapterDisabled,
        HostFailureReason.NotThisHost => HostDetectionStatus.NoSupportedHost,
        HostFailureReason.HostGone => HostDetectionStatus.HostGone,
        HostFailureReason.HostElevated => HostDetectionStatus.NoSupportedHost,
        HostFailureReason.AccessDenied => HostDetectionStatus.AccessDenied,
        HostFailureReason.FolderUnavailable => HostDetectionStatus.FolderUnavailable,
        _ => HostDetectionStatus.DetectFailed,
    };

    private static HostDetectionStatus StatusFor(RootRejection rejection) => rejection switch
    {
        RootRejection.AccessDenied => HostDetectionStatus.AccessDenied,
        RootRejection.VolumeNotIndexed => HostDetectionStatus.RootNotIndexed,
        _ => HostDetectionStatus.RootInvalid,
    };

    private static string NoticeFor(HostDetectionStatus status) => status switch
    {
        HostDetectionStatus.Detected => "",
        HostDetectionStatus.NotAttempted => "",
        HostDetectionStatus.FeatureDisabled => "",
        HostDetectionStatus.NoSupportedHost => "",
        HostDetectionStatus.AdapterDisabled => "",
        HostDetectionStatus.FolderUnavailable => "未能读取当前目录，已回到全局搜索",
        HostDetectionStatus.AccessDenied => "当前目录不可访问，已回到全局搜索",
        HostDetectionStatus.RootInvalid => "当前目录路径不受支持，已回到全局搜索",
        HostDetectionStatus.RootNotIndexed => "当前目录不在索引中，已回到全局搜索",
        HostDetectionStatus.HostGone => "原窗口已关闭，已回到全局搜索",
        _ => "未能识别当前目录，已回到全局搜索",
    };

    private static string LeafName(string? root)
    {
        if (string.IsNullOrEmpty(root)) return "";
        var trimmed = root.TrimEnd('\\');
        if (trimmed.Length == 2 && trimmed[1] == ':') return trimmed + "\\";
        var index = trimmed.LastIndexOf('\\');
        return index >= 0 && index + 1 < trimmed.Length ? trimmed[(index + 1)..] : trimmed;
    }
}
