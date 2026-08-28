using System.Windows.Input;
using Prism.Models;
using Prism.Services;
using Xunit;

namespace Prism.Tests;

public sealed class CommandShortcutTableTests
{
    // ── FromCatalog：目录快照构建快捷键表 ─────────────────────────────

    [Fact]
    public void FromCatalog_Empty_ReturnsEmptyTable()
    {
        var table = CommandShortcutTable.FromCatalog(Array.Empty<CommandDescriptor>());
        Assert.False(table.TryMatch(Key.S, ModifierKeys.Control, out _));
    }

    [Fact]
    public void FromCatalog_CommandWithShortcut_ParsesIntoTable()
    {
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", "Ctrl+Shift+S", inputKind: "none"),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        Assert.True(table.TryMatch(Key.S, ModifierKeys.Control | ModifierKeys.Shift, out var entry));
        Assert.Equal("prism.settings.open", entry.CommandId);
        Assert.Equal("none", entry.InputKind);
    }

    [Fact]
    public void FromCatalog_CommandWithoutShortcut_NotInTable()
    {
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", null, inputKind: "none"),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        Assert.False(table.TryMatch(Key.S, ModifierKeys.Control | ModifierKeys.Shift, out _));
    }

    [Fact]
    public void FromCatalog_DisabledCommand_NotInTable()
    {
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", "Ctrl+Shift+S", inputKind: "none", enabled: false),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        Assert.False(table.TryMatch(Key.S, ModifierKeys.Control | ModifierKeys.Shift, out _));
    }

    [Fact]
    public void FromCatalog_ReservedKey_SilentlyDropped()
    {
        // Enter is reserved — should be dropped from the table
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", "Ctrl+Enter", inputKind: "none"),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        Assert.False(table.TryMatch(Key.Enter, ModifierKeys.Control, out _));
    }

    [Fact]
    public void FromCatalog_UnparseableCombo_SilentlyDropped()
    {
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", "garbage", inputKind: "none"),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        // Unparseable combo means no match
        Assert.False(table.TryMatch(Key.S, ModifierKeys.Control, out _));
    }

    [Fact]
    public void FromCatalog_DuplicateCombo_FirstWins()
    {
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", "Ctrl+Shift+S", inputKind: "none"),
            MakeCommand("prism.terminal.open", "Ctrl+Shift+S", inputKind: "none"),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        Assert.True(table.TryMatch(Key.S, ModifierKeys.Control | ModifierKeys.Shift, out var entry));
        Assert.Equal("prism.settings.open", entry.CommandId);
    }

    [Fact]
    public void FromCatalog_MultipleCommands_DistinctKeys()
    {
        var catalog = new[]
        {
            MakeCommand("prism.settings.open", "Ctrl+Shift+S", inputKind: "none"),
            MakeCommand("prism.terminal.open", "Ctrl+T", inputKind: "none"),
        };
        var table = CommandShortcutTable.FromCatalog(catalog);
        Assert.True(table.TryMatch(Key.S, ModifierKeys.Control | ModifierKeys.Shift, out var e1));
        Assert.Equal("prism.settings.open", e1.CommandId);
        Assert.True(table.TryMatch(Key.T, ModifierKeys.Control, out var e2));
        Assert.Equal("prism.terminal.open", e2.CommandId);
    }

    // ── ValidateConflict：与 ActionHotkeys 撞键检测 ──────────────────

    [Fact]
    public void ValidateConflict_NoConflict_ReturnsNull()
    {
        var actionHotkeys = new Dictionary<string, string>
        {
            ["copy"] = "Ctrl+Shift+C",
        };
        var result = CommandShortcutTable.ValidateConflict("Ctrl+Shift+S", actionHotkeys);
        Assert.Null(result);
    }

    [Fact]
    public void ValidateConflict_SameCombo_ReturnsError()
    {
        var actionHotkeys = new Dictionary<string, string>
        {
            ["copy"] = "Ctrl+Shift+S",
        };
        var result = CommandShortcutTable.ValidateConflict("Ctrl+Shift+S", actionHotkeys);
        Assert.NotNull(result);
        Assert.Contains("冲突", result);
    }

