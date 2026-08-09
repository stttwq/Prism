# 诊断探针

G4 宿主联动与 IPC 韧性排查期间写的工具。留在库里是因为它们各自对应一个
**尚未收敛或需要实机复验**的问题，不是历史包袱。

背景与踩坑经过见 [`docs/排查踩坑记录.md`](../../docs/排查踩坑记录.md)。

> **脚本一律纯 ASCII**：PowerShell 5.1 在中文 locale 下按 GBK 读无 BOM 的
> UTF-8，中文注释会让脚本报**假语法错**（引号失配、`}` 意外），且行号指向
> 无关位置。中文说明写在本文件里，不要写进 `.ps1`。

## IPC 韧性（`08-07-prism-ipc-resilience`，未收敛）

`ERROR_PIPE_BUSY` 的因果链**还没证明**——A/B 对照显示未修复的旧二进制在
更猛的洪峰下同样零失败。下次遇到现场时用这些复现：

| 脚本 | 用途 |
| --- | --- |
| `probe-pipe-busy.ps1` | 交错发 Status 轮询 + 重查询，统计 `PIPE_BUSY` / 超时 |
| `probe-under-flood.ps1` | 起 N 个 writer 制造 USN 洪峰，再跑上面那个探针 |
| `ab-compare.ps1` | 对同一负载跑「未修复 vs 已修复」两臂。**需提权**，会换服务二进制并在 `finally` 恢复 |
| `elevate-ab.ps1` | 提权启动 `ab-compare.ps1` 并把全部输出写日志 |

真实触发条件（合成负载复现不了，务必看清）：完整规模索引
（`memory_bytes` ≥ 150MB）、**CPU 接近饱和**的外部负载、并发 Status 轮询。
单纯把 USN 速率推高没用——实测 5800 世代/秒仍不触发，而故障当天只有 340/秒。

## 宿主联动实机验收

| 脚本 | 用途 |
| --- | --- |
| `close-host-later.ps1` | 倒计时后 `PostMessage(WM_CLOSE)` 关掉宿主窗口。用于矩阵 E7/O5「宿主关闭竞态」——Prism 失焦即隐藏，鼠标点不到宿主的关闭按钮，必须让脚本代劳。`-HostKind Opus` 测 Opus |
| `TabProbe/` | C# 控制台探针，打印每条 `ShellWindows` 记录的 HWND、路径、view 窗口、父链、cloaked、`ShellTabWindowClass` 的兄弟 Z 序 |

`TabProbe` 是 E3 判据的来源。它证明了多标签 Explorer 下所有标签的视图窗口
**恒为 `visible=true`、`cloaked=0`、矩形相同**，所以 `IsWindowVisible` 判不出
活动标签；可用的判据是 `ShellTabWindowClass` 在兄弟中的 Z 序（最前者即活动）。
若将来 Windows 更新改了 shell 窗口层次，用它重新推导判据。

用法：`dotnet build -c Release`，然后 `TabProbe.exe`（一次采样）或
`TabProbe.exe watch`（40 秒轮询，手动切标签观察哪个字段跟着变）。
必须 STA——`Shell.Application` 的 `ShellWindows` 在 MTA 下跨单元编组会失败，
表现为 HWND=0、属性全空、`QueryService` 返回 `E_NOINTERFACE`。

> 测**真**资源管理器只能用 **Win+E**。这台机器上 Directory Opus 接管了默认
> 文件管理器，`explorer.exe <路径>` 和 `explorer.exe shell:MyComputerFolder`
> 打开的都是 Opus Lister，`CabinetWClass` 永远找不到。

## 其他

| 脚本 | 用途 |
| --- | --- |
| `elevate-install.ps1` | 从普通 shell 提权跑 `scripts/prism-build.ps1 -Bootstrap`，输出写 `install.log` |

## 不进版本库的内容

`.gitignore` 排除了本目录的 `*.exe` / `*.dll` / `*.pdb` / `Prism.*.json`、
`*.log`、`probe-output.txt` 和 `TabProbe/bin|obj`。构建产物留在库里会造成
「陈旧副本遮蔽新构建」——那正是本轮排查最大的时间黑洞。
