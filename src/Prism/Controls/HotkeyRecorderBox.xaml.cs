using System.Windows;
using System.Windows.Input;
using System.Windows.Media;

namespace Prism.Controls;

/// <summary>
/// 组合键录制框（frontend-spec HotkeyRecorderBox）。
/// 点击后进入录制，按下修饰键+主键后把 "Alt+Space" 形式写回 <see cref="Value"/>。
/// Esc 取消录制；单独修饰键不提交。
/// </summary>
public partial class HotkeyRecorderBox : System.Windows.Controls.UserControl
{
    public static readonly DependencyProperty ValueProperty = DependencyProperty.Register(
        nameof(Value),
        typeof(string),
        typeof(HotkeyRecorderBox),
        new FrameworkPropertyMetadata(
            "",
            FrameworkPropertyMetadataOptions.BindsTwoWayByDefault,
            OnValueChanged));

    public static readonly DependencyProperty IsRecordingProperty = DependencyProperty.Register(
        nameof(IsRecording),
        typeof(bool),
        typeof(HotkeyRecorderBox),
        new PropertyMetadata(false, OnIsRecordingChanged));

    private static readonly Brush IdleBorder = Freeze(new SolidColorBrush(Color.FromRgb(0xD0, 0xD3, 0xD8)));
    private static readonly Brush ActiveBorder = Freeze(new SolidColorBrush(Color.FromRgb(0x1E, 0x7A, 0xD4)));
    private static readonly Brush IdleFg = Freeze(new SolidColorBrush(Color.FromRgb(0x37, 0x39, 0x3E)));
    private static readonly Brush HintFg = Freeze(new SolidColorBrush(Color.FromRgb(0x9B, 0x9F, 0xA6)));

    private static Brush Freeze(SolidColorBrush b)
    {
        if (b.CanFreeze) b.Freeze();
        return b;
    }

    public string Value
    {
        get => (string)GetValue(ValueProperty);
        set => SetValue(ValueProperty, value);
    }

    public bool IsRecording
    {
        get => (bool)GetValue(IsRecordingProperty);
        set => SetValue(IsRecordingProperty, value);
    }

    public event EventHandler? ValueChanged;

    public HotkeyRecorderBox()
    {
        InitializeComponent();
        UpdateDisplay();
    }

    private static void OnValueChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is HotkeyRecorderBox box)
        {
            box.UpdateDisplay();
            box.ValueChanged?.Invoke(box, EventArgs.Empty);
        }
    }

    private static void OnIsRecordingChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is HotkeyRecorderBox box)
            box.UpdateDisplay();
    }

    private void OnMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        Focus();
        IsRecording = true;
        e.Handled = true;
    }

    private void OnGotKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        IsRecording = true;
    }

    private void OnLostKeyboardFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        IsRecording = false;
    }

    private void OnPreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (!IsRecording)
            return;

        var key = e.Key == Key.System ? e.SystemKey : e.Key;

        // Esc：取消录制，不改 Value。
        if (key == Key.Escape)
        {
            IsRecording = false;
            Keyboard.ClearFocus();
            e.Handled = true;
            return;
        }

        // 单独修饰键：只更新提示，不提交。
        if (key is Key.LeftCtrl or Key.RightCtrl or Key.LeftAlt or Key.RightAlt
            or Key.LeftShift or Key.RightShift or Key.LWin or Key.RWin
            or Key.System)
        {
            e.Handled = true;
            return;
        }

        // Tab 等导航键在录制中也当作主键提交（用户明确按了）。
        var mods = Keyboard.Modifiers;
        var parts = new List<string>();
        if (mods.HasFlag(ModifierKeys.Control)) parts.Add("Ctrl");
        if (mods.HasFlag(ModifierKeys.Alt)) parts.Add("Alt");
        if (mods.HasFlag(ModifierKeys.Shift)) parts.Add("Shift");
        if (mods.HasFlag(ModifierKeys.Windows)) parts.Add("Win");

        var keyName = FormatKey(key);
        if (string.IsNullOrEmpty(keyName))
        {
            e.Handled = true;
            return;
        }

        // 至少需要一个修饰键，避免单独字母抢占全局输入。
        if (parts.Count == 0)
        {
            Display.Text = "请配合 Ctrl / Alt / Shift / Win 使用";
            Display.Foreground = HintFg;
            e.Handled = true;
            return;
        }

        parts.Add(keyName);
        Value = string.Join("+", parts);
        IsRecording = false;
        Keyboard.ClearFocus();
        e.Handled = true;
    }

    private static string FormatKey(Key key)
    {
        // 返回值必须能被 HotkeyService.ParseCombo 用 Enum.TryParse<Key> 解析。
        // 因此用 Key 枚举名（OemComma 等），不要自造 "Comma"。
        if (key is >= Key.D0 and <= Key.D9)
            return key.ToString(); // D0..D9
        if (key is >= Key.NumPad0 and <= Key.NumPad9)
            return key.ToString(); // NumPad0..
        if (key is >= Key.A and <= Key.Z)
            return key.ToString();
        // Space / F1.. / 方向键 / Oem* 等枚举名本身可解析。
        return key.ToString();
    }

    private void UpdateDisplay()
    {
        if (Chrome is null || Display is null) return;

        if (IsRecording)
        {
            Chrome.BorderBrush = ActiveBorder;
            Display.Text = "请按下组合键…（Esc 取消）";
            Display.Foreground = HintFg;
        }
        else if (string.IsNullOrWhiteSpace(Value))
        {
            Chrome.BorderBrush = IdleBorder;
            Display.Text = "点击此处，然后按下组合键";
            Display.Foreground = HintFg;
        }
        else
        {
            Chrome.BorderBrush = IdleBorder;
            Display.Text = Value;
            Display.Foreground = IdleFg;
        }
    }
}