    [Fact]
    public void ValidateConflict_ReservedKey_ReturnsError()
    {
        var actionHotkeys = new Dictionary<string, string>();
        var result = CommandShortcutTable.ValidateConflict("Ctrl+Enter", actionHotkeys);
        Assert.NotNull(result);
        Assert.Contains("保留", result);
    }

    [Fact]
    public void ValidateConflict_Unparseable_ReturnsError()
    {
        var actionHotkeys = new Dictionary<string, string>();
        var result = CommandShortcutTable.ValidateConflict("garbage", actionHotkeys);
        Assert.NotNull(result);
        Assert.Contains("无法解析", result);
    }

    [Fact]
    public void ValidateConflict_EmptyCombo_ReturnsNull()
    {
        var actionHotkeys = new Dictionary<string, string>
        {
            ["copy"] = "Ctrl+Shift+C",
        };
        Assert.Null(CommandShortcutTable.ValidateConflict("", actionHotkeys));
        Assert.Null(CommandShortcutTable.ValidateConflict(null!, actionHotkeys));
    }

    [Fact]
    public void ValidateConflict_ExactModsMatch_NoFalsePositive()
    {
        // Ctrl+S should not conflict with Ctrl+Shift+S
        var actionHotkeys = new Dictionary<string, string>
        {
            ["copy"] = "Ctrl+Shift+S",
        };
        Assert.Null(CommandShortcutTable.ValidateConflict("Ctrl+S", actionHotkeys));
    }

    // ── CommandDescriptor.Parse 解析 shortcut_combo ──────────────────

    [Fact]
    public void Parse_ShortcutBinding_ExtractsShortcutCombo()
    {
        var json = """
        {
            "id": "prism.settings.open",
            "title": "打开设置",
            "owner": "ui",
            "trust": "builtin",
            "input": {"kind": "none", "required": false, "prompt": ""},
            "bindings": {
                "shortcut": {"priority": 0, "shortcut_combo": "Ctrl+Shift+S"}
            },
            "danger": "normal",
            "enabled": true
        }
        """;
        using var doc = System.Text.Json.JsonDocument.Parse(json);
        var desc = InvokeParse(doc.RootElement);
        Assert.NotNull(desc);
        Assert.NotNull(desc!.Bindings.Shortcut);
        Assert.Equal("Ctrl+Shift+S", desc.Bindings.Shortcut!.ShortcutCombo);
    }

    [Fact]
    public void Parse_NoShortcutCombo_DefaultsNull()
    {
        var json = """
        {
            "id": "prism.settings.open",
            "title": "打开设置",
            "owner": "ui",
            "trust": "builtin",
            "input": {"kind": "none", "required": false, "prompt": ""},
            "bindings": {},
            "danger": "normal",
            "enabled": true
        }
        """;
        using var doc = System.Text.Json.JsonDocument.Parse(json);
        var desc = InvokeParse(doc.RootElement);
        Assert.NotNull(desc);
        Assert.Null(desc!.Bindings.Shortcut);
    }

    // ── 辅助 ─────────────────────────────────────────────────────────

    private static CommandDescriptor MakeCommand(
        string id, string? shortcutCombo,
        string inputKind = "none", bool enabled = true)
    {
        var bindings = new CommandBindingsDto(
            null, null, null, null,
            shortcutCombo is not null
                ? new CommandBindingDto(0, "", null, Array.Empty<string>(), false, shortcutCombo)
                : null);
        return new CommandDescriptor(
            id, "Test", "", "", "broker", "builtin",
            Array.Empty<string>(),
            new CommandInputDto(inputKind, false, ""),
            bindings, "normal", enabled, true);
    }

    /// <summary>CommandDescriptor.Parse is internal — use reflection.</summary>
    private static CommandDescriptor? InvokeParse(System.Text.Json.JsonElement el)
    {
        var method = typeof(CommandDescriptor)
            .GetMethod("Parse", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Static);
        return (CommandDescriptor?)method!.Invoke(null, new object[] { el });
    }
}
