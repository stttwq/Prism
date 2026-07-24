using System.IO;
using Microsoft.Win32;

namespace Prism.Services;

/// <summary>
/// 开机自启：写 HKCU\Software\Microsoft\Windows\CurrentVersion\Run。
/// 值名固定为 Prism；启用时写入当前 Prism.exe 的带引号绝对路径（兼容空格/中文路径）。
/// </summary>
public sealed class AutoStartService
{
    private const string RunKeyPath = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string ValueName = "Prism";

    /// <summary>注册表中是否已存在自启项。</summary>
    public bool IsEnabled
    {
        get
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKeyPath, writable: false);
            return key?.GetValue(ValueName) is string s && !string.IsNullOrWhiteSpace(s);
        }
    }

    /// <summary>启用或关闭开机自启。</summary>
    public void SetEnabled(bool enabled)
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKeyPath, writable: true)
            ?? Registry.CurrentUser.CreateSubKey(RunKeyPath)
            ?? throw new InvalidOperationException("无法打开注册表 Run 键");

        if (enabled)
        {
            var path = ResolveExePath();
            // 引号包裹，避免路径含空格或中文时被拆成多个参数。
            key.SetValue(ValueName, "\"" + path + "\"");
        }
        else
        {
            key.DeleteValue(ValueName, throwOnMissingValue: false);
        }
    }

    /// <summary>使注册表状态与设置中的 AutoStart 一致。</summary>
    public void Apply(bool autoStart) => SetEnabled(autoStart);

    private static string ResolveExePath()
    {
        var path = Environment.ProcessPath;
        if (!string.IsNullOrEmpty(path) && File.Exists(path))
            return path;

        try
        {
            path = Environment.GetCommandLineArgs().FirstOrDefault();
            if (!string.IsNullOrEmpty(path))
            {
                path = Path.GetFullPath(path);
                if (File.Exists(path))
                    return path;
            }
        }
        catch
        {
            // ignore
        }

        throw new InvalidOperationException("无法定位 Prism.exe 路径，自启注册失败");
    }
}
