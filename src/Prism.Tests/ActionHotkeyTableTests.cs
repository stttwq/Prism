using System.Windows.Input;
using Prism.Models;
using Prism.Services;
using Prism.ViewModels;
using Xunit;

namespace Prism.Tests;

public sealed class ActionHotkeyTableTests
{
    // ── 解析与规范化 ───────────────────────────────────────────────────

    [Theory]
    [InlineData("Ctrl+Shift+C", "Ctrl+Shift+C")]
    [InlineData("ALT+ctrl+C", "Ctrl+Alt+C")]        // 修饰键顺序规范化 + 大小写不敏感
    [InlineData("ctrl+1", "Ctrl+D1")]               // 单数字别名 → D1（ParseCombo 同规则）
    [InlineData("Win+F4", "Win+F4")]
    [InlineData("Ctrl+OemComma", "Ctrl+OemComma")]  // 录键框输出 Key 枚举名
    public void CanonicalizeNormalizesModifierOrderAndAliases(string raw, string canonical)
    {
        Assert.Equal(canonical, ActionHotkeyTable.Canonicalize(raw));
    }

    [Theory]
    [InlineData("C")]            // 无修饰键
    [InlineData("Ctrl")]         // 无主键
    [InlineData("Ctrl+")]        // 空主键
    [InlineData("Ctrl+Nothing")] // 主键不认识
    [InlineData("Ctrl+C+X")]     // 两个主键
    [InlineData("")]
    public void ParseRejectsMalformedCombos(string raw)
    {
        Assert.Null(ActionHotkeyTable.Parse(raw));
        Assert.Null(ActionHotkeyTable.Canonicalize(raw));
    }

    // ── 保留键 ─────────────────────────────────────────────────────────

    [Theory]
    [InlineData(Key.G, ModifierKeys.Control)]
    [InlineData(Key.G, ModifierKeys.Control | ModifierKeys.Shift)] // 现有逻辑只看 HasFlag(Ctrl)
    [InlineData(Key.D1, ModifierKeys.Control)]
    [InlineData(Key.NumPad1, ModifierKeys.Control | ModifierKeys.Alt)]
    [InlineData(Key.Enter, ModifierKeys.Control)]   // Ctrl+Enter = 原宿主定位，保持现状
    [InlineData(Key.Enter, ModifierKeys.None)]
    [InlineData(Key.Escape, ModifierKeys.Shift)]
    [InlineData(Key.Up, ModifierKeys.Control)]
    [InlineData(Key.Down, ModifierKeys.Alt)]
    [InlineData(Key.Left, ModifierKeys.None)]
    [InlineData(Key.Right, ModifierKeys.Control)]   // Right 带修饰本就留给文本框
    public void ReservedKeysCoverNavigationSet(Key key, ModifierKeys mods)
    {
        Assert.True(ActionHotkeyTable.IsReserved(key, mods), $"{key}+{mods} 应为保留键");
    }

    [Theory]
    [InlineData(Key.C, ModifierKeys.Control | ModifierKeys.Shift)]
    [InlineData(Key.B, ModifierKeys.Control)]
    [InlineData(Key.F4, ModifierKeys.Alt)]
    [InlineData(Key.O, ModifierKeys.Control | ModifierKeys.Alt | ModifierKeys.Shift)]
    public void NonNavigationCombosAreNotReserved(Key key, ModifierKeys mods)
    {
        Assert.False(ActionHotkeyTable.IsReserved(key, mods));
    }

    // ── 匹配 ───────────────────────────────────────────────────────────

    [Fact]
    public void TryMatchRequiresExactModifierSet()
    {
        var table = ActionHotkeyTable.FromSettings(new Dictionary<string, string>
        {
            ["open_folder"] = "Ctrl+Shift+O",
        });

        Assert.True(table.TryMatch(Key.O, ModifierKeys.Control | ModifierKeys.Shift, out var id));
        Assert.Equal("open_folder", id);
        // 多按/少按修饰键都不命中。
        Assert.False(table.TryMatch(Key.O, ModifierKeys.Control, out _));
        Assert.False(table.TryMatch(Key.O, ModifierKeys.Control | ModifierKeys.Shift | ModifierKeys.Alt, out _));
        Assert.False(table.TryMatch(Key.P, ModifierKeys.Control | ModifierKeys.Shift, out _));
    }

    [Fact]
    public void EmptyTableNeverMatches()
    {
        Assert.False(ActionHotkeyTable.Empty.TryMatch(Key.C, ModifierKeys.Control, out _));
    }

