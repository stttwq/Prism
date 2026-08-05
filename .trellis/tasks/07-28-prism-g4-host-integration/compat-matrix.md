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
**已知限制**：Windows 11 多标签若 COM 无法区分活动标签 → 返回 `FolderUnavailable`（**禁止**猜错标签路径）。不把「新开 Explorer 窗口」当作导航成功。

| # | 场景 | 步骤 | 期望 | 通过 |
| --- | --- | --- | --- | --- |
| E1 | 单窗口取目录 | 打开一个 Explorer 到 `C:\Users\<you>\Documents`，从该窗口呼出 Prism | 范围标签「当前目录：Documents」，搜索仅限该树 | ☐ |
| E2 | 多窗口按 HWND | 再开一个 Explorer 到 `D:\`，分别从两个窗口呼出 | 每次 root 对应当前前台窗口，不串路径 | ☐ |
| E3 | 标签（若可测） | 同一窗口多个标签；能区分则取活动标签，不能则降级全局并提示 | 绝不显示错误标签的路径 | ☐ |
| E4 | 导航文件夹 | 在当前目录范围内选中一文件夹，触发宿主导航（adapter API / 后续 Ctrl+Enter） | **同一** Explorer 窗口导航到该文件夹；不新开无关窗口 | ☐ |
| E5 | 定位文件 | 选中一文件 RevealInHost | 同一窗口进入父目录并选中该文件；失败 → ActionFailed，保留 Prism 结果 | ☐ |
| E6 | 取消 / Esc | 呼出后 Esc 隐藏 | 宿主窗口状态不变 | ☐ |
| E7 | 宿主关闭竞态 | 呼出前关掉 Explorer，或捕获后立刻关 | 全局搜索；提示「原窗口已关闭」类文案；root 为空 | ☐ |
| E8 | 路径变化 | 在 Explorer 中进入子目录后再呼出 | 新 root 为新路径，不复用旧目录 | ☐ |
| E9 | 中文路径 | 目录名含中文 | 识别与搜索正常 | ☐ |
| E10 | 长路径 | 接近 MAX_PATH 或已启用长路径的深目录 | 可识别则限定；过长/过深 → 降级全局并提示 | ☐ |
| E11 | 访问拒绝 | 对无权限目录（或模拟） | AccessDenied → 全局 + 提示 | ☐ |
| E12 | 提权宿主 | 以管理员开 Explorer（若可），普通权限 Prism 呼出 | `HostElevated`，不控制，全局搜索 | ☐ |
| E13 | 开关关闭 | 关掉 Explorer 开关后再从 Explorer 呼出 | 不识别，全局，无错误噪声 | ☐ |
| E14 | 桌面 | 前台为桌面（Progman/WorkerW） | NotThisHost，全局 | ☐ |
| E15 | 非文件系统 | 打开「此电脑」/控制面板类虚拟文件夹 | FolderUnavailable → 全局 | ☐ |

**版本记录**：OS build ______ / 验收人 ______ / 日期 ______

---

## 2. Directory Opus 13.23（官方外部命令）

**实现路径**：进程名 `dopus` 识别主窗口；`dopusrt.exe /info <file>,paths` 取路径；`dopusrt /cmd Go <path> NEWTAB=no` 导航。参数一律 `ProcessStartInfo.ArgumentList`，**禁止** `cmd /c` 字符串拼接。  
**已知限制**：多 Lister 时若窗口标题无法消歧 → `FolderUnavailable`，不猜。本机无 `dopusrt` → 降级，不抛到 UI。

| # | 场景 | 步骤 | 期望 | 通过 |
| --- | --- | --- | --- | --- |
| O1 | 识别 | 前台为 Opus 主窗口，打开 Opus 开关后呼出 | Detect 成功，能力位含 Read/Navigate/Reveal | ☐ |
| O2 | 取目录 | Lister 停在已知文件夹 | root 正确，范围标签显示叶名 | ☐ |
| O3 | 导航 / 定位 | NavigateFolder / RevealInHost | Opus 复用现有窗口（NEWTAB=no）打开路径；失败结构化 ActionFailed | ☐ |
| O4 | 多窗口 / 标签 | 两个 Lister 不同路径 | 能消歧则对；不能则全局，不串路径 | ☐ |
| O5 | 关闭竞态 | 呼出前后关闭 Opus | 全局 + HostGone/FolderUnavailable 提示 | ☐ |
| O6 | 中文 / 空格路径 | `C:\Users\...\项目 Docs` | `/info` 与 `Go` 参数不因空格/中文断裂 | ☐ |
| O7 | 长路径 | 深目录 | 与 Explorer 相同的本地校验与降级 | ☐ |
| O8 | dopusrt 缺失 | 临时重命名 dopusrt 或假路径 | FolderUnavailable / ActionFailed，全局可用 | ☐ |
| O9 | 非 0 退出 / 超时 | （可用测试桩或损坏命令） | 不抛异常到 UI，结构化失败 | ☐ |
| O10 | 提权 Opus | 管理员 Opus + 普通 Prism | HostElevated，不控制 | ☐ |
| O11 | 开关关闭 | DirectoryOpus 开关 off | 不识别，全局 | ☐ |

**版本记录**：Opus ______ / OS build ______ / 验收人 ______ / 日期 ______

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
| S1 | 无 DLL 注入 | 代码路径仅 Shell COM / dopusrt / Win32 查询 | ☐ |
| S2 | 无 LocalSystem Shell | 所有宿主调用在用户会话 WPF 进程 | ☐ |
| S3 | 原始 selector/命令不进 indexer | 协议只有可选 `root` 字符串路径 | ☐ |
| S4 | adapter 关时全局搜索 | 文件搜索、历史、拼音不受影响 | ☐ |
| S5 | 质量门 | `dotnet test` / `dotnet build -c Release` 通过 | ☐ |

---

## 5. 签署

矩阵 **全部** 相关行通过后，才可将对应开关考虑改为默认 true（需另开产品决策，不在本文件自动生效）。

| Adapter | 全部通过 | 签署 | 日期 |
| --- | --- | --- | --- |
| Explorer | ☐ | | |
| Directory Opus 13.23 | ☐ | | |
