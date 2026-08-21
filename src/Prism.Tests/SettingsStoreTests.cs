using System.Text.Json;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

public sealed class SettingsStoreTests
{
    [Fact]
    public void MissingFieldsAndLegacySchemaUseSafeDefaults()
    {
        var directory = TestDirectory();
        try
        {
            File.WriteAllText(Path.Combine(directory, "settings.json"), "{}");
            var settings = new SettingsStore(directory).Load();
            Assert.Equal(0, settings.SchemaVersion);
            Assert.Empty(settings.ExcludedPaths);
            Assert.True(settings.HistoryEnabled);
            Assert.True(settings.PinyinEnabled);
            // 当前目录搜索总开关默认开启，旧设置文件缺字段时也一样。
            Assert.True(settings.CurrentDirectorySearchEnabled);
            // 两个宿主 adapter 开关默认关闭，旧文件缺字段也必须是 false。
            Assert.False(settings.ExplorerHostIntegrationEnabled);
            Assert.False(settings.DirectoryOpusHostIntegrationEnabled);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void HostIntegrationSwitchesRoundTripAndDefaultFalse()
    {
        var directory = TestDirectory();
        try
        {
            var store = new SettingsStore(directory);
            Assert.False(Settings.Default.ExplorerHostIntegrationEnabled);
            Assert.False(Settings.Default.DirectoryOpusHostIntegrationEnabled);

            store.Save(Settings.Default with
            {
                ExplorerHostIntegrationEnabled = true,
                DirectoryOpusHostIntegrationEnabled = true,
            });
            var loaded = store.Load();
            Assert.True(loaded.ExplorerHostIntegrationEnabled);
            Assert.True(loaded.DirectoryOpusHostIntegrationEnabled);

            using var document = JsonDocument.Parse(File.ReadAllText(store.SettingsPath));
            Assert.True(document.RootElement.GetProperty("ExplorerHostIntegrationEnabled").GetBoolean());
            Assert.True(document.RootElement.GetProperty("DirectoryOpusHostIntegrationEnabled").GetBoolean());
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void SaveWritesCurrentSchemaAndValidatesExclusions()
    {
        var directory = TestDirectory();
        try
        {
            var store = new SettingsStore(directory);
            store.Save(Settings.Default with { ExcludedPaths = [@"C:\work\build"] });
            using var document = JsonDocument.Parse(File.ReadAllText(store.SettingsPath));
            Assert.Equal(
                Settings.CurrentSchemaVersion,
                document.RootElement.GetProperty("SchemaVersion").GetInt32());
            Assert.True(document.RootElement.GetProperty("HistoryEnabled").GetBoolean());
            Assert.True(document.RootElement.GetProperty("PinyinEnabled").GetBoolean());
            Assert.True(document.RootElement
                .GetProperty("CurrentDirectorySearchEnabled").GetBoolean());

            Assert.Throws<InvalidDataException>(() =>
                store.Save(Settings.Default with { ExcludedPaths = ["relative"] }));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void FutureSchemaFallsBackWithoutReadingItsPayload()
    {
        var directory = TestDirectory();
        try
        {
            File.WriteAllText(
                Path.Combine(directory, "settings.json"),
                """{"SchemaVersion":999,"ComboHotkey":"Future"}""");
            var settings = new SettingsStore(directory).Load();
            Assert.Equal(Settings.CurrentSchemaVersion, settings.SchemaVersion);
            Assert.Equal(Settings.Default.ComboHotkey, settings.ComboHotkey);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void ExplicitNullCollectionsNormalizeToEmpty()
    {
        var directory = TestDirectory();
        try
        {
            File.WriteAllText(
                Path.Combine(directory, "settings.json"),
                """{"WebEngines":null,"ExcludedPaths":null,"ComboHotkey":null,"ActionHotkeys":null}""");
            var settings = new SettingsStore(directory).Load();
            Assert.Empty(settings.WebEngines);
            Assert.Empty(settings.ExcludedPaths);
            Assert.Empty(settings.ActionHotkeys);
            Assert.Equal(Settings.Default.ComboHotkey, settings.ComboHotkey);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void ActionHotkeysRoundTripAndLoadCleansHandEditedEntries()
    {
        var directory = TestDirectory();
        try
        {
            var store = new SettingsStore(directory);
            store.Save(Settings.Default with
            {
                ActionHotkeys = new Dictionary<string, string>
                {
                    ["open_folder"] = "Ctrl+Shift+O",
                    ["copy_path"] = "Ctrl+Shift+C",
                },
            });
            var loaded = store.Load();
            Assert.Equal("Ctrl+Shift+O", loaded.ActionHotkeys["open_folder"]);
            Assert.Equal("Ctrl+Shift+C", loaded.ActionHotkeys["copy_path"]);

            // 手改文件：未知 id / 无修饰键 / 保留键 / 乱序修饰——加载时清理，规范化。
            File.WriteAllText(
                Path.Combine(directory, "settings.json"),
                """{"SchemaVersion":1,"ActionHotkeys":{"nope":"Ctrl+K","copy":"C","cut":"Ctrl+G","zip":"ALT+ctrl+Z"}}""");
            var cleaned = store.Load();
            Assert.False(cleaned.ActionHotkeys.ContainsKey("nope"));
            Assert.False(cleaned.ActionHotkeys.ContainsKey("copy"));
            Assert.False(cleaned.ActionHotkeys.ContainsKey("cut"));
            Assert.Equal("Ctrl+Alt+Z", cleaned.ActionHotkeys["zip"]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void SaveRejectsInvalidActionHotkeyBindings()
    {
        var directory = TestDirectory();
        try
        {
            var store = new SettingsStore(directory);
            Assert.Throws<InvalidDataException>(() =>
                store.Save(Settings.Default with
                {
                    ActionHotkeys = new Dictionary<string, string> { ["copy"] = "Ctrl+G" },
                }));
            Assert.Throws<InvalidDataException>(() =>
                store.Save(Settings.Default with
                {
                    ActionHotkeys = new Dictionary<string, string>
                    {
                        ["copy"] = "Ctrl+K",
                        ["cut"] = "Ctrl+K",
                    },
                }));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static string TestDirectory()
    {
        var path = Path.Combine(Path.GetTempPath(), "prism-settings-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(path);
        return path;
    }
}
