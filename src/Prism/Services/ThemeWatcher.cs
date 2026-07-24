using System.Globalization;
using System.Windows;
using Microsoft.Win32;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 监听 Windows 应用浅/深色偏好（AppsUseLightTheme），切换 Application 资源字典。
/// 规格：frontend-spec.md §5；AppState.Theme 由本类独占写入。
/// </summary>
public sealed class ThemeWatcher : IDisposable
{
    private const string PersonalizeKey =
        @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

    private readonly AppState _state;
    private ResourceDictionary? _tokensDict;
    private bool _disposed;

    public ThemeWatcher(AppState state)
    {
        _state = state;
    }

    /// <summary>启动时调用：应用当前系统主题并订阅变化。</summary>
    public void Start()
    {
        Apply(ReadSystemTheme());
        SystemEvents.UserPreferenceChanged += OnUserPreferenceChanged;
    }

    public AppTheme Current => _state.Theme;

    /// <summary>主题资源字典替换后触发，供控件刷新缓存画刷。</summary>
    public event Action? ThemeApplied;

    private void OnUserPreferenceChanged(object sender, UserPreferenceChangedEventArgs e)
    {
        // General / Color 都可能伴随深浅色切换。
        if (e.Category is not (UserPreferenceCategory.General or UserPreferenceCategory.Color))
            return;
        var theme = ReadSystemTheme();
        var app = Application.Current;
        if (app is null) return;
        if (app.Dispatcher.CheckAccess())
            Apply(theme);
        else
            app.Dispatcher.Invoke(() => Apply(theme));
    }

    public static AppTheme ReadSystemTheme()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(PersonalizeKey);
            var value = key?.GetValue("AppsUseLightTheme");
            if (value is int i)
                return i == 0 ? AppTheme.Dark : AppTheme.Light;
            if (value is not null
                && int.TryParse(Convert.ToString(value, CultureInfo.InvariantCulture), out var n))
                return n == 0 ? AppTheme.Dark : AppTheme.Light;
        }
        catch
        {
            // 读注册表失败默认浅色（与截图基准一致）。
        }
        return AppTheme.Light;
    }

    private void Apply(AppTheme theme)
    {
        if (_disposed) return;

        var app = Application.Current;
        if (app is null) return;

        // 主题未变且字典已就位则跳过（启动后重复事件）。
        if (_tokensDict is not null && _state.Theme == theme)
            return;

        var source = theme == AppTheme.Dark
            ? "Themes/Tokens.Dark.xaml"
            : "Themes/Tokens.Light.xaml";

        var dict = new ResourceDictionary
        {
            Source = new Uri(source, UriKind.Relative),
        };

        var merged = app.Resources.MergedDictionaries;
        if (_tokensDict is not null)
        {
            merged.Remove(_tokensDict);
        }
        else
        {
            // 启动时 App.xaml 已合并 Tokens.Light，替换令牌字典；Styles.xaml 保留。
            for (var i = 0; i < merged.Count; i++)
            {
                var src = merged[i].Source?.OriginalString ?? "";
                if (src.Contains("Tokens.", StringComparison.OrdinalIgnoreCase))
                {
                    merged.RemoveAt(i);
                    break;
                }
            }
        }

        merged.Insert(0, dict);
        _tokensDict = dict;
        _state.Theme = theme;
        ThemeApplied?.Invoke();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        SystemEvents.UserPreferenceChanged -= OnUserPreferenceChanged;
    }
}
