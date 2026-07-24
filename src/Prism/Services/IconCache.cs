using System.Collections.Concurrent;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace Prism.Services;

/// <summary>
/// 按路径/扩展名取系统文件图标并缓存。
/// - .lnk / .exe / 目录：按完整路径缓存（图标因快捷方式/可执行文件而异）
/// - 其它文件：按扩展名缓存（大量同类型文件共享一张图，显著省内存）
/// - 容量上限 + 可主动清空，隐藏搜索窗时释放
/// </summary>
public sealed class IconCache
{
    /// <summary>最多缓存项。扩展名合并后 128 足够覆盖常见类型 + 一批 lnk/exe。</summary>
    public const int MaxEntries = 128;

    private readonly ConcurrentDictionary<string, ImageSource?> _cache = new(StringComparer.OrdinalIgnoreCase);
    private readonly ConcurrentQueue<string> _order = new();

    /// <summary>异步获取 32×32 系统图标。</summary>
    public Task<ImageSource?> GetAsync(string path, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(path))
            return Task.FromResult<ImageSource?>(null);

        var key = CacheKey(path);
        if (_cache.TryGetValue(key, out var hit))
            return Task.FromResult(hit);

        return Task.Run(() =>
        {
            ct.ThrowIfCancellationRequested();
            var icon = LoadIcon(path);
            if (_cache.TryAdd(key, icon))
            {
                _order.Enqueue(key);
                TrimIfNeeded();
            }
            return icon;
        }, ct);
    }

    /// <summary>清空缓存（搜索窗隐藏后调用，把位图交还 GC）。</summary>
    public void Clear()
    {
        _cache.Clear();
        while (_order.TryDequeue(out _)) { }
    }

    /// <summary>当前缓存条数（诊断用）。</summary>
    public int Count => _cache.Count;

    private void TrimIfNeeded()
    {
        while (_cache.Count > MaxEntries && _order.TryDequeue(out var old))
            _cache.TryRemove(old, out _);
    }

    /// <summary>
    /// 缓存键：可执行/快捷方式/目录用完整路径；其余用扩展名键，避免每文件一图。
    /// </summary>
    internal static string CacheKey(string path)
    {
        // URL
        if (path.StartsWith("http://", StringComparison.OrdinalIgnoreCase)
            || path.StartsWith("https://", StringComparison.OrdinalIgnoreCase))
            return path;

        try
        {
            // 先看路径形态，避免对每个普通文件做 Directory.Exists（UI 装饰路径上会卡）。
            if (path.EndsWith('\\') || path.EndsWith('/'))
                return "dir:";

            var ext = Path.GetExtension(path);
            if (ext.Equals(".lnk", StringComparison.OrdinalIgnoreCase)
                || ext.Equals(".exe", StringComparison.OrdinalIgnoreCase)
                || ext.Equals(".msc", StringComparison.OrdinalIgnoreCase)
                || ext.Equals(".bat", StringComparison.OrdinalIgnoreCase)
                || ext.Equals(".cmd", StringComparison.OrdinalIgnoreCase)
                || ext.Equals(".com", StringComparison.OrdinalIgnoreCase))
            {
                // 快捷方式与 exe 图标各不相同
                return path;
            }

            if (!string.IsNullOrEmpty(ext))
                return "ext:" + ext.ToLowerInvariant();

            // 无扩展名：可能是目录或无后缀文件；仅此时探测文件系统。
            if (Directory.Exists(path))
                return "dir:";

            return "file:";
        }
        catch
        {
            return path;
        }
    }

    private static ImageSource? LoadIcon(string path)
    {
        try
        {
            var isDir = Directory.Exists(path);
            var exists = isDir || File.Exists(path);
            // SMALLICON 在部分 DPI 下偏糊；LARGEICON=32 与列表一致。
            var flags = SHGFI_ICON | SHGFI_LARGEICON;
            uint attrs = 0;
            if (!exists)
            {
                flags |= SHGFI_USEFILEATTRIBUTES;
                attrs = (path.EndsWith('\\') || path.EndsWith('/'))
                    ? FILE_ATTRIBUTE_DIRECTORY
                    : FILE_ATTRIBUTE_NORMAL;
            }
            else if (isDir)
            {
                attrs = FILE_ATTRIBUTE_DIRECTORY;
            }

            // 扩展名缓存命中路径时：若源文件不存在，用扩展名属性图标即可
            var shfi = new SHFILEINFO();
            var hr = SHGetFileInfo(path, attrs, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
            if (hr == IntPtr.Zero || shfi.hIcon == IntPtr.Zero)
            {
                // 回退：纯扩展名
                if (!exists)
                    return null;
                flags |= SHGFI_USEFILEATTRIBUTES;
                attrs = FILE_ATTRIBUTE_NORMAL;
                shfi = new SHFILEINFO();
                hr = SHGetFileInfo(path, attrs, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
                if (hr == IntPtr.Zero || shfi.hIcon == IntPtr.Zero)
                    return null;
            }

            try
            {
                var source = Imaging.CreateBitmapSourceFromHIcon(
                    shfi.hIcon,
                    Int32Rect.Empty,
                    BitmapSizeOptions.FromWidthAndHeight(32, 32));
                source.Freeze();
                return source;
            }
            finally
            {
                DestroyIcon(shfi.hIcon);
            }
        }
        catch
        {
            return null;
        }
    }

    private const uint SHGFI_ICON = 0x000000100;
    private const uint SHGFI_LARGEICON = 0x000000000;
    private const uint SHGFI_USEFILEATTRIBUTES = 0x000000010;
    private const uint FILE_ATTRIBUTE_NORMAL = 0x80;
    private const uint FILE_ATTRIBUTE_DIRECTORY = 0x10;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct SHFILEINFO
    {
        public IntPtr hIcon;
        public int iIcon;
        public uint dwAttributes;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)] public string szTypeName;
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SHGetFileInfo(
        string pszPath, uint dwFileAttributes, ref SHFILEINFO psfi, uint cbFileInfo, uint uFlags);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool DestroyIcon(IntPtr hIcon);
}
