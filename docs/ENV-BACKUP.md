# Listary 项目迁移到新机器清单

> 2026-08-23 整理。目标:新机器 **仅 C/D 两盘** 时,把本机 `E:\LS\DM\Listary` 迁过去后能直接编译、跑测试、出安装包。
> 本机已完成的清理见文末「本次已做」。

---

## 一、本机开发环境(版本 / 安装位置)

| 工具 | 版本 | 本机安装位置 | 新机必装 |
|---|---|---|---|
| Rust (rustup) | 1.97.1, stable-x86_64-pc-windows-msvc | `C:\Users\jia\.rustup` + `C:\Users\jia\.cargo` | ✔ |
| MSVC 工具链 | VS 2022 Build Tools 17.14.37 | `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools` | ✔ |
| Windows SDK | 10.0.26100.7705 / 10.0.22621.5040 | `C:\Program Files (x86)\Windows Kits\10` | ✔ |
| .NET SDK | 8.0.423 (运行时 8.0.29) | `C:\Program Files\dotnet` | ✔ |
| Inno Setup | 7.0.2 | **`D:\LS\Setup 7\`**(非默认盘!) | ✔(路径随意,构建脚本自动发现) |
| Git | 2.55.0.windows.3 | `C:\Program Files\Git` | ✔ |
| Python | 3.13.14 + pip 26.1.2 | `C:\Users\jia\AppData\Local\Programs\Python\Python313` | 按需 |
| Node.js | v22.22.2 (npm 10.9.7) | `C:\Users\jia\AppData\Local\Programs\nodejs` | 按需 |
| JDK | Temurin 21.0.12 | `E:\LS\Android\Java\jdk-21.0.12+8` | 仅 Android 开发 |

**只迁 Listary 项目:** 上表「新机必装」三列(✔)是编译链必需,其余(编辑器、AI 助手、日常软件)按需。

---

## 二、迁移打包清单(拷哪些 / 不拷哪些)

### 必拷(源码 + 配置)
```
README.md  AGENTS.md  rustfmt.toml  .gitignore
src\Prism\            WPF 前端源码
src\Prism.Tests\      dotnet 测试
src\prism-core\       核心 crate(注意:不含 target/)
scripts\              全部构建/测试脚本
docs\                 (含本文件)
tools\                bench 等辅助工具(见下)
.trellis\  .agents\  .zcode\  .codex\  .claude\   工作区配置(可选,带则保留 AI 协作上下文)
```
- 若要用 `tools\bench` 跑性能基线,把 `tools\bench\` 和 `tools\pinyin-match-test\`(合计约 2.3GB,主要是 `scan-floor` 输入数据)一起拷;否则可跳过。
- 其余 `tools\gbk-test\`、`tools\suggestion-test\` 是源码(几百 KB),按需。

### 不拷(已清理 或 会自动重建)
| 目录 | 清理前大小 | 说明 |
|---|---|---|
| `src\prism-core\target\` | 6.2 GB | cargo 编译产物,**已删**,`.gitignore` 已含 `**/target/` |
| `src\Prism\bin\ obj\`、`src\Prism.Tests\bin\ obj\` | 小 | dotnet 产物,**已删** |
| `target\`(仓库根) | 0.5 GB | g0/gTest 临时目录,**已删** |
| `tools\*\target\ bin\ obj\` | ~2.3 GB(部分) | 工具链编译产物,**已删** |
| `artifacts\` | 若干 | 测试日志/截图,手动测试产物,可拷可不拷 |
| `.git` | — | 建议通过 `git clone` 新克隆,不要拷历史 |

> **首次构建在新机重编**:`cargo build` 会重建 `src\prism-core\target\`(几分钟到十几分钟)。若想省重建时间,可保留 `src\prism-core\target\` 一起拷(6.2GB)。

---

## 三、新机器落地步骤

1. 装环境:按上表「新机必装」安装(官方安装器 / winget)。
   - Inno Setup 随便装哪个盘都行:`scripts\build-installer.ps1` 会从注册表自动发现;找不到时回退到固定路径。
2. 把项目拷到新位置(如 `D:\projects\Listary`)。
3. 改以下「硬编码路径」(见第四节)。
4. 首构建:`.\scripts\prism-build.ps1` → 出安装包 `.\scripts\build-installer.ps1 [-IsccPath <Inno的ISCC.exe>]`。

---

## 四、迁移后必须处理的硬编码路径

### 已修好(无需处理)—— 已改为相对 `$PSScriptRoot`
| 文件 | 原硬编码 |
|---|---|
| `scripts\path-query-smoke.ps1` | `E:\LS\DM\Listary\src\Prism...`(3 处) |
| `scripts\tmp-after-search-mem.ps1` | `E:\LS\DM\Listary\artifacts\...` |
| `scripts\tmp-icon-probe.ps1` | `E:\LS\DM\Listary\artifacts\...` |
| `scripts\ui-shot.ps1` | `E:\LS\DM\listary\artifacts\ui-shot.png` |

### 需在新机手动改
1. **用户环境变量 PATH** 里的 6 条 `E:` 条目(指向旧盘,删或改成新路径):
   - `E:\LS\Android\Java\jdk-21.0.12+8\bin`
   - `E:\LS\Android\Sdk\platform-tools` / `cmdline-tools\latest\bin` / `build-tools\37.0.0`
   - `E:\LS\ZCode\resources\tools\ripgrep` / `\ugrep`
2. **用户环境变量** `JAVA_HOME`(`E:\LS\Android\Java\jdk-21.0.12+8`)、`ANDROID_HOME`(`E:\LS\Android\Sdk`)——不跑 Android 可删。
3. **文档操作命令**:`docs\G4-手动测试流程.md` 有 4 处 `E:\LS\DM\listary\...` 绝对路径(第 71/106/318/403 行),照文档前先把 `E:\LS\DM\listary` 替换成新路径。
4. `C:\Users\jia\.cargo\config.toml` 配了 USTC crates 镜像(不涉及盘符,一般保留即可;公司内网想用官方源可删)。

### 无需处理(仅历史记录)
- `.trellis\tasks\...`、`.zcode\plans\...`、`docs\PRISM-*` 里的 `E:\...` 出现是设计文档/计划记录,不是执行路径,不影响构建。

---

## 五、本次已做(2026-08-23)

- 删除全部编译残留:根 `target\`、`src\prism-core\target\`、`src\Prism\bin|obj`、`src\Prism.Tests\bin|obj`、`tools\{gbk-test,suggestion-test}\bin|obj`、`tools\pinyin-match-test\target`、`tools\bench\scan-floor\target`。所有被删目录都在 `.gitignore`(未跟踪),删除对 git 无影响。
- 4 个脚本的 `E:` 绝对路径改为相对 `$PSScriptRoot`.