# G4 宿主联动兼容矩阵（机器验收清单）

> **状态**：实验性 / 默认关闭  
> **基线**：Windows 11 x64 build 22631；Directory Opus 13.23（若测 Opus）  
> **代码**：`ExplorerHostAdapter`、`DirectoryOpusHostAdapter`  
> **设置开关**（均默认 **false**，矩阵全部通过前不得默认开启）：
>
> - `ExplorerHostIntegrationEnabled`
> - `DirectoryOpusHostIntegrationEnabled`
>
> 总开关 `CurrentDirectorySearchEnabled` 仍默认 true；在 adapter 关闭时呼出只走全局搜索，不识别宿主。

任一失败项的产品行为都必须是：**立即回到全局搜索，清空 root，不复用上一次目录**，并在 UI 给出可见提示（识别失败类「无支持宿主」可静默）。

---

## 0. 公共前置

| 步骤 | 操作 | 期望 |
| --- | --- | --- |
| 0.1 | 确认设置中两个宿主开关均为关闭 | 从 Explorer / Opus 呼出 → 全局搜索，无范围标签 |
| 0.2 | 仅打开待测 adapter 开关，保存 | 下一次呼出才生效；不沿用旧 root |
| 0.3 | 关闭总开关 `Current目录搜索` | 即使 adapter 开着也不识别，Ctrl+G 提示已在设置中关闭 |

---

## 1. Windows Explorer（Shell COM）

**实现路径**：`Shell.Application` / shell windows 按 **捕获的 HWND** 匹配 → `Document.Folder.Self.Path`；导航用同一窗口 `Navigate` / `SelectItem`。  
**已知限制**：不把「新开 Explorer 窗口」当作导航成功。

**活动标签判据（实测确定，勿改回）**：取视图所属 `ShellTabWindowClass` 在兄弟中的
Z 序，最前者即活动标签。`IsWindowVisible` / `DWMWA_CLOAKED` **判不出来**——实测多标签
下所有视图窗口恒为 `visible=true`、`cloaked=0`、矩形相同；键盘焦点归属多数时候一致但
存在焦点滞留在非活动标签的瞬间，不可作主判据。详见 `ShellBrowserInterop` 注释。

**本机环境注意**：Directory Opus 已接管默认文件管理器，`explorer.exe <path>` 与
`explorer.exe shell:MyComputerFolder` 打开的都是 Opus Lister。测 Explorer 项必须用
**Win+E** 打开真资源管理器（窗口类 `CabinetWClass`）。另外 Opus 会把自己的 Lister 注册进
`ShellWindows`，但读 HWND/Document 会抛 `COMException`（已被 catch 兜住，属预期）。

