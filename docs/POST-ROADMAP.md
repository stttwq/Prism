# Prism 路线图完成后优化方向（已废止 / 仅供审计）

> **状态：已被取代，其中多个方向已被明确排除。** 本文原样保留供审计对照。
> 收敛稿是 [`POST-ROADMAP-REVISED.md`](./POST-ROADMAP-REVISED.md)，
> 现行实施入口是 [`PRISM-COMPREHENSIVE-PLAN.md`](./PRISM-COMPREHENSIVE-PLAN.md)。
> 制定日期：2026-07-27；废止标注：2026-08-09
>
> | 本文方向 | 最终决定 |
> | --- | --- |
> | 剪贴板历史（`c ` / `c:` 前缀、图片缩略图） | **彻底排除**，普通复制/剪切/复制路径动作保留 |
> | 文件预览（Space 展开、图片/文本/PDF） | **彻底排除** |
> | 收藏夹 `pins.json` + `pin_score` | **彻底排除** |
> | `size:` / `datemodified:` 过滤 | **彻底排除**，只保留 `ext:` / `path:`（G7，未启动） |
> | JSON 配置动作、DLL 动态加载 | **彻底排除**，只做内置动作（G6，未启动） |
> | 内容索引、HTTP API、跨设备、内嵌搜索框 | **彻底排除** |
> | 窗口切换（`>` 前缀、实时枚举、不常驻 HWND） | **保留**，即 G5（未启动）。只做搜索与切换，不做关闭/最小化/置顶/进程管理 |
> | 当前文件夹搜索 `root: Option<String>` | **已交付**（G4）。但**不是**按本文所说给 `NodeSlot` 加子树索引——12B 布局未动，实际走「名称候选 → `parent_record` 祖先链验证 → 全局 Top-K」 |
> | 动作扩展「复制到/移动到」子流程 | **保留**，即 G6（未启动） |
> | 高级语法在 `ipc.rs` 拆 `(filters, name_query)` | 方向成立，协议 `filters` 字段已在 G1 预留、G3 开始使用 |
> | 内存追加预算「合计 < 1.5MB，终态 43–45MB」 | 口径已废弃。现行门槛是三进程私有工作集 ≤100MiB；2026-08-01 实测 55.2 MiB |

---

## 一、已锁定的设计决策

| 决策 | 结论 | 影响 |
| --- | --- | --- |
| "复制到…"UI | 先做最近文件夹列表（从使用历史取），底部"搜索更多…"入口，后续扩展子搜索 | 动作扩展工程量可控，不需要 VM 嵌套状态 |
| 空输入显示 | 显示最近窗口（5-10 个，从窗口切换历史取） | 剪贴板需要单独触发 |
| 剪贴板触发 | 前缀 `c ` / `c:` | 和 web 搜索引擎前缀风格一致 |
| 收藏夹时机 | P3，排序层预留接口 | P0 排序加权设计时预留 `pin_score` 字段 |
| 剪贴板图片 | 只存文本 + 图片缩略图（96×96） | 内存 < 1MB |
| 剪贴板持久化 | 默认不持久化，设置加开关 | |
| 剪贴板实现位置 | C# 前端（`AddClipboardFormatListener`） | 不走 IPC，省管道开销 |
| 窗口搜索触发 | 前缀 `>` 全量窗口搜索；空输入显示最近窗口历史 | |
| 高级语法解析 | `ipc.rs` 拆 query 为 `(filters, name_query)` | 后端解析，前端不改 |
| 文件大小过滤 | 不存索引，对 top-K 结果 `GetFileSize` 后过滤 | 零内存增量 |
| 文件预览 MVP | 图片 + 文本/代码，Space 键展开 | WPF 原生支持 |
| 当前文件夹搜索 | `Request::Search` 加 `root: Option<String>` | 需要子树遍历，P0 后加 NodeSlot 子树索引 |

---

## 二、功能方向详细设计

### S 级：剪贴板历史 + 当前文件夹搜索

