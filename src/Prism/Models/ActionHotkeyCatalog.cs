namespace Prism.Models;

/// <summary>动作适用目标的位标志（与 broker TargetKind 对应）。</summary>
[Flags]
public enum ActionHotkeyKinds
{
    None = 0,
    File = 1,
    Folder = 2,
    App = 4,
    FileAndFolder = File | Folder,
    All = File | Folder | App,
}

/// <summary>
/// 动作快捷键目录（2026-08-21 设想）：broker 动作面板 15 个动作的前端静态镜像。
/// id/标签/适用类型与 prism-core actions.rs 的 allowed_actions 并集一致；
/// locate_app 不在任何面板列表，不收录。锚测试防止两边漂移。
/// </summary>
public static class ActionHotkeyCatalog
{
    /// <summary>目录行：一个可绑快捷键的动作。</summary>
    /// <param name="Id">broker 动作 id（稳定封闭枚举）。</param>
    /// <param name="Label">中文动作名（与动作面板一致；镜像 actions.rs label()）。</param>
    /// <param name="Kinds">适用的目标类型。</param>
    /// <param name="Category">功能分类（下拉分组用）。</param>
    /// <param name="IconGlyph">Segoe Fluent Icons 字形（镜像 actions.rs icon_glyph()）。</param>
    /// <param name="Description">一行短说明（区分 Label 相同的动作）。</param>
    public sealed record Entry(
        string Id,
        string Label,
        ActionHotkeyKinds Kinds,
        string Category,
        string IconGlyph,
        string Description);

    public static readonly IReadOnlyList<Entry> Entries =
    [
        // 顺序 = 设置页"添加动作"的默认选取序（测试与用户预期依赖）。
        // 下拉分组由 AvailableActionsView 的 PropertyGroupDescription("Category") 出，
        // 各组内仍按此序。字形/标签镜像 broker actions.rs 的 icon_glyph()/label()。
        new("open_folder", "打开所在文件夹", ActionHotkeyKinds.All, "文件操作", "\uE8DA", "在资源管理器中定位并选中"),
        new("copy", "复制", ActionHotkeyKinds.FileAndFolder, "剪贴板", "\uE8C8", "复制到剪贴板"),
        new("cut", "剪切", ActionHotkeyKinds.FileAndFolder, "剪贴板", "\uE8C6", "剪切到剪贴板"),
        new("copy_path", "复制路径至剪贴板", ActionHotkeyKinds.FileAndFolder, "剪贴板", "\uE8C8", "复制文件/文件夹完整路径"),
        new("properties", "属性", ActionHotkeyKinds.FileAndFolder, "属性", "\uE946", "文件/文件夹属性页"),
        new("open_with", "打开方式", ActionHotkeyKinds.File, "打开与运行", "\uE7B7", "选择程序打开文件"),
        new("rename", "重命名", ActionHotkeyKinds.FileAndFolder, "文件操作", "\uE8AC", "重命名文件或文件夹"),
        new("copy_to", "复制到…", ActionHotkeyKinds.FileAndFolder, "文件操作", "\uE8C8", "选择目标文件夹复制"),
        new("move_to", "移动到…", ActionHotkeyKinds.FileAndFolder, "文件操作", "\uE8C6", "选择目标文件夹移动"),
        new("recycle", "移入回收站", ActionHotkeyKinds.FileAndFolder, "删除与压缩", "\uE74D", "送回收站（可还原）"),
        new("delete_permanent", "永久删除", ActionHotkeyKinds.FileAndFolder, "删除与压缩", "\uE74D", "直接删除不可还原"),
        new("zip", "压缩为 ZIP", ActionHotkeyKinds.FileAndFolder, "删除与压缩", "\uE7F8", "压缩为 ZIP 归档"),
        new("copy_app_path", "复制路径至剪贴板", ActionHotkeyKinds.App, "剪贴板", "\uE8C8", "复制应用 exe 完整路径，自动解析快捷方式"),
        new("app_properties", "属性", ActionHotkeyKinds.App, "属性", "\uE946", "应用属性页"),
        new("run_as_admin", "以管理员身份运行", ActionHotkeyKinds.App, "打开与运行", "\uE7EF", "提权启动应用"),
    ];

    /// <summary>按 id 查目录行；未知 id 返回 null。</summary>
    public static Entry? Find(string id) =>
        Entries.FirstOrDefault(e => e.Id == id);

    /// <summary>适用类型的设置页文案（行模型与设置页共用，避免两处漂移）。</summary>
    public static string ScopeText(ActionHotkeyKinds kinds) => kinds switch
    {
        ActionHotkeyKinds.All => "文件 / 文件夹 / 应用",
        ActionHotkeyKinds.FileAndFolder => "文件 / 文件夹",
        ActionHotkeyKinds.File => "文件",
        ActionHotkeyKinds.App => "应用",
        _ => "",
    };

    /// <summary>该动作是否适用于此目标 kind（file/folder/app）。未知 kind 恒 false。</summary>
    public static bool AppliesTo(string id, string? kind)
    {
        var flag = KindFlag(kind);
        if (flag == ActionHotkeyKinds.None)
            return false;
        return Find(id)?.Kinds.HasFlag(flag) == true;
    }

    /// <summary>构造执行用 ActionItem（走与动作面板同一条 RunActionOnAsync 路径）。</summary>
    public static ActionItem ToActionItem(string id)
    {
        var entry = Find(id) ?? throw new ArgumentException($"未知动作 id：{id}", nameof(id));
        return new ActionItem(entry.Id, entry.Label, "", HasSubmenu: false, IsSectionHeader: false);
    }

    private static ActionHotkeyKinds KindFlag(string? kind) => kind switch
    {
        "file" => ActionHotkeyKinds.File,
        "folder" => ActionHotkeyKinds.Folder,
        "app" => ActionHotkeyKinds.App,
        _ => ActionHotkeyKinds.None,
    };
}