| # | 场景 | 步骤 | 期望 | 通过 |
| --- | --- | --- | --- | --- |
| E1 | 单窗口取目录 | 打开一个 Explorer 到 `C:\Users\<you>\Documents`，从该窗口呼出 Prism | 范围标签「当前目录：Documents」，搜索仅限该树 | ☑ |
| E2 | 多窗口按 HWND | 再开一个 Explorer 到 `D:\`，分别从两个窗口呼出 | 每次 root 对应当前前台窗口，不串路径 | ☑ |
| E3 | 标签（若可测） | 同一窗口多个标签；能区分则取活动标签，不能则降级全局并提示 | 绝不显示错误标签的路径 | ☑ |
| E4 | 导航文件夹 | 在当前目录范围内选中一文件夹，触发宿主导航（adapter API / 后续 Ctrl+Enter） | **同一** Explorer 窗口导航到该文件夹；不新开无关窗口 | ☑ |
| E5 | 定位文件 | 选中一文件 RevealInHost | 同一窗口进入父目录并选中该文件；失败 → ActionFailed，保留 Prism 结果 | ☑ |
| E6 | 取消 / Esc | 呼出后 Esc 隐藏 | 宿主窗口状态不变 | ☑ |
| E7 | 宿主关闭竞态 | 呼出前关掉 Explorer，或捕获后立刻关 | 全局搜索；提示「原窗口已关闭」类文案；root 为空 | ☑ |
| E8 | 路径变化 | 在 Explorer 中进入子目录后再呼出 | 新 root 为新路径，不复用旧目录 | ☑ |
| E9 | 中文路径 | 目录名含中文 | 识别与搜索正常 | ☑ |
| E10 | 长路径 | 接近 MAX_PATH 或已启用长路径的深目录 | 可识别则限定；过长/过深 → 降级全局并提示 | ☑ |
| E11 | 访问拒绝 | 对无权限目录（或模拟） | AccessDenied → 全局 + 提示 | ☑ |
| E12 | 提权宿主 | 以管理员开 Explorer（若可），普通权限 Prism 呼出 | `HostElevated`，不控制，全局搜索 | ☑ |
| E13 | 开关关闭 | 关掉 Explorer 开关后再从 Explorer 呼出 | 不识别，全局，无错误噪声 | ☑ |
| E14 | 桌面 | 前台为桌面（Progman/WorkerW） | NotThisHost，全局 | ☑ |
| E15 | 非文件系统 | 打开「此电脑」/控制面板类虚拟文件夹 | FolderUnavailable → 全局 | ☑ |

**版本记录**：OS build 22631 / 验收人 jiazjzs / 日期 2026-08-06

---

## 2. Directory Opus 13.23（官方外部命令）

**实现路径**：进程名 `dopus` 识别主窗口；`dopusrt.exe /info <file>,paths` 取路径；`dopusrt /cmd Go <path> NEWTAB=no` 导航。参数一律 `ProcessStartInfo.ArgumentList`，**禁止** `cmd /c` 字符串拼接。  
**已知限制**：多 Lister/多面板时若无法消歧 → `FolderUnavailable`，不猜。本机无 `dopusrt` → 降级，不抛到 UI。

**`/info paths` 输出是 XML（实测确定，勿改回按行解析）**：形如
`<path active_lister="1" active_tab="1" lister="0x1f087e" side="1" tab="0x1a08d2">C:\Windows</path>`。
`lister` 即窗口句柄，可直接与捕获的 HWND 比对，无需靠标题猜；`side`（1 左 / 2 右）与
`active_tab` 用于双面板和多标签消歧。`dopusrt` 路径优先取运行中 `dopus.exe` 的同目录，
以支持非默认安装位置（本机在 `D:\效率工具\DOpus\`）。

| # | 场景 | 步骤 | 期望 | 通过 |
| --- | --- | --- | --- | --- |
| O1 | 识别 | 前台为 Opus 主窗口，打开 Opus 开关后呼出 | Detect 成功，能力位含 Read/Navigate/Reveal | ☑ |
| O2 | 取目录 | Lister 停在已知文件夹 | root 正确，范围标签显示叶名 | ☑ |
| O3 | 导航 / 定位 | NavigateFolder / RevealInHost | Opus 复用现有窗口（NEWTAB=no）打开路径；失败结构化 ActionFailed | ☑ |
| O4 | 多窗口 / 标签 | 两个 Lister 不同路径 | 能消歧则对；不能则全局，不串路径 | ☑ 双面板+标签已过 |
| O5 | 关闭竞态 | 呼出前后关闭 Opus | 全局 + HostGone/FolderUnavailable 提示 | ☑ |
| O6 | 中文 / 空格路径 | `C:\Users\...\项目 Docs` | `/info` 与 `Go` 参数不因空格/中文断裂 | ☑ |
| O7 | 长路径 | 深目录 | 与 Explorer 相同的本地校验与降级 | ☑ |
| O8 | dopusrt 缺失 | 临时重命名 dopusrt 或假路径 | FolderUnavailable / ActionFailed，全局可用 | ☑ |
| O9 | 非 0 退出 / 超时 | （可用测试桩或损坏命令） | 不抛异常到 UI，结构化失败 | ☑ |
| O10 | 提权 Opus | 管理员 Opus + 普通 Prism | HostElevated，不控制 | ☑ |
| O11 | 开关关闭 | DirectoryOpus 开关 off | 不识别，全局 | ☑ |

**版本记录**：Opus 13.23.0.0 / OS build 22631 / 验收人 jiazjzs / 日期 2026-08-06

---

## 3. SystemFileDialog（本轮不做）

| 项 | 状态 |
| --- | --- |
| UIA 打开/保存对话框 adapter | **未实现**，`DisabledHostAdapter` 占位 |
| 设置开关 | 无；矩阵通过前不增加默认开启项 |

---

## 4. 安全与回归抽检

| # | 检查 | 期望 | 通过 |
| --- | --- | --- | --- |
| S1 | 无 DLL 注入 | 代码路径仅 Shell COM / dopusrt / Win32 查询 | ☑ | 宿主路径仅 Shell COM / dopusrt / 只读 Win32 查询；`SetWindowsHookEx` 仅用于双击 Ctrl 热键（`WH_KEYBOARD_LL`，回调留在本进程，不注入 DLL），与宿主路径无关
| S2 | 无 LocalSystem Shell | 所有宿主调用在用户会话 WPF 进程 | ☑ | 服务只引用 `index_cache/indexer_runtime/log/logging`，不含 shell 模块；`ShellExecutor` 仅被 broker 侧 `apps/ipc/main` 引用
| S3 | 原始 selector/命令不进 indexer | 协议只有可选 `root` 字符串路径 | ☑ | `IndexerRequest::Search` 与宿主相关的字段只有 `root: Option<String>`，且经 `requested_root` 长度/空白校验
| S4 | adapter 关时全局搜索 | 文件搜索、历史、拼音不受影响 | ☑ | 两 adapter `Detect` 在 `!IsEnabled` 时立即返回 `AdapterDisabled`，不触发任何 COM/命令调用
| S5 | 质量门 | `dotnet test` / `dotnet build -c Release` 通过 | ☑ | Rust 145 / C# 84 全通过；clippy `-D warnings` 无告警；`cargo fmt --check` 干净

---

## 5. 签署

矩阵 **全部** 相关行通过后，才可将对应开关考虑改为默认 true（需另开产品决策，不在本文件自动生效）。

| Adapter | 全部通过 | 签署 | 日期 |
| --- | --- | --- | --- |
| Explorer | ☑ | jiazjzs（实测） | 2026-08-06 |
| Directory Opus 13.23 | ☑ | jiazjzs（实测） | 2026-08-06 |
