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
    private readonly ImageSource _genericIcon;

    public WebIconProvider(FaviconCache? faviconCache = null)
    {
        _faviconCache = faviconCache;
        _genericIcon = CreateGenericIcon();
        _genericIcon.Freeze();
    }

    /// <summary>
    /// 获取引擎图标。优先内置图标，其次 favicon 缓存，最后通用图标。
    /// </summary>
    /// <param name="engineName">引擎显示名。</param>
    /// <param name="url">引擎 URL（用于提取 origin 查 favicon 缓存）。</param>
    public ImageSource GetIcon(string engineName, string url)
    {
        // 内置引擎：程序化生成图标
        var builtIn = CreateBuiltInIcon(engineName);
        if (builtIn is not null)
        {
            builtIn.Freeze();
            return builtIn;
        }

        // 自定义引擎：查 favicon 缓存
        if (_faviconCache is not null)
        {
            var origin = FaviconCache.NormalizeOrigin(url);
            if (origin is not null)
            {
                // 检查是否已授权
                var granted = _faviconCache.GetFavicon(origin, granted: true);
                if (granted is not null)
                    return granted;
            }
        }

        // 回退通用图标
        return _genericIcon;
    }

    /// <summary>生成内置引擎图标。非内置引擎返回 null。</summary>
    private static ImageSource? CreateBuiltInIcon(string engineName)
    {
        return engineName switch
        {
            "Bing" => CreateLetterIcon("#008373", "B"),
            "百度" => CreateLetterIcon("#2932E1", "百"),
            "Google" => CreateLetterIcon("#4285F4", "G"),
            _ => null,
        };
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
