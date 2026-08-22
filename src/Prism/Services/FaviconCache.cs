using System.IO;
using System.Net.Http;
using System.Text.Json;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// favicon 缓存服务（G8）。自定义引擎保存或 origin 变化时单独征求联网许可。
/// 拒绝、失败或损坏回退通用图标。内置引擎图标随程序打包，不走此路径。
/// </summary>
public sealed class FaviconCache
{
    /// <summary>favicon 缓存目录名（位于用户数据目录下）。</summary>
    public const string CacheDirName = "favicons";

    /// <summary>最大缓存条目数（LRU 淘汰）。</summary>
    private const int MaxEntries = 64;
    /// <summary>单文件大小上限（256 KB）。</summary>
    private const int MaxFileSize = 256 * 1024;
    /// <summary>最大重定向次数。</summary>
    private const int MaxRedirects = 3;
    /// <summary>最大像素尺寸（128×128）。</summary>
    private const int MaxPixelDimension = 128;
    /// <summary>metadata schema 版本。</summary>
    private const int MetadataVersion = 1;

    private readonly string _cacheDir;
    private readonly HttpClient _http;

    /// <summary>缓存目录绝对路径（供卸载清单使用）。</summary>
    public string CacheDirectory => _cacheDir;

    public FaviconCache(string dataDir)
    {
        _cacheDir = Path.Combine(dataDir, CacheDirName);
        Directory.CreateDirectory(_cacheDir);
        var handler = new SocketsHttpHandler
        {
            AllowAutoRedirect = true,
            MaxAutomaticRedirections = MaxRedirects,
        };
        _http = new HttpClient(handler)
        {
            Timeout = TimeSpan.FromSeconds(5),
            MaxResponseContentBufferSize = MaxFileSize + 1024,
        };
    }

    /// <summary>
    /// 获取指定 origin 的 favicon。如果已授权且缓存有效则返回缓存；
    /// 否则返回 null（调用方回退通用图标）。
    /// </summary>
    /// <param name="origin">规范化 origin（scheme://host[:port]）。</param>
    /// <param name="granted">是否已获得用户授权。</param>
    public ImageSource? GetFavicon(string origin, bool granted)
    {
        if (string.IsNullOrEmpty(origin) || !granted)
            return null;

        var key = NormalizeOrigin(origin);
        if (key is null)
            return null;

        // 先查缓存
        var meta = ReadMetadata(key);
        if (meta is not null)
        {
            var img = TryLoadImage(key);
            if (img is not null)
                return img;
            // 缓存损坏：删除 metadata 和图片
            DeleteEntry(key);
        }

        return null;
    }

    /// <summary>
    /// 下载并缓存 favicon。下载前验证 MIME、大小、实际格式和像素尺寸。
    /// 直接下载 <c>/favicon.ico</c> 失败（404 / 格式不符）时回退 DuckDuckGo
    /// 图标聚合服务（域名只发给 duckduckgo.com，不引入 Google）。
    /// 失败、拒绝或损坏返回 null。
    /// </summary>
    public async Task<ImageSource?> DownloadFaviconAsync(
        string origin,
        CancellationToken ct = default)
    {
        var key = NormalizeOrigin(origin);
        if (key is null)
            return null;

        var host = key[(key.IndexOf("://", StringComparison.Ordinal) + 3)..];
        var sources = new[]
        {
            // key 本身已含 scheme（如 "https://example.com"），直接拼根路径；
            // 此前多套一层 "https://" 得到 https://https://… 双 scheme，
            // 直连下载永远 DNS 失败，静默退化为聚合源。
            $"{key}/favicon.ico",
            $"https://icons.duckduckgo.com/ip3/{host}.ico",
        };
        foreach (var faviconUrl in sources)
        {
            var downloaded = await TryDownloadAsync(key, faviconUrl, ct).ConfigureAwait(false);
            if (downloaded is not null)
                return downloaded;
        }
        return null;
    }

