using System.Collections.Concurrent;
using System.Collections.Generic;
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

    /// <summary>异步获取系统文件图标；pixelSize 按目标物理像素请求（高 DPI 下取 48/256px 源，避免拉伸发虚）。</summary>
    public Task<ImageSource?> GetAsync(string path, int pixelSize = 32, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(path))
            return Task.FromResult<ImageSource?>(null);

        var key = SizedKey(path, pixelSize);
        if (_cache.TryGetValue(key, out var hit))
            return Task.FromResult(hit);

        return Task.Run(() =>
        {
            ct.ThrowIfCancellationRequested();
            var icon = LoadIcon(path, pixelSize);
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

    /// <summary>
    /// 只清路径类键（完整路径、URL），保留 ext:/dir: 扩展名键。
    /// 扩展名图标恒定且由 LRU 保护，保留后下次呼出扩展名图标立即可见，
    /// 路径类图标按需重新加载。
    /// </summary>
    public void ClearPathKeys()
    {
        var keysToRemove = new List<string>();
        foreach (var key in _cache.Keys)
        {
            // SizedKey = CacheKey + "@" + pixelSize；拆出 CacheKey 部分判断。
            var atIdx = key.LastIndexOf('@');
            var cacheKey = atIdx > 0 ? key.Substring(0, atIdx) : key;
            // 保留 ext: 和 dir: 前缀的键（扩展名/目录类型图标恒定）。
            if (cacheKey.StartsWith("ext:", StringComparison.OrdinalIgnoreCase)
                || cacheKey.StartsWith("dir:", StringComparison.OrdinalIgnoreCase))
                continue;
            keysToRemove.Add(key);
        }
        foreach (var key in keysToRemove)
            _cache.TryRemove(key, out _);
        // 重建队列：只保留仍在缓存中的键。
        var remaining = new List<string>();
        while (_order.TryDequeue(out var k))
            if (_cache.ContainsKey(k))
                remaining.Add(k);
        foreach (var k in remaining)
            _order.Enqueue(k);
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
    /// 尺寸并入键：多显示器不同缩放的条目共存，LRU 自然淘汰。
    /// </summary>
    internal static string SizedKey(string path, int pixelSize) =>
        CacheKey(path) + "@" + Math.Max(1, pixelSize);

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

    private static ImageSource? LoadIcon(string path, int pixelSize)
    {
        // 高 DPI：优先从系统图像列表取更大尺寸的源图标（48 / 256px），
        // 显示时按 32 DIP × 缩放近乎原样呈现。任何一步失败回退 32px 老路径。
        if (pixelSize > 32)
        {
            var larger = TryLoadFromSystemImageList(path, pixelSize);
            if (larger is not null)
                return larger;
        }
        return LoadIcon32(path);
    }

    /// <summary>系统图像列表尺寸标志（commoncontrols.h SHIL_*）。</summary>
    private const int SHIL_EXTRALARGE = 0x2;
    private const int SHIL_JUMBO = 0x4;
    private const int ILD_TRANSPARENT = 0x1;

    /// <summary>
    /// 经 SHGFI_SYSICONINDEX + SHGetImageList 取 48/256px 源图标。
    /// USEFILEATTRIBUTES 语义与 32px 路径一致（不存在的文件按类型给图标）。
    /// </summary>
    private static ImageSource? TryLoadFromSystemImageList(string path, int pixelSize)
    {
        var listFlag = pixelSize <= 48 ? SHIL_EXTRALARGE : SHIL_JUMBO;
        var isDir = Directory.Exists(path);
        var exists = isDir || File.Exists(path);

        var flags = SHGFI_SYSICONINDEX;
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

        var shfi = new SHFILEINFO();
        var hr = SHGetFileInfo(path, attrs, ref shfi, (uint)Marshal.SizeOf<SHFILEINFO>(), flags);
        if (hr == IntPtr.Zero)
            return null;

        IImageList? imageList = null;
        IntPtr hIcon = IntPtr.Zero;
        try
        {
            var iid = new Guid(0x46EB5926, 0x582E, 0x4017, 0x9F, 0xDF, 0xE8, 0x99, 0x8D, 0xAA, 0x09, 0x50);
            if (SHGetImageList(listFlag, ref iid, out imageList) != 0 || imageList is null)
                return null;
            imageList.GetIcon(shfi.iIcon, ILD_TRANSPARENT, out hIcon);
            if (hIcon == IntPtr.Zero)
                return null;
            var source = Imaging.CreateBitmapSourceFromHIcon(
                hIcon,
                Int32Rect.Empty,
                BitmapSizeOptions.FromWidthAndHeight(pixelSize, pixelSize));
            source.Freeze();
            return source;
        }
        catch
        {
            return null;
        }
        finally
        {
            if (hIcon != IntPtr.Zero)
                DestroyIcon(hIcon);
            if (imageList is not null)
            {
                try { Marshal.ReleaseComObject(imageList); } catch { /* ignore */ }
            }
        }
    }

    private static ImageSource? LoadIcon32(string path)
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
    private const uint SHGFI_SYSICONINDEX = 0x000004000;
    private const uint SHGFI_USEFILEATTRIBUTES = 0x000000010;
    private const uint FILE_ATTRIBUTE_NORMAL = 0x80;
    private const uint FILE_ATTRIBUTE_DIRECTORY = 0x10;

    /// <summary>
    /// IImageList（系统图像列表）最小声明：vtable 槽位必须与 commoncontrols.h 逐一对齐——
    /// IUnknown 之后依次是 Add/ReplaceIcon/SetOverlayImage/Replace/AddMasked/Draw/Remove/GetIcon。
    /// 除 GetIcon 外全是占位，绝不能调用；槽位错位会导致 AccessViolation（进程级崩溃，
    /// catch 不可达），改这里时务必对照 SDK 原文。
    /// </summary>
    [ComImport]
    [Guid("46EB5926-582E-4017-9FDF-E8998DAA0950")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IImageList
    {
        void Add(IntPtr hbmImage, IntPtr hbmMask, out int pi);
        void ReplaceIcon(int i, IntPtr hicon, out int pi);
        void SetOverlayImage(int iImage, int iOverlay);
        void Replace(int iImage, IntPtr hbmImage, IntPtr hbmMask);
        void AddMasked(IntPtr hbmImage, int crMask, out int pi);
        void Draw(ref DRAW_PARAMS pimldp);
        void Remove(int i);
        void GetIcon(int i, int flags, out IntPtr hicon);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DRAW_PARAMS
    {
        public IntPtr HwndDst;
        public int XDst;
        public int YDst;
        public int CxDst;
        public int CyDst;
    }

    [DllImport("shell32.dll", EntryPoint = "SHGetImageList", SetLastError = false)]
    private static extern int SHGetImageList(int iImageList, ref Guid riid, out IImageList ppv);

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
