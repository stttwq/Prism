using Prism.Services;
using Xunit;

namespace Prism.Tests;

/// <summary>
/// 高 DPI 图标（B1）：缓存键必须并入像素尺寸——
/// 同一路径在不同缩放的显示器上各有一份条目，多显示器双尺寸共存由 LRU 淘汰。
/// </summary>
public sealed class IconCacheKeyTests
{
    [Theory]
    [InlineData(@"C:\app\tool.exe")]
    [InlineData(@"C:\docs\readme.txt")]
    [InlineData(@"C:\notes\link.lnk")]
    [InlineData(@"C:\folder\")]
    public void SizedKey_DistinguishesPixelSizes_And_IsStable(string path)
    {
        var at32 = IconCache.SizedKey(path, 32);
        var at48 = IconCache.SizedKey(path, 48);
        var at64 = IconCache.SizedKey(path, 64);

        Assert.NotEqual(at32, at48);
        Assert.NotEqual(at48, at64);
        Assert.Equal(at32, IconCache.SizedKey(path, 32));

        // 尺寸合并发生在扩展名/路径归类之后：同尺寸下扩展名共享键的语义不变。
        Assert.Equal(
            IconCache.SizedKey(@"C:\other\memo.txt", 48),
            IconCache.SizedKey(path == @"C:\docs\readme.txt" ? @"D:\x.txt" : @"C:\docs\readme.txt", 48));
    }

    [Fact]
    public async Task GetAsync_ReturnsNonNullIconForExtensionAtRequestedSize()
    {
        // 不存在的文件走 USEFILEATTRIBUTES 分支：按扩展名给图标，两条尺寸路径都必须有产物。
        var cache = new IconCache();
        var icon32 = await cache.GetAsync(@"Z:\definitely\not\here\document.txt", 32);
        var icon48 = await cache.GetAsync(@"Z:\definitely\not\here\document.txt", 48);

        Assert.NotNull(icon32);
        Assert.NotNull(icon48);
        // 48px 请求在 SHGetImageList 失败时允许回退 32px 位图，但绝不为空。
        Assert.True(icon48!.Width >= 32);
    }
}