    [Fact]
    public void FromSettingsDropsUnknownMalformedReservedAndDuplicateEntries()
    {
        var table = ActionHotkeyTable.FromSettings(new Dictionary<string, string>
        {
            ["open_folder"] = "Ctrl+Shift+O",
            ["not_an_action"] = "Ctrl+Shift+X",   // 未知 id：丢弃
            ["copy_path"] = "C",                   // 无修饰键：丢弃
            ["copy"] = "Ctrl+G",                   // 保留键：丢弃
            ["cut"] = "Ctrl+Shift+O",              // 与 open_folder 撞键：目录顺序先到先得
        });

        Assert.True(table.TryMatch(Key.O, ModifierKeys.Control | ModifierKeys.Shift, out var id));
        Assert.Equal("open_folder", id);
        Assert.False(table.TryMatch(Key.G, ModifierKeys.Control, out _));
        Assert.False(table.TryMatch(Key.C, ModifierKeys.None, out _));
    }

    // ── 保存校验 ───────────────────────────────────────────────────────

    [Fact]
    public void ValidateBindingsAcceptsCleanTable()
    {
        var error = ActionHotkeyTable.ValidateBindings(new Dictionary<string, string>
        {
            ["open_folder"] = "Ctrl+Shift+O",
            ["copy_path"] = "Ctrl+Shift+C",
            ["rename"] = "", // 空值 = 未绑定，合法
        });
        Assert.Null(error);
    }

    [Fact]
    public void ValidateBindingsRejectsDuplicateCombo()
    {
        var error = ActionHotkeyTable.ValidateBindings(new Dictionary<string, string>
        {
            ["open_folder"] = "Ctrl+Shift+O",
            ["copy"] = "Ctrl+Shift+O",
        });
        Assert.NotNull(error);
        Assert.Contains("同一个组合键", error);
    }

    [Fact]
    public void ValidateBindingsRejectsReservedAndMalformedAndUnknown()
    {
        Assert.Contains("保留给导航",
            ActionHotkeyTable.ValidateBindings(new Dictionary<string, string> { ["copy"] = "Ctrl+G" }));
        Assert.Contains("无法解析",
            ActionHotkeyTable.ValidateBindings(new Dictionary<string, string> { ["copy"] = "C" }));
        Assert.Contains("未知动作",
            ActionHotkeyTable.ValidateBindings(new Dictionary<string, string> { ["nope"] = "Ctrl+K" }));
    }

    // ── 动作目录锚（与 broker actions.rs allowlist 并集对齐）───────────

    [Fact]
    public void CatalogMatchesBrokerActionAllowlistUnion()
    {
        // broker allowed_actions(File ∪ Directory ∪ Application) 的 id 并集（去掉
        // 不在任何面板列表的 locate_app）。此锚防止前端目录与后端枚举漂移。
        var expected = new[]
        {
            "open_folder", "copy", "cut", "copy_path", "properties", "open_with",
            "rename", "copy_to", "move_to", "recycle", "delete_permanent", "zip",
            "copy_app_path", "app_properties", "run_as_admin",
        };
        var actual = ActionHotkeyCatalog.Entries.Select(e => e.Id).ToArray();
        Assert.Equal(expected.OrderBy(x => x), actual.OrderBy(x => x));
        Assert.Equal(expected.Length, actual.Distinct().Count());
    }

    [Fact]
    public void CatalogKindRulesMirrorBrokerAllowlist()
    {
        Assert.True(ActionHotkeyCatalog.AppliesTo("open_folder", "file"));
        Assert.True(ActionHotkeyCatalog.AppliesTo("open_folder", "folder"));
        Assert.True(ActionHotkeyCatalog.AppliesTo("open_folder", "app"));
        // open_with 仅文件；应用专属动作对文件不适用。
        Assert.True(ActionHotkeyCatalog.AppliesTo("open_with", "file"));
        Assert.False(ActionHotkeyCatalog.AppliesTo("open_with", "folder"));
        Assert.False(ActionHotkeyCatalog.AppliesTo("run_as_admin", "file"));
        Assert.True(ActionHotkeyCatalog.AppliesTo("run_as_admin", "app"));
        Assert.False(ActionHotkeyCatalog.AppliesTo("copy_app_path", "folder"));
        // 窗口/网页/more 行永远不适用。
        Assert.False(ActionHotkeyCatalog.AppliesTo("copy", "window"));
        Assert.False(ActionHotkeyCatalog.AppliesTo("copy", "web"));
        Assert.False(ActionHotkeyCatalog.AppliesTo("copy", "more"));
        Assert.False(ActionHotkeyCatalog.AppliesTo("copy", null));
    }