    private async Task<ImageSource?> TryDownloadAsync(
        string key,
        string faviconUrl,
        CancellationToken ct)
    {
        try
        {
            var resp = await _http.GetAsync(faviconUrl, HttpCompletionOption.ResponseHeadersRead, ct)
                .ConfigureAwait(false);
            if (!resp.IsSuccessStatusCode)
                return null;

            // 验证 MIME
            var contentType = resp.Content.Headers.ContentType?.MediaType ?? "";
            if (!IsValidImageMime(contentType))
                return null;

            // 读取并限制字节
            using var stream = await resp.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
            var buf = new byte[MaxFileSize];
            var read = 0;
            int n;
            while (read < buf.Length && (n = await stream.ReadAsync(buf.AsMemory(read), ct).ConfigureAwait(false)) > 0)
                read += n;

            // 验证实际格式和像素尺寸
            if (!ValidateImageData(buf[..read], out var width, out var height))
                return null;

            // 原子写：先写临时文件再重命名（B8：落盘名统一经 KeyToFileName）
            var tmpPath = Path.Combine(_cacheDir, KeyToFileName(key) + ".tmp");
            await File.WriteAllBytesAsync(tmpPath, buf[..read], ct).ConfigureAwait(false);

            var imgPath = GetImagePath(key);
            var metaPath = GetMetadataPath(key);
            // M16（全仓复审 2026-08-22）：原子的 overwrite-Move 取代
            // Delete+Move——两步之间的窗口里没有图，且崩溃会留下无 .meta 的
            // 孤儿 .img（TrimCache 只枚举 *.meta，永不淘汰）。对齐 SettingsStore/
            // StagingStore 的 File.Move(tmp, path, overwrite: true) 写法。
            File.Move(tmpPath, imgPath, overwrite: true);

            // 写 metadata
            var metadata = new FaviconMetadata
            {
                Version = MetadataVersion,
                Origin = key,
                Etag = resp.Headers.ETag?.Tag,
                FileName = Path.GetFileName(imgPath),
                Size = read,
                Width = width,
                Height = height,
                DownloadedAt = DateTimeOffset.UtcNow.ToString("o"),
            };
            await File.WriteAllTextAsync(metaPath, JsonSerializer.Serialize(metadata), ct)
                .ConfigureAwait(false);

            // LRU 淘汰
            TrimCache();

            return LoadImageFromFile(imgPath);
        }
        catch
        {
            return null;
        }
    }

    /// <summary>规范化 origin：小写 scheme+host，保留非默认端口。</summary>
    internal static string? NormalizeOrigin(string origin)
    {
        if (string.IsNullOrWhiteSpace(origin))
            return null;
        // 只允许 http/https
        var lower = origin.ToLowerInvariant();
        if (!lower.StartsWith("https://") && !lower.StartsWith("http://"))
            return null;
        // 提取 scheme://host[:port]
        var schemeEnd = lower.IndexOf("://", StringComparison.Ordinal);
        if (schemeEnd < 0)
            return null;
        var rest = lower[(schemeEnd + 3)..];
        // 去掉路径、查询、片段
        var pathStart = rest.IndexOfAny(['/', '?', '#']);
        if (pathStart >= 0)
            rest = rest[..pathStart];
        var scheme = lower[..schemeEnd];
        return $"{scheme}://{rest}";
    }

    /// <summary>验证 MIME 类型是否为已知图片格式。</summary>
    private static bool IsValidImageMime(string mime) =>
        mime.Equals("image/x-icon", StringComparison.OrdinalIgnoreCase)
        || mime.Equals("image/vnd.microsoft.icon", StringComparison.OrdinalIgnoreCase)
        || mime.Equals("image/png", StringComparison.OrdinalIgnoreCase)
        || mime.Equals("image/jpeg", StringComparison.OrdinalIgnoreCase)
        || mime.Equals("image/gif", StringComparison.OrdinalIgnoreCase)
        || mime.Equals("image/webp", StringComparison.OrdinalIgnoreCase)
        || mime.Equals("image/bmp", StringComparison.OrdinalIgnoreCase);

    /// <summary>验证字节数据为有效图片且尺寸在限制内。</summary>
    private static bool ValidateImageData(byte[] data, out int width, out int height)
    {
        width = 0;
        height = 0;
        if (data.Length < 16)
            return false;
        try
        {
            using var ms = new MemoryStream(data);
            var decoder = BitmapDecoder.Create(
                ms, BitmapCreateOptions.DelayCreation, BitmapCacheOption.None);
            var frame = decoder.Frames[0];
            width = frame.PixelWidth;
            height = frame.PixelHeight;
            return width > 0 && height > 0
                && width <= MaxPixelDimension && height <= MaxPixelDimension;
        }
        catch
        {
            return false;
        }
    }

