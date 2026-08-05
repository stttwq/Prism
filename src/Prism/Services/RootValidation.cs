using System.IO;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// root 被拒绝的原因。名称与 prism-core `root_scope::RootRejection` 一一对应，
/// 便于后续把后端 `root_unavailable` 响应直接映射进来。
/// </summary>
public enum RootRejection
{
    NotAbsolute,
    Unsupported,
    TooLong,
    TooDeep,
    VolumeNotIndexed,
    NotFound,
    NotADirectory,
    AccessDenied,
}

/// <summary>
/// `RootRejection` 与 prism-core `root_scope::RootRejection` 的稳定线格式互转。
/// 后端只发这些字符串；未知值一律当作「无法解释的降级」返回 null，由调用方走通用提示。
/// </summary>
public static class RootRejectionCodes
{
    public static string ToCode(RootRejection rejection) => rejection switch
    {
        RootRejection.NotAbsolute => "not_absolute",
        RootRejection.Unsupported => "unsupported",
        RootRejection.TooLong => "too_long",
        RootRejection.TooDeep => "too_deep",
        RootRejection.VolumeNotIndexed => "volume_not_indexed",
        RootRejection.NotFound => "not_found",
        RootRejection.NotADirectory => "not_a_directory",
        RootRejection.AccessDenied => "access_denied",
        _ => "",
    };

    public static RootRejection? Parse(string? code) => code switch
    {
        "not_absolute" => RootRejection.NotAbsolute,
        "unsupported" => RootRejection.Unsupported,
        "too_long" => RootRejection.TooLong,
        "too_deep" => RootRejection.TooDeep,
        "volume_not_indexed" => RootRejection.VolumeNotIndexed,
        "not_found" => RootRejection.NotFound,
        "not_a_directory" => RootRejection.NotADirectory,
        "access_denied" => RootRejection.AccessDenied,
        _ => null,
    };
}

/// <summary>校验并规范化宿主给出的当前目录。</summary>
public interface IRootValidator
{
    /// <summary>成功时返回 null 并给出规范化路径；失败时返回原因。</summary>
    RootRejection? Validate(string? path, out string normalized);
}

/// <summary>
/// 本地校验：绝对盘符路径、长度与深度上限、目录存在且可访问。
/// 「卷/目录是否在索引中」只有 indexer 知道，本地不猜 —— 该情况由后端
/// `root_unavailable` 回报后再走 <see cref="HostDetectionStatus.RootNotIndexed"/> 降级。
/// </summary>
public sealed class FileSystemRootValidator : IRootValidator
{
    /// <summary>与 prism-core `MAX_ROOT_PATH_BYTES` 保持一致。</summary>
    public const int MaxRootPathLength = 32_767;

    /// <summary>与 prism-core `MAX_ROOT_DEPTH`（父链深度上限）保持一致。</summary>
    public const int MaxRootDepth = 64;

    public RootRejection? Validate(string? path, out string normalized)
    {
        normalized = "";
        var trimmed = path?.Trim();
        if (string.IsNullOrEmpty(trimmed))
            return RootRejection.NotAbsolute;
        if (trimmed.Length > MaxRootPathLength)
            return RootRejection.TooLong;
        if (trimmed.Any(char.IsControl))
            return RootRejection.Unsupported;

        var unified = trimmed.Replace('/', '\\');
        if (unified.StartsWith(@"\\", StringComparison.Ordinal))
        {
            // UNC 共享、`\\?\UNC\` 与设备路径都不在索引里。
            var extended = unified.StartsWith(@"\\?\", StringComparison.Ordinal)
                ? unified[4..]
                : null;
            if (extended is null || extended.StartsWith("UNC\\", StringComparison.OrdinalIgnoreCase))
                return RootRejection.Unsupported;
            unified = extended;
        }

        if (unified.Length < 2 || !char.IsAsciiLetter(unified[0]) || unified[1] != ':')
            return RootRejection.NotAbsolute;
        var rest = unified[2..];
        if (rest.Length > 0 && rest[0] != '\\')
            return RootRejection.NotAbsolute; // `C:docs` 是盘符相对路径。

        var parts = rest.Split('\\', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Any(part => part is "." or ".."))
            return RootRejection.Unsupported;
        if (parts.Length > MaxRootDepth)
            return RootRejection.TooDeep;

        var candidate = char.ToUpperInvariant(unified[0]) + ":\\" + string.Join('\\', parts);
        try
        {
            if (File.Exists(candidate))
                return RootRejection.NotADirectory;
            if (!Directory.Exists(candidate))
                return RootRejection.NotFound;
            // 触一次目录枚举，权限问题在这里暴露，而不是留到搜索时。
            using var probe = Directory.EnumerateFileSystemEntries(candidate).GetEnumerator();
            probe.MoveNext();
        }
        catch (UnauthorizedAccessException)
        {
            return RootRejection.AccessDenied;
        }
        catch (IOException)
        {
            return RootRejection.NotFound;
        }

        normalized = candidate;
        return null;
    }
}

/// <summary>捕获的宿主窗口是否还活着。</summary>
public interface IHostWindowProbe
{
    bool IsAlive(IntPtr window);
}

/// <summary>user32 `IsWindow`。只读探测，不做任何窗口控制。</summary>
public sealed class Win32HostWindowProbe : IHostWindowProbe
{
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool IsWindow(IntPtr hWnd);

    public bool IsAlive(IntPtr window)
    {
        if (window == IntPtr.Zero) return false;
        try
        {
            return IsWindow(window);
        }
        catch
        {
            return false;
        }
    }
}