#### 剪贴板历史

**实现位置**：C# 前端（Prism.exe），不走 IPC。

**钩子方式**：`AddClipboardFormatListener`（比 `SetWindowsHookEx` 轻量，不需要全局钩子权限），注册在 WPF 窗口。

**触发方式**：输入前缀 `c ` 或 `c:` 切到剪贴板模式。空输入时显示最近窗口（不显示剪贴板）。

**存储**：

- 文本：`CF_UNICODETEXT` / `CF_TEXT`，存原文，上限 200 条
- 图片：`CF_DIB` / `CF_BITMAP`，生成 96×96 缩略图后丢弃原图，上限 20 张
- 持久化：默认不持久化，设置加开关
- 内存预算：< 1MB

**搜索**：文本做子串匹配，图片按时间排序显示。

#### 当前文件夹搜索

**协议变更**：`ipc.rs` 的 `Request::Search` 加 `root: Option<String>`。

**后端实现**：需要给 `NodeSlot` 加子树遍历能力（`first_child` 或 siblings 链）。P0 后单独做，不影响现有 NodeSlot 12B 布局（flags 仍有空位可复用或扩展字段）。

**前端触发**：Milestone A 的 UIA 检测拿到资源管理器/对话框当前路径时，自动作为 `root` 参数传递。

---

### A 级：动作扩展 + 窗口管理

#### 动作扩展

**第一层：内置动作扩展**（直接在 `actions.rs::list_actions` 加分支）

| 动作 | 实现 | 工作量 |
| --- | --- | --- |
| 复制到… | 最近文件夹列表（从使用历史取）+ 底部"搜索更多…"入口 | 3h |
| 移动到… | 复用"复制到…"的子流程 | 1h |
| 重命名 | 新 IPC 消息，前端弹出内联编辑框 | 3h |
| 用…打开 | `ShellExecuteW` 用用户选的程序 | 2h |
| 删除到回收站 | `SHFileOperationW` 或 `IFileOperation` | 1h |
| 属性 | `ShellExecuteW("properties")` | 30min |
| 压缩 | 调 7z.exe 或 Windows 内置 `ComShell` | 2h |

**"复制到…"子流程**：先做最近文件夹列表（从使用历史取最近访问的文件夹，用户选一个即执行）。列表底部放"搜索更多…"入口，后续扩展为嵌套子搜索（输入框变文件夹搜索模式）。

**第二层：JSON 配置动作**（路线图 P4 的轻方案）

```json
{
  "custom_actions": [
    { "id": "open_vscode", "label": "用 VS Code 打开", "command": "code", "args": "{path}" },
    { "id": "open_terminal", "label": "在此打开终端", "command": "wt", "args": "-d \"{dir}\"" }
  ]
}
```

**第三层：DLL 动态加载**（最远期，不在近期范围）。

#### 窗口管理

**复用现有代码**：`ForceActivate`（`SearchWindow.xaml.cs:177-224`）的完整抢前台逻辑 + P/Invoke。

**需要新写**：

1. `EnumWindows` + `GetWindowTextW` + `IsWindowVisible` + `GetWindowThreadProcessId` 枚举窗口（过滤不可见/无标题/后台进程）
2. 对窗口标题做子串匹配
3. `SetForegroundWindow` 切换（已有 P/Invoke）

**内存**：窗口列表搜索时实时枚举，不常驻。几百个窗口 × 200B 标题 = 几十 KB，搜索完即丢。

**触发方式**：

- 空输入时：显示最近使用的窗口（5-10 个，从窗口切换历史取）
- 前缀 `>` 时：全量窗口搜索

---

### B 级：高级搜索语法 + 文件预览 + 收藏夹

#### 高级搜索语法

**解析位置**：`ipc.rs`（或新增 `parser.rs`），把 query 拆成 `(filters, name_query)`。

```
"hello ext:pdf size:>1m" →
  filters: [{type: ext, value: "pdf"}, {type: size, op: >, value: 1m}]
  name_query: "hello"
```

