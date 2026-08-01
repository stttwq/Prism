using System.IO;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Prism.Models;

namespace Prism.Services;

/// <summary>
/// 设置读写 + 数据目录探测。
///
/// 数据目录策略（design.md / prd.md R7）：优先「安装目录\data」；若不可写
/// （如装在 Program Files）自动退回 %LocalAppData%\Prism，并暴露 <see cref="DataDir"/>
/// 供设置页显示实际位置。索引缓存后续步骤也放在同一目录。
/// </summary>
public sealed class SettingsStore
{
    private const string SettingsFileName = "settings.json";

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
        // 中文不转义为 \uXXXX，保持文件可读。
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        Converters = { new JsonStringEnumConverter() },
    };

    /// <summary>实际使用的数据目录（已确保可写）。</summary>
    public string DataDir { get; }

    /// <summary>设置文件完整路径。</summary>
    public string SettingsPath => Path.Combine(DataDir, SettingsFileName);

    public SettingsStore()
    {
        DataDir = ResolveDataDir();
        Directory.CreateDirectory(DataDir);
    }

    internal SettingsStore(string dataDir)
    {
        DataDir = dataDir;
        Directory.CreateDirectory(DataDir);
    }
    /// <summary>从磁盘加载设置；文件不存在或损坏时返回默认值。</summary>
    public Settings Load()
    {
        try
        {
            if (!File.Exists(SettingsPath))
                return Settings.Default;

            var json = File.ReadAllText(SettingsPath, Encoding.UTF8);
            var settings = JsonSerializer.Deserialize<Settings>(json, JsonOptions);
            if (settings is null || settings.SchemaVersion > Settings.CurrentSchemaVersion)
                return Settings.Default;
            return settings with
            {
                ComboHotkey = settings.ComboHotkey ?? Settings.Default.ComboHotkey,
                WebEngines = settings.WebEngines ?? [],
                ExcludedPaths = settings.ExcludedPaths ?? [],
            };
        }
        catch
        {
            // 损坏时恢复默认，不抛出（design.md 回滚策略）。
            return Settings.Default;
        }
    }

    /// <summary>将设置持久化到磁盘（原子写：先写临时文件再替换）。</summary>
    public void Save(Settings settings)
    {
        Validate(settings);
        settings = settings with { SchemaVersion = Settings.CurrentSchemaVersion };
        var tmp = SettingsPath + ".tmp";
        var json = JsonSerializer.Serialize(settings, JsonOptions);
        File.WriteAllText(tmp, json, Encoding.UTF8);
        File.Move(tmp, SettingsPath, overwrite: true);
    }

    private static void Validate(Settings settings)
    {
        if (settings.ExcludedPaths.Count > 32)
            throw new InvalidDataException("ExcludedPaths may contain at most 32 entries.");
        foreach (var path in settings.ExcludedPaths)
        {
            if (string.IsNullOrWhiteSpace(path)
                || path.Length > 1024
                || path.Any(char.IsControl)
                || !Path.IsPathFullyQualified(path))
            {
                throw new InvalidDataException("ExcludedPaths must contain bounded absolute paths.");
            }
        }
    }

    /// <summary>
    /// 探测数据目录：优先「安装目录\data」，不可写时退回 %LocalAppData%\Prism。
    /// 全程 Unicode API，中文路径无损（design.md 数据目录策略）。
    /// </summary>
    private static string ResolveDataDir()
    {
        var installData = Path.Combine(AppContext.BaseDirectory, "data");
        if (IsWritable(installData))
            return installData;

        return Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Prism");
    }

    private static bool IsWritable(string dir)
    {
        try
        {
            Directory.CreateDirectory(dir);
            var probe = Path.Combine(dir, ".write_probe");
            File.WriteAllText(probe, "");
            File.Delete(probe);
            return true;
        }
        catch
        {
            return false;
        }
    }
}
