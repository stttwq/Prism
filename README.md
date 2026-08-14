# Prism

Listary 替代品：全盘文件名即时搜索 / 启动软件 / 网页快捷搜索，常驻后台，三进程私有工作集 ≤ 100MB。

三个常驻进程：`Prism.exe`（WPF 前端，普通用户）→ `prism-core.exe`（broker，普通用户）→
`prism-indexer-service.exe`（LocalSystem 服务，只负责 MFT/USN 索引与只读搜索）。
Shell 操作、剪贴板、用户设置和联网一律留在普通用户会话，不下沉到服务。

## 安装

用 `dist\PrismSetup-1.0.0.exe`（由 Inno Setup 脚本 `dist\prism.iss` 编译生成）安装：

- 向导全程中文，可选安装目录（支持中文目录，如 `D:\工具\Prism`）。
- 可勾选：开始菜单快捷方式、桌面快捷方式、开机自启。
- 系统要求：**Windows 11 x64**，已安装 .NET 8 运行时（前端框架依赖发布，未内嵌；如未安装可在
  https://dotnet.microsoft.com/download/dotnet/8.0 安装 Desktop Runtime）。
  产品边界只承诺 Windows 11 x64——Windows 10、ARM64 和 x86 均不在兼容承诺内
  （见综合计划 §18）。安装包本身不拦截 Windows 10，但那属未验收环境。
- 安装需要管理员权限：索引器以 LocalSystem 服务（`PrismIndexer`）注册运行。

如需自行编译安装包：

1. 安装 Inno Setup 6（https://jrsoftware.org/isdl.php）。
2. 在仓库根目录执行：

   ```bash
   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" dist\prism.iss
   ```

   产物为 `dist\PrismSetup-1.0.0.exe`。安装源已在 `dist/` 备齐，共四个文件：
   `Prism.exe`（单文件）、`prism-core.exe`、`prism-indexer-service.exe`、`prism.ico`。

   > **`dist/` 里的产物可能落后于源码，发版前必须重建。** 先跑 `.\scripts\prism-build.ps1`
   > 再打包，不要直接拿库里的二进制。「跑的不是你改的代码」是本项目最常见的
   > 时间黑洞，成因和排查方法见 [`docs/排查踩坑记录.md`](docs/排查踩坑记录.md)。

## 使用

- **呼出搜索框**：连续按两下 `Ctrl`（默认），在屏幕中央弹出搜索框；按 `Esc` 或点击别处隐藏。
  可在设置里改成其他快捷键。
- **搜文件**：输入文件名片段，1 秒内列出全盘匹配结果，越打字越刷新。跨卷结果参与同一排序，
  不再按 MFT 顺序截断。
  - `回车` 打开选中项；
  - `→` 打开动作面板（打开所在文件夹 / 复制 / 剪切 / 复制路径 / 重命名 / 复制到… / 移动到… /
    移入回收站 / 永久删除 / 压缩为 ZIP / 属性 / 打开方式 / 系统右键菜单）；
  - `Ctrl + 1..9` 直接打开第 1~9 条结果。
- **查询过滤器**：在搜索词后加 `ext:pdf` 按扩展名过滤（逗号分隔多个，如 `ext:pdf,doc`），
  `path:"Project Docs"` 按路径过滤（引号包裹含空格的值）。过滤器只返回文件/文件夹，不混入
  应用和网页结果。
- **拼音搜索**：默认开启。支持首字母与全拼，`wx` / `weixin` / `weix` / `xin` 都能命中「微信」；
  不匹配父路径，至少两个拉丁字母才触发。sidecar 缺失或损坏时自动退回字面搜索。
- **使用历史**：默认开启。成功的打开/定位操作会在**同一匹配等级内**加权，不会让弱拼音命中
  盖过强字面命中。设置里可关闭，也可一键清除。
- **限定当前目录**：从资源管理器或 Directory Opus 呼出时可限定宿主当前目录（含子目录），
  `Ctrl+G` 或点击范围标签在当前目录与全局之间切换。识别失败立即退回全局，不复用上次目录。
  两个宿主识别开关（Explorer / Opus）目前**默认关闭**，属实验性功能，需在设置里手动开启。
- **启动软件**：输入软件名（含中文名，如「微信」），回车启动。类型只作为最终平局规则，
  不再无条件把所有应用排在文件前面。
