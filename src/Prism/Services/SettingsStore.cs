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
    /// <summary>
    /// 从磁盘加载设置；文件不存在或 JSON 损坏时返回默认值（design.md 回滚策略）。
    /// 读取 IO 失败（杀软/备份软件短暂锁文件的共享冲突）重试一次后上抛——
    /// 那不是损坏，回退默认值会让启动路径按默认注销自启注册表、设置页保存
    /// 用默认值覆盖完好文件。
    /// </summary>
    public Settings Load()
    {
        string json;
        for (var attempt = 1; ; attempt++)
        {
            try
            {
                if (!File.Exists(SettingsPath))
                    return Settings.Default;
                json = File.ReadAllText(SettingsPath, Encoding.UTF8);
                break;
            }
            catch (IOException)
            {
                if (attempt == 2) throw;
                Thread.Sleep(50);
            }
        }

        try
        {
            return LoadFromJson(json);
        }
        catch (JsonException)
        {
            // 真损坏：回滚默认，不抛出（design.md 回滚策略）。
            return Settings.Default;
        }
    }

    private Settings LoadFromJson(string json)
    {
        var settings = JsonSerializer.Deserialize<Settings>(json, JsonOptions);
        if (settings is null || settings.SchemaVersion > Settings.CurrentSchemaVersion)
            return Settings.Default;
        var actionHotkeys = NormalizeActionHotkeys(settings.ActionHotkeys);
        return settings with
        {
            ComboHotkey = settings.ComboHotkey ?? Settings.Default.ComboHotkey,
            WebEngines = settings.WebEngines ?? [],
            ExcludedPaths = settings.ExcludedPaths ?? [],
            ZipProgram = string.IsNullOrWhiteSpace(settings.ZipProgram) ? null : settings.ZipProgram,
            ActionHotkeys = actionHotkeys,
            StagingCapacity = settings.StagingCapacity is < 1 or > 32
                ? 5
                : settings.StagingCapacity,
            // 缺字段（旧文件）→ 默认 Ctrl+D；有值但非法（手改）→ 置空禁用。
            StagingAddHotkey = settings.StagingAddHotkey is null
                ? Settings.Default.StagingAddHotkey
                : NormalizeStagingAddHotkey(settings.StagingAddHotkey, actionHotkeys),
        };
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
        if (settings.ActionHotkeys.Count > ActionHotkeyCatalog.Entries.Count)
            throw new InvalidDataException(
                $"ActionHotkeys may contain at most {ActionHotkeyCatalog.Entries.Count} entries.");
        var hotkeyError = ActionHotkeyTable.ValidateBindings(settings.ActionHotkeys);
        if (hotkeyError is not null)
            throw new InvalidDataException("ActionHotkeys: " + hotkeyError);

        // 暂存区（2026-08-22）：容量界 + 快捷键三查（可解析/非保留/不与动作快捷键撞键）。
        if (settings.StagingCapacity is < 1 or > 32)
            throw new InvalidDataException("StagingCapacity must be between 1 and 32.");
        var stagingHotkeyError = ValidateStagingAddHotkey(settings.StagingAddHotkey, settings.ActionHotkeys);
        if (stagingHotkeyError is not null)
            throw new InvalidDataException("StagingAddHotkey: " + stagingHotkeyError);
    }

    /// <summary>
    /// 校验「加入暂存区」快捷键：空 = 禁用（合法）；否则须可解析、不落保留集、
    /// 不与任何动作快捷键同组合。返回错误文案；合法返回 null。
    /// </summary>
    private static string? ValidateStagingAddHotkey(string? combo, Dictionary<string, string> actionHotkeys)
    {
        if (string.IsNullOrWhiteSpace(combo)) return null;
        var parsed = ActionHotkeyTable.Parse(combo);
        if (parsed is null)
            return "无法解析（需至少一个修饰键 + 主键）。";
        var (key, mods) = parsed.Value;
        if (ActionHotkeyTable.IsReserved(key, mods))
            return $"{ActionHotkeyTable.Canonicalize(combo)} 保留给导航，请换一个。";
        foreach (var (actionId, actionCombo) in actionHotkeys)
        {
            if (string.IsNullOrWhiteSpace(actionCombo)) continue;
            var other = ActionHotkeyTable.Parse(actionCombo);
            if (other is not null && other.Value.Key == key && other.Value.Mods == mods)
                return $"{ActionHotkeyTable.Canonicalize(combo)} 与动作「{actionId}」的快捷键冲突。";
        }
        return null;
    }

    /// <summary>
    /// 加载时清理手改文件：无法解析/落保留集/与动作快捷键撞键的「加入暂存区」
    /// 绑定静默置空（禁用），与 NormalizeActionHotkeys 同纪律。
    /// </summary>
    private static string NormalizeStagingAddHotkey(string? combo, Dictionary<string, string> actionHotkeys)
    {
        if (string.IsNullOrWhiteSpace(combo)) return "";
        return ValidateStagingAddHotkey(combo, actionHotkeys) is null
            ? ActionHotkeyTable.Canonicalize(combo)!
            : "";
    }

    /// <summary>
    /// 2026-08-21 动作快捷键：加载时清理手改文件——未知动作 id、无法解析的组合键、
    /// 保留键占用静默丢弃，其余规范化为固定顺序（Ctrl+Alt+Shift+Win+主键）。
    /// </summary>
    private static Dictionary<string, string> NormalizeActionHotkeys(Dictionary<string, string>? raw)
    {
        if (raw is null || raw.Count == 0)
            return [];
        var clean = new Dictionary<string, string>(raw.Count, StringComparer.Ordinal);
        foreach (var (id, combo) in raw)
        {
            if (ActionHotkeyCatalog.Find(id) is null)
                continue;
            var parsed = ActionHotkeyTable.Parse(combo);
            if (parsed is null || ActionHotkeyTable.IsReserved(parsed.Value.Key, parsed.Value.Mods))
                continue;
            clean[id] = ActionHotkeyTable.Canonicalize(combo)!;
        }
        return clean;
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