    [Fact]
    public void ToActionItemProducesExecutableAction()
    {
        var item = ActionHotkeyCatalog.ToActionItem("copy_path");
        Assert.Equal("copy_path", item.Id);
        Assert.Equal("复制路径至剪贴板", item.Label);
        Assert.False(item.IsSectionHeader);
        Assert.False(item.HasSubmenu);
        Assert.Throws<ArgumentException>(() => ActionHotkeyCatalog.ToActionItem("nope"));
    }
}

/// <summary>设置页保存路径的动作快捷键校验（VM 层拦截在 Store 校验之前）。</summary>
public sealed class SettingsViewModelActionHotkeyTests
{
    [Fact]
    public void SaveBlocksDuplicateAndReservedCombosBeforeTouchingDisk()
    {
        var directory = TestDir();
        try
        {
            var store = new SettingsStore(directory);
            store.Save(Settings.Default);
            var before = File.ReadAllText(store.SettingsPath);

            var vm = new SettingsViewModel(store, new AutoStartService());
            Assert.Equal(ActionHotkeyCatalog.Entries.Count, vm.ActionHotkeys.Count);

            // 撞键：两个动作同一个组合键。
            vm.ActionHotkeys.Single(r => r.Id == "open_folder").Value = "Ctrl+Shift+O";
            vm.ActionHotkeys.Single(r => r.Id == "copy").Value = "Ctrl+Shift+O";
            vm.SaveCommand.Execute(null);
            Assert.Contains("同一个组合键", vm.StatusMessage);
            Assert.True(vm.IsQuickAccessTab);
            Assert.Equal(before, File.ReadAllText(store.SettingsPath));

            // 保留键。
            vm.ActionHotkeys.Single(r => r.Id == "copy").Value = "Ctrl+G";
            vm.SaveCommand.Execute(null);
            Assert.Contains("保留给导航", vm.StatusMessage);
            Assert.Equal(before, File.ReadAllText(store.SettingsPath));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void SavePersistsCanonicalCombosAndClearMeansUnbound()
    {
        var directory = TestDir();
        Settings? applied = null;
        try
        {
            var store = new SettingsStore(directory);
            store.Save(Settings.Default with
            {
                ActionHotkeys = new Dictionary<string, string> { ["zip"] = "Ctrl+Shift+Z" },
            });

            var vm = new SettingsViewModel(store, new AutoStartService(), onApplied: s => applied = s);
            // 旧绑定预填进行；改写 + 新增一个，其余留空。
            Assert.Equal("Ctrl+Shift+Z", vm.ActionHotkeys.Single(r => r.Id == "zip").Value);
            vm.ActionHotkeys.Single(r => r.Id == "open_folder").Value = "Ctrl+Shift+O";
            vm.ActionHotkeys.Single(r => r.Id == "run_as_admin").Value = "Ctrl+Alt+Shift+A";
            vm.SaveCommand.Execute(null);

            Assert.Equal("已保存", vm.StatusMessage);
            var loaded = store.Load();
            Assert.Equal(3, loaded.ActionHotkeys.Count);
            Assert.Equal("Ctrl+Shift+O", loaded.ActionHotkeys["open_folder"]);
            Assert.Equal("Ctrl+Shift+Z", loaded.ActionHotkeys["zip"]);
            Assert.Equal("Ctrl+Alt+Shift+A", loaded.ActionHotkeys["run_as_admin"]);
            Assert.Equal(3, applied!.ActionHotkeys.Count);

            // 清空即解绑：落盘后字典只剩其余两键。
            vm.ActionHotkeys.Single(r => r.Id == "zip").Value = "";
            vm.ClearActionHotkeyCommand.Execute(vm.ActionHotkeys.Single(r => r.Id == "zip"));
            vm.SaveCommand.Execute(null);
            var reloaded = store.Load();
            Assert.Equal(2, reloaded.ActionHotkeys.Count);
            Assert.False(reloaded.ActionHotkeys.ContainsKey("zip"));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static string TestDir()
    {
        var path = Path.Combine(Path.GetTempPath(), "prism-actionhotkey-vm-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(path);
        return path;
    }
}
