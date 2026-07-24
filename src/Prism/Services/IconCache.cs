using System.Collections.Concurrent;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace Prism.Services;

/// <summary>
/// 按路径取系统文件图标并缓存（frontend-spec.md §IconCache）。
/// 同步取图在线程池执行；失败返回 null。
/// </summary>
public sealed class IconCache
{
    private readonly ConcurrentDictionary<string, ImageSource?> _cache = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>异步获取 32×32 系统图标；同一路径只解析一次。</summary>
    public Task<ImageSource?> GetAsync(string path, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(path))
            return Task.FromResult<ImageSource?>(null);

        if (_cache.TryGetValue(path, out var hit))
            return Task.FromResult(hit);

        return Task.Run(() =>
        {
            ct.ThrowIfCancellationRequested();
            var icon = LoadIcon(path);
            _cache.TryAdd(path, icon);
            return icon;
        }, ct);
    }

    private static ImageSource? LoadIcon(string path)
    {
        try
        {
            // 真实存在的路径（含 .lnk）不走 USEFILEATTRIBUTES，让 shell 解析快捷方式图标。
            // 不存在的路径才用扩展名/目录属性的默认图标，避免列表空白。
            var isDir = Directory.Exists(path);
            var exists = isDir || File.Exists(path);
            var flags = SHGFI_ICON | SHGFI_LARGEICON;
            uint attrs = 0;
            if (!exists)
            {
                flags |= SHGFI_USEFILEATTRIBUTES;
                // 路径不存在时 Directory.Exists 恒 false，用尾部分隔符粗判目录。
                attrs = (path.EndsWith('\\') || path.EndsWith('/'))
                    ? FILE_ATTRIBUTE_DIRECTORY
                    : FILE_ATTRIBUTE_NORMAL;
            }
            else if (isDir)
            {
                attrs = FILE_ATTRIBUTE_DIRECTORY;
            }

            var shfi = new SHFILEINFO();
            var hr = SHGetFileInfo(path, attrs, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
            if (hr == IntPtr.Zero || shfi.hIcon == IntPtr.Zero)
                return null;

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
