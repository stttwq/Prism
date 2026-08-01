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
                """{"WebEngines":null,"ExcludedPaths":null,"ComboHotkey":null}""");
            var settings = new SettingsStore(directory).Load();
            Assert.Empty(settings.WebEngines);
            Assert.Empty(settings.ExcludedPaths);
            Assert.Equal(Settings.Default.ComboHotkey, settings.ComboHotkey);
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
