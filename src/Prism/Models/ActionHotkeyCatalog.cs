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
    /// <param name="Label">中文动作名（与动作面板一致）。</param>
    /// <param name="Kinds">适用的目标类型。</param>
    public sealed record Entry(string Id, string Label, ActionHotkeyKinds Kinds);

    public static readonly IReadOnlyList<Entry> Entries =
    [
        new("open_folder", "打开所在文件夹", ActionHotkeyKinds.All),
        new("copy", "复制", ActionHotkeyKinds.FileAndFolder),
        new("cut", "剪切", ActionHotkeyKinds.FileAndFolder),
        new("copy_path", "复制路径至剪贴板", ActionHotkeyKinds.FileAndFolder),
        new("properties", "属性", ActionHotkeyKinds.FileAndFolder),
        new("open_with", "打开方式", ActionHotkeyKinds.File),
        new("rename", "重命名", ActionHotkeyKinds.FileAndFolder),
        new("copy_to", "复制到…", ActionHotkeyKinds.FileAndFolder),
        new("move_to", "移动到…", ActionHotkeyKinds.FileAndFolder),
        new("recycle", "移入回收站", ActionHotkeyKinds.FileAndFolder),
        new("delete_permanent", "永久删除", ActionHotkeyKinds.FileAndFolder),
        new("zip", "压缩为 ZIP", ActionHotkeyKinds.FileAndFolder),
        new("copy_app_path", "复制路径至剪贴板", ActionHotkeyKinds.App),
        new("app_properties", "属性", ActionHotkeyKinds.App),
        new("run_as_admin", "以管理员身份运行", ActionHotkeyKinds.App),
    ];

    /// <summary>按 id 查目录行；未知 id 返回 null。</summary>
    public static Entry? Find(string id) =>
        Entries.FirstOrDefault(e => e.Id == id);

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
