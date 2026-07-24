// WPF + WinForms 并存时，默认类型优先 WPF，避免 CS0104。
// NotifyIcon 等托盘 API 在 TrayService 中显式使用 System.Windows.Forms。
global using Application = System.Windows.Application;
global using Brush = System.Windows.Media.Brush;
global using Color = System.Windows.Media.Color;
global using Image = System.Windows.Controls.Image;
global using KeyEventArgs = System.Windows.Input.KeyEventArgs;
global using MessageBox = System.Windows.MessageBox;
global using MessageBoxButton = System.Windows.MessageBoxButton;
global using MessageBoxImage = System.Windows.MessageBoxImage;
global using Pen = System.Windows.Media.Pen;
global using Point = System.Windows.Point;
global using UserControl = System.Windows.Controls.UserControl;