**语法规则**：特定关键字开头的 token 才是过滤器（`ext:`/`size:`/`path:`/`datemodified:`），普通文本永远是文件名搜索。

**文件大小**：不存索引（避免 +8MB），对 top-K 结果 `GetFileSize` 后过滤。

**日期过滤**：支持"今天/本周/本月"，计算时间戳范围后 `GetFileAttributesExW` 验证。

#### 文件预览

**MVP 范围**：图片 + 文本/代码。

| 文件类型 | 预览方式 | 复杂度 |
| --- | --- | --- |
| 图片 (png/jpg/gif/bmp/webp) | `BitmapImage` + `Image` 控件 | 低 |
| 文本/代码 (txt/md/json/xml/cs/rs) | `TextBox`（只读）或 syntax highlight | 低-中 |
| PDF | 引入 `PdfViewer` 或 WebView2 | 中（后续） |
| 视频/音频 | `MediaElement` 控件 | 中（后续） |
| Office (docx/xlsx) | WebView2 渲染或提取文本 | 高（后续） |

**交互**：选中文件后按 Space 键展开预览面板。

**内存注意**：图片预览限制尺寸，`BitmapImage.DecodePixelWidth = 300` + `CacheOption.OnLoad`，原图立即释放。

#### 收藏夹

**实现**：`pins.json` 存路径列表，排序时最高优先级（比使用历史还高）。

**UI**：

- 收藏：选中后 `Ctrl+S` 或右键菜单"添加到收藏"
- 管理：设置页收藏夹管理列表（拖拽排序、删除）
- 失效处理：搜索时 `Path.Exists` 检查，失效的灰显或自动移除

**与 P0 的关系**：排序层预留 `pin_score` 接口，P3 时实现。

---

## 三、暂不考虑的方向

| 方向 | 原因 |
| --- | --- |
| 内容索引（搜文件内容） | 内存不可行（20 万小文本 trigram 约 2-3GB）；"top-K 后 mmap 扫"的退化方案延迟不可控 |
| HTTP API / 跨设备 | 锦上添花，优先级低 |
| 流式渲染（搜索结果逐条推送） | 前端增量缓存已覆盖连续打字场景；单次大查询的延迟后续再优化 |
| 内嵌搜索框 UI（Milestone D） | 跨进程 UI 渲染月级工程， indefinitely postponed |
| 片段展开 / 文本模板 | 和插件框架同体系，P4 后再评估 |

---

## 四、建议实现顺序

```
P0–P3（已有路线图）
    ↓
S1  窗口管理            1-2 天  （枚举窗口 + 搜索 + ForceActivate 复用）
S2  剪贴板历史           1-2 天  （AddClipboardFormatListener + 存储 + c 前缀）
S3  当前文件夹搜索       1 天    （NodeSlot 子树索引 + root 参数）
A1  动作扩展（内置）      3-4 天  （复制到/移动到/删除/属性/用…打开）
A2  动作子流程            1-2 天  （最近文件夹列表 + "搜索更多…"入口）
B1  收藏夹               0.5 天  （pins.json + 排序最高优先级）
B2  高级搜索语法         2-3 天  （解析器 + ext/size/date 过滤）
B3  文件预览             2-3 天  （图片+文本，Space 展开面板）
```

**S1+S2 约 3-4 天可上线**，投入产出比最高。窗口管理 + 剪贴板会让 Prism 从"搜索工具"变成"离不开的常驻工具"。

---

## 五、内存预算追加

| 新增组件 | 内存增量 |
| --- | --- |
| 剪贴板历史（200 文本 + 20 缩略图） | < 1MB |
| 窗口列表（实时枚举，非常驻） | ~几十 KB（瞬态） |
| 收藏夹（pins.json 加载到内存） | < 10KB |
| NodeSlot 子树索引 | 0（复用现有 flags 空位） |
| **合计** | **< 1.5MB** |

追加后 Prism 总内存约 **~43-45MB**，仍在可接受范围内。
