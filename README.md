# Prism

Listary 替代品：全盘文件名即时搜索 / 启动软件 / 网页快捷搜索，常驻后台，内存占用 ≤ 100MB。

## 安装

用 `dist\PrismSetup-1.0.0.exe`（由 Inno Setup 脚本 `dist\prism.iss` 编译生成）安装：

- 向导全程中文，可选安装目录（支持中文目录，如 `D:\工具\Prism`）。
- 可勾选：开始菜单快捷方式、桌面快捷方式、开机自启。
- 系统要求：Windows 10 / 11 64 位，已安装 .NET 8 运行时（前端框架依赖发布，未内嵌；如未安装可在
  https://dotnet.microsoft.com/download/dotnet/8.0 安装 Deskop Runtime）。

如需自行编译安装包：

1. 安装 Inno Setup 6（https://jrsoftware.org/isdl.php）。
2. 在仓库根目录执行：

   ```bash
   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" dist\prism.iss
   ```

   产物为 `dist\PrismSetup-1.0.0.exe`。安装源（`Prism.exe` 单文件 + `prism-core.exe` + `prism.ico`）已在 `dist/` 备齐。

## 使用

- **呼出搜索框**：连续按两下 `Ctrl`（默认），在屏幕中央弹出搜索框；按 `Esc` 或点击别处隐藏。
  可在设置里改成其他快捷键。
- **搜文件**：输入文件名片段，1 秒内列出全盘匹配结果，越打字越刷新。
  - `回车` 打开选中项；
  - `→` 打开动作面板（打开所在文件夹 / 复制 / 剪切 / 复制路径 / 系统右键菜单）；
  - `Ctrl + 1..9` 直接打开第 1~9 条结果。
- **启动软件**：输入软件名（含中文名，如「微信」），已安装程序排在文件结果前面，回车启动。
- **网页快捷搜索**：预设引擎——`g 关键词` = Google、`b 关键词` = 百度、`bi 关键词` = Bing，用默认浏览器打开。
  设置里可新增任意网站关键词。

## 设置与自启

- 托盘图标右键 → 「打开设置」：可改呼出快捷键、管理网页引擎关键词、切换开机自启。
- 开机自启通过注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Prism` 实现，无需管理员权限。
- **数据存放位置**（索引缓存 + settings.json）：
  - 装在用户可写目录（如 `D:\工具\Prism`）→ 用「安装目录\data」;
  - 装在 Program Files（只读）→ 自动改用 `%LocalAppData%\Prism`，并在设置页显示实际位置。
- **深/浅色**：跟随 Windows 系统主题自动切换。

## 卸载

开始菜单 → 「卸载 Prism」，或「设置 → 应用」中卸载。卸载时会先退出常驻进程，并清理便携模式下的 `data` 目录；用户数据夹中的数据保留。

## 内存验收

任务管理器观察 Prism 全部进程（前端 Prism.exe + 后端 prism-core.exe）：常驻合计私有工作集约 38MB，远低于 100MB 上限（实测基线见 `.trellis/spec/backend/quality-guidelines.md`）。

## 项目结构

- `src/prism-core`：Rust 后端（索引 / 搜索 / 程序清单 / 网页关键词，命名管道 JSON 服务）。
- `src/Prism`：C# WPF 前端（搜索窗口 / 设置 / 托盘 / 全局快捷键）。
- `dist/`：安装源与 Inno Setup 脚本。

## 开发构建

用一条命令完成「构建 → 安装 → 校验」，管理员权限运行：

```powershell
.\scripts\prism-build.ps1              # 常规
.\scripts\prism-build.ps1 -Bootstrap   # 新机器（目录/服务都还没有）
.\scripts\prism-build.ps1 -VerifyOnly  # 只查漂移，不改任何东西
```

装完会用 SHA-256 逐个比对安装字节与构建字节，不一致就报 `DRIFT DETECTED`。
**行为不符预期时先跑 `-VerifyOnly`**：「跑的不是你改的代码」是本项目最常见的
时间黑洞，成因和排查方法见 [`docs/排查踩坑记录.md`](docs/排查踩坑记录.md)。

从 Git Bash 手工构建前先 `source scripts/msvc-env.sh`，否则 Git 自带的
coreutils `link` 会被 rustc 当链接器用（只在产出 `.exe` 时才报错）。