    /// <summary>从缓存文件加载图片。</summary>
    private static ImageSource? LoadImageFromFile(string path)
    {
        try
        {
            using var fs = File.OpenRead(path);
            var img = BitmapFrame.Create(fs, BitmapCreateOptions.None, BitmapCacheOption.OnLoad);
            img.Freeze();
            return img;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>
    /// B8（AUDIT-4 批次C）：origin 到落盘文件名的统一映射。端口 origin
    ///（如 https://host:8080）此前会把 <c>:</c> 留在文件名里——NTFS 把
    /// <c>name:stream</c> 解释成 ADS 备用数据流，favicon 静默写丢且脱离
    /// LRU 淘汰。所有落盘名都经本函数（<c>:</c> 一并替换）。
    /// </summary>
    internal static string KeyToFileName(string key) =>
        key.Replace("://", "_").Replace('/', '_').Replace(':', '_');

    private ImageSource? TryLoadImage(string key)
    {
        var path = GetImagePath(key);
        if (!File.Exists(path))
            return null;
        return LoadImageFromFile(path);
    }

    private FaviconMetadata? ReadMetadata(string key)
    {
        var path = GetMetadataPath(key);
        if (!File.Exists(path))
            return null;
        try
        {
            var json = File.ReadAllText(path);
            return JsonSerializer.Deserialize<FaviconMetadata>(json);
        }
        catch
        {
            return null;
        }
    }

    private string GetImagePath(string key) =>
        Path.Combine(_cacheDir, KeyToFileName(key) + ".img");

    private string GetMetadataPath(string key) =>
        Path.Combine(_cacheDir, KeyToFileName(key) + ".meta");

    private void DeleteEntry(string key)
    {
        try { File.Delete(GetImagePath(key)); } catch { /* ignore */ }
        try { File.Delete(GetMetadataPath(key)); } catch { /* ignore */ }
    }

    /// <summary>LRU 淘汰：按文件最后写入时间排序，删除最旧的。</summary>
    private void TrimCache()
    {
        try
        {
            // M16：顺手清掉无 .meta 的孤儿 .img（历史版本 Delete+Move 窗口 /
            // .meta 写入前崩溃留下的），否则它们永不进淘汰视野。
            foreach (var orphan in Directory.GetFiles(_cacheDir, "*.img"))
            {
                var meta = Path.ChangeExtension(orphan, ".meta");
                if (!File.Exists(meta))
                    try { File.Delete(orphan); } catch { /* ignore */ }
            }

            var files = Directory.GetFiles(_cacheDir, "*.meta");
            if (files.Length <= MaxEntries)
                return;
            Array.Sort(files, (a, b) => File.GetLastWriteTimeUtc(a).CompareTo(File.GetLastWriteTimeUtc(b)));
            var toDelete = files.Length - MaxEntries;
            for (var i = 0; i < toDelete; i++)
            {
                var metaPath = files[i];
                var baseName = Path.GetFileNameWithoutExtension(metaPath);
                var imgPath = Path.Combine(_cacheDir, baseName + ".img");
                try { File.Delete(metaPath); } catch { /* ignore */ }
                try { File.Delete(imgPath); } catch { /* ignore */ }
            }
        }
        catch { /* ignore */ }
    }

    /// <summary>清空缓存（卸载或清理时调用）。</summary>
    public void Clear()
    {
        try
        {
            if (Directory.Exists(_cacheDir))
                Directory.Delete(_cacheDir, recursive: true);
        }
        catch { /* ignore */ }
    }
}

/// <summary>favicon 缓存元数据（版本化）。</summary>
internal sealed class FaviconMetadata
{
    public int Version { get; set; }
    public string Origin { get; set; } = "";
    public string? Etag { get; set; }
    public string FileName { get; set; } = "";
    public int Size { get; set; }
    public int Width { get; set; }
    public int Height { get; set; }
    public string DownloadedAt { get; set; } = "";
}
