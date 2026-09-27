using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;

namespace Prism.Controls;

/// <summary>
/// 右上角外挂圆形固定按钮（frontend-spec.md PinButton）。
/// 直径 28px、白底、圆形阴影；点击切换 IsPinned。
/// </summary>
public partial class PinButton : UserControl
{
    public static readonly DependencyProperty IsPinnedProperty =
        DependencyProperty.Register(
            nameof(IsPinned),
            typeof(bool),
            typeof(PinButton),
            new FrameworkPropertyMetadata(
                false,
                FrameworkPropertyMetadataOptions.BindsTwoWayByDefault,
                OnIsPinnedChanged));

    public PinButton()
    {
        InitializeComponent();
        UpdateVisual();
    }

    public bool IsPinned
    {
        get => (bool)GetValue(IsPinnedProperty);
        set => SetValue(IsPinnedProperty, value);
    }

    public event Action<bool>? IsPinnedChanged;

    private static void OnIsPinnedChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is PinButton btn)
            btn.UpdateVisual();
    }

    private void OnClick(object sender, MouseButtonEventArgs e)
    {
        IsPinned = !IsPinned;
        IsPinnedChanged?.Invoke(IsPinned);
        e.Handled = true;
    }

    /// <summary>主题切换后强制重取 DynamicResource 画刷（同值 Set 不会触发 DP 回调）。</summary>
    public void RefreshTheme() => UpdateVisual();

    private void UpdateVisual()
    {
        // 固定时箭头变为"钉住"态：略深背景 + 旋转 0°；未固定保持 45° 斜箭头。
        if (Arrow is null || Bd is null) return;
        if (Arrow.RenderTransform is not RotateTransform rotate)
        {
            rotate = new RotateTransform(IsPinned ? 0 : 45);
            Arrow.RenderTransform = rotate;
        }
        // 省略 From：从当前（动画中的）角度平滑续接到新目标。
        rotate.BeginAnimation(
            RotateTransform.AngleProperty,
            new DoubleAnimation(IsPinned ? 0 : 45, System.TimeSpan.FromMilliseconds(120))
            {
                EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            });
        Arrow.Opacity = IsPinned ? 1.0 : 0.75;
        Bd.Background = IsPinned
            ? (TryFindResource("BgItemSelected") as Brush) ?? Bd.Background
            : (TryFindResource("BgWindow") as Brush) ?? Bd.Background;
        ToolTip = IsPinned ? "取消固定窗口" : "固定窗口（失焦不隐藏）";
    }
}