- **网页快捷搜索**：预设引擎——`g 关键词` = Google、`b 关键词` = 百度、`bi 关键词` = Bing，用默认浏览器打开。
  设置里可新增任意网站关键词。
- **首次安装**：索引逐卷发布，系统卷建完即可搜该卷文件，其余卷仍在后台构建，搜索框显示
  可解释进度，不再整段不可用。

## 设置与自启

- 托盘图标右键 → 「打开设置」：可改呼出快捷键、管理网页引擎关键词、切换开机自启，
  以及开关拼音、使用历史、当前目录限定与两个宿主识别。
- 开机自启通过注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Prism` 实现，无需管理员权限。
- **数据存放位置**分两级：
  - **用户级**（`settings.json`、`history-v1.json`）：装在用户可写目录（如 `D:\工具\Prism`）→
    用「安装目录\data」；装在 Program Files（只读）→ 自动改用 `%LocalAppData%\Prism`，
    并在设置页显示实际位置。
  - **机器级**（索引缓存 `index-v5.bin`、拼音 sidecar `pinyin-v1.bin`）：固定在
    `%ProgramData%\Prism`，由 LocalSystem 索引服务写入。两者都是可重建的派生数据，
    删掉只会触发重建，不丢用户数据。
- **深/浅色**：跟随 Windows 系统主题自动切换。

## 卸载

开始菜单 → 「卸载 Prism」，或「设置 → 应用」中卸载。卸载时会先停服务、退出常驻进程，
并清理便携模式下的 `data` 目录与机器级 `%ProgramData%\Prism`（索引缓存和拼音 sidecar 都是
可重建派生数据）。装在 Program Files 时，`%LocalAppData%\Prism` 里的用户数据保留。

## 内存验收

门槛是**三个进程**（`Prism.exe` + `prism-core.exe` + `prism-indexer-service.exe`）同步采样的
私有工作集之和 ≤ 100MiB。不要用任务管理器的「提交大小」或 `PrivateMemorySize64` 代替。

最近一次完整实测（2026-08-14，拼音开启，3 卷索引）：

| 进程 | 私有工作集 |
| --- | ---: |
| Prism.exe | 15.0 MiB |
| prism-core.exe | 8.5 MiB |
| prism-indexer-service.exe | 78.7 MiB |
| **合计** | **102.2 MiB** |

Prism.exe 内存偏高是因为前台窗口已展开且加载了结果图标；隐藏后会降回 ~4 MiB。
prism-indexer-service.exe 占大头，主要是 MFT 索引常驻内存（3 卷约 330 万节点）。
绝对值随索引规模变化，不同机器、不同卷数不可直接对比。
采样口径、门槛定义与失败矩阵见
[`.trellis/spec/backend/quality-guidelines.md`](.trellis/spec/backend/quality-guidelines.md)，
采样脚本是 `tools/bench/Measure-ProcessMemory.ps1`。

## 项目结构

- `src/prism-core`：Rust 后端，产出两个二进制——`prism-core.exe`（broker：应用清单 / 网页关键词 /
  历史 / Shell 动作）与 `prism-indexer-service.exe`（LocalSystem 服务：MFT/USN 索引、v5 缓存、
  拼音 sidecar、只读搜索协议）。
- `src/Prism`：C# WPF 前端（搜索窗口 / 设置 / 托盘 / 全局快捷键 / 宿主适配器）。
- `src/Prism.Tests`：前端单元测试（xUnit）。
- `tools/bench/`：性能与内存基准脚本，见 [`tools/bench/README.md`](tools/bench/README.md)。
- `dist/`：安装源与 Inno Setup 脚本。
- `docs/`：规划与排查文档，入口见 [`docs/PRISM-COMPREHENSIVE-PLAN.md`](docs/PRISM-COMPREHENSIVE-PLAN.md)。

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

## 质量门

每个阶段收尾前四条全绿（2026-08-14 实测：Rust 262 通过 / C# 134 通过 / clippy 与 build 无告警）：

```bash
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test src/Prism.Tests/Prism.Tests.csproj -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

静态检查通过**不能**替代机器验收。涉及索引、范围、拼音或动作的改动还要跑 `tools/bench/`
的延迟与内存采样，宿主联动改动要重跑兼容矩阵。
