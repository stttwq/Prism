using System.IO;
using System.Text.Json;
using System.Windows.Threading;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// B2：favicon 授权门控与解析缓存。授权流程此前写完 grant 就结束——
/// 下载无人调用、结果侧硬编码 granted:true。这里验证接通后的语义：
/// 未授权 origin 一律通用图标；授权 + 磁盘缓存命中才返回 favicon；
/// 解析缓存给稳定引用（防逐键重赋），Invalidate 后重新读盘。
/// WPF 图像解码在 STA 线程上执行（与视觉测试同一模式）。
/// </summary>
public sealed class FaviconGrantTests : IDisposable
{
    private readonly string _dir =
        Path.Combine(Path.GetTempPath(), "prism-favicon-test-" + Guid.NewGuid().ToString("N"));

    private const string Origin = "https://example.com";
    private const string Url = "https://example.com/search?q={q}";

    // 1×1 透明 PNG。
    private static readonly byte[] Png = Convert.FromBase64String(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=");

    public FaviconGrantTests() => Directory.CreateDirectory(_dir);

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { /* ignore */ }
    }

    private FaviconCache CacheWithPlantedFavicon()
    {
        var cache = new FaviconCache(_dir);
        // favicon 落在数据目录的 favicons 子目录（CacheDirName）。
        var cacheDir = Path.Combine(_dir, FaviconCache.CacheDirName);
        Directory.CreateDirectory(cacheDir);
        var key = FaviconCache.NormalizeOrigin(Origin)!;
        var baseName = key.Replace("://", "_").Replace('/', '_');
        File.WriteAllBytes(Path.Combine(cacheDir, baseName + ".img"), Png);
        File.WriteAllText(
            Path.Combine(cacheDir, baseName + ".meta"),
            JsonSerializer.Serialize(new FaviconMetadata
            {
                Version = 1,
                Origin = key,
                FileName = baseName + ".img",
                Size = Png.Length,
                Width = 1,
                Height = 1,
                DownloadedAt = DateTimeOffset.UtcNow.ToString("o"),
            }));
        return cache;
    }

    [Fact]
    public void UnauthorizedOriginFallsBackToGenericEvenWithCacheHit()
    {
        RunOnSta(() =>
        {
            var cache = CacheWithPlantedFavicon();
            var provider = new WebIconProvider(cache, _ => false);

            var icon = provider.GetIcon(Url);
            // 基线取同一 provider 的通用回退（未知 origin），引用必须一致。
            var generic = provider.GetIcon("https://unknown.invalid/x");
            Assert.Same(generic, icon);
        });
    }

    [Fact]
    public void GrantedOriginWithCacheHitReturnsFaviconWithStableReference()
    {
        RunOnSta(() =>
        {
            var cache = CacheWithPlantedFavicon();
            var provider = new WebIconProvider(cache, _ => true);

            var first = provider.GetIcon(Url);
            var second = provider.GetIcon(Url);
            var generic = provider.GetIcon("https://unknown.invalid/x");

            Assert.NotSame(generic, first);
            Assert.Same(first, second);
        });
    }

    [Fact]
    public void MissingCacheEntryStillFallsBackToGeneric()
    {
        RunOnSta(() =>
        {
            // 缓存目录里什么都没有：即使授权了也只是通用图标（下载尚未完成时的表现）。
            var cache = new FaviconCache(_dir);
            var provider = new WebIconProvider(cache, _ => true);

            Assert.Same(provider.GetIcon("https://unknown.invalid/x"), provider.GetIcon(Url));
        });
    }

    [Fact]
    public void InvalidateDropsResolvedCacheAndReloadsFromDisk()
    {
        RunOnSta(() =>
        {
            var cache = CacheWithPlantedFavicon();
            var provider = new WebIconProvider(cache, _ => true);

            var first = provider.GetIcon(Url);
            provider.Invalidate();
            var reloaded = provider.GetIcon(Url);
            var generic = provider.GetIcon("https://unknown.invalid/x");

            // 重新从磁盘解析：仍是 favicon（非通用图标）但是新实例。
            Assert.NotSame(first, reloaded);
            Assert.NotSame(generic, reloaded);
        });
    }

    [Fact]
    public void BuiltinEnginesBypassGrantAndCache()
    {
        RunOnSta(() =>
        {
            var cache = CacheWithPlantedFavicon();
            var provider = new WebIconProvider(cache, _ => false);

            // 内置引擎不走授权/favicon：与无缓存 provider 的内置图标同源（各自实例，仅语义对照）。
            var bing = provider.GetIcon("https://www.bing.com/search?q=x");
            Assert.NotSame(provider.GetIcon("https://unknown.invalid/x"), bing);
            // 同一 provider 内多次取内置图标必须稳定。
            Assert.Same(bing, provider.GetIcon("https://www.bing.com/search?q=y"));
        });
    }

    /// <summary>WPF 图像解码与视觉测试同模式：在 STA 线程上执行。</summary>
    private static void RunOnSta(Action action)
    {
        Exception? error = null;
        var thread = new Thread(() =>
        {
            try { action(); }
            catch (Exception ex) { error = ex; }
            finally { Dispatcher.CurrentDispatcher.InvokeShutdown(); }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.IsBackground = true;
        thread.Start();
        Assert.True(thread.Join(TimeSpan.FromSeconds(30)), "STA 线程超时");
        if (error is not null)
            throw new Xunit.Sdk.XunitException("STA 线程内失败：" + error);
    }
}
