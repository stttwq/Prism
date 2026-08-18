using System.Windows;
using System.Windows.Media;
using Prism.Models;

using WpfColor = System.Windows.Media.Color;
using WpfBrushes = System.Windows.Media.Brushes;
using WpfFlowDirection = System.Windows.FlowDirection;
using WpfFontFamily = System.Windows.Media.FontFamily;
using WpfColorConverter = System.Windows.Media.ColorConverter;

namespace Prism.Services;

/// <summary>
/// 网页引擎图标提供器（G8）。
/// 内置引擎（Bing/百度/Google）使用程序化生成的矢量图标（彩色圆 + 首字母），随程序打包无需二进制文件。
/// 自定义引擎：已授权 origin 用 FaviconCache 缓存的 favicon；未授权或失败回退通用网页图标。
/// </summary>
public sealed class WebIconProvider
{
    private readonly FaviconCache? _faviconCache;
    /// <summary>origin → 是否允许联网获取（读设置里的 FaviconGrants）。</summary>
    private readonly Func<string, bool>? _isGranted;
    private readonly ImageSource _genericIcon;
    private readonly Dictionary<string, ImageSource> _builtInIcons = new(StringComparer.OrdinalIgnoreCase);
    /// <summary>
    /// 成功解析的 favicon 内存缓存：同一 origin 恒定返回同一实例，
    /// ResultList 按引用比较即可零重赋（防逐键闪烁），下载完成后
    /// <see cref="Invalidate"/> 清空、下一次装饰自动换上新图标。
    /// </summary>
    private readonly Dictionary<string, ImageSource> _resolved = new(StringComparer.OrdinalIgnoreCase);

    public WebIconProvider(FaviconCache? faviconCache = null, Func<string, bool>? isGranted = null)
    {
        _faviconCache = faviconCache;
        _isGranted = isGranted;
        _genericIcon = CreateGenericIcon();
        _genericIcon.Freeze();
        // 预建并冻结内置引擎图标，避免每次 GetIcon 重建导致 UI 闪烁。
        foreach (var (name, hex, letter) in new (string, string, string)[]
        {
            ("Bing", "#008373", "B"),
            ("百度", "#2932E1", "百"),
            ("Google", "#4285F4", "G"),
        })
        {
            var icon = CreateLetterIcon(hex, letter);
            icon.Freeze();
            _builtInIcons[name] = icon;
        }
    }

    /// <summary>
    /// 下载完成后清空解析缓存：下一次取图标重新读磁盘，让新 favicon 立即可见。
    /// AUDIT-2026-08-18 C-D2: <see cref="Invalidate"/> 被后台线程（DownloadFavicon 的
    /// Task.Run finally 块）调用，而 <see cref="GetIcon"/> 在 UI 线程读写 <see cref="_resolved"/>。
    /// Dictionary 非线程安全——经 Dispatcher.BeginInvoke 投递到 UI 线程清空，保持单线程语义。
    /// </summary>
    public void Invalidate()
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is null || dispatcher.CheckAccess())
            _resolved.Clear();
        else
            dispatcher.BeginInvoke(new Action(() => _resolved.Clear()));
    }

    /// <summary>
    /// 获取引擎图标。优先内置图标，其次（已授权的）favicon 缓存，最后通用图标。
    /// </summary>
    /// <param name="url">引擎 URL（用于提取 origin 查 favicon 缓存，或推断内置引擎）。</param>
    /// <param name="engineName">引擎显示名（可选，用于精确匹配内置图标）。</param>
    public ImageSource GetIcon(string url, string? engineName = null)
    {
        // 内置引擎：优先用 engineName 匹配，其次从 URL 推断
        var name = engineName ?? InferEngineName(url);
        if (name is not null && _builtInIcons.TryGetValue(name, out var cached))
            return cached;

        // 自定义引擎：查 favicon 缓存（必须已授权该 origin）
        if (_faviconCache is not null)
        {
            var origin = FaviconCache.NormalizeOrigin(url);
            if (origin is not null && _isGranted?.Invoke(origin) != false)
            {
                if (_resolved.TryGetValue(origin, out var hit))
                    return hit;
                var favicon = _faviconCache.GetFavicon(origin, granted: true);
                if (favicon is not null)
                {
                    favicon.Freeze();
                    _resolved[origin] = favicon;
                    return favicon;
                }
                // 未命中（尚未下载/损坏）继续回退通用图标；不做负缓存——
                // 下载完成后无需 Invalidate 也能在下一次装饰时拿到。
            }
        }

        // 回退通用图标
        return _genericIcon;
    }

    /// <summary>
    /// 图标身份键：决定 GetIcon 返回哪个图标的最小信息（内置引擎名或 origin）。
    /// 网页模式的 URL 每按一次键就变，但图标只取决于引擎，用它作为 UI 侧的缓存键，
    /// 避免逐键重新赋值 Image.Source 造成闪烁。
    /// </summary>
    public string IconKey(string url, string? engineName = null)
    {
        var name = engineName ?? InferEngineName(url);
        if (name is not null && _builtInIcons.ContainsKey(name))
            return "builtin:" + name;
        return FaviconCache.NormalizeOrigin(url) ?? "generic";
    }

    /// <summary>从 URL 推断内置引擎名。非内置引擎返回 null。</summary>
    private static string? InferEngineName(string url)
    {
        var lower = url.ToLowerInvariant();
        if (lower.Contains("bing.com")) return "Bing";
        if (lower.Contains("baidu.com")) return "百度";
        if (lower.Contains("google.com")) return "Google";
        return null;
    }

    /// <summary>通用网页图标：灰色圆 + "W"。</summary>
    private static ImageSource CreateGenericIcon() =>
        CreateLetterIcon("#707070", "W");

    /// <summary>
    /// 生成 32×32 图标：彩色圆背景 + 白色首字母。
    /// 用 DrawingImage 矢量绘制，无需二进制资源文件。
    /// </summary>
    private static ImageSource CreateLetterIcon(string hexColor, string letter)
    {
        var color = (WpfColor)WpfColorConverter.ConvertFromString(hexColor);
        var brush = new SolidColorBrush(color);
        var pen = new Pen(WpfBrushes.Transparent, 0);
        var geometry = new EllipseGeometry(new Rect(2, 2, 28, 28));

        var drawing = new DrawingGroup();
        using (var dc = drawing.Open())
        {
            dc.DrawGeometry(brush, pen, geometry);
            // 首字母居中：白色，粗体，16px
            var formatted = new FormattedText(
                letter,
                System.Globalization.CultureInfo.InvariantCulture,
                WpfFlowDirection.LeftToRight,
                new Typeface(new WpfFontFamily("Segoe UI"), FontStyles.Normal, FontWeights.Bold, FontStretches.Normal),
                16,
                WpfBrushes.White,
                1.0);
            // 居中
            var x = (32 - formatted.Width) / 2;
            var y = (32 - formatted.Height) / 2;
            dc.DrawText(formatted, new Point(x, y));
        }

        var img = new DrawingImage(drawing);
        return img;
    }
}
