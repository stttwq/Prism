# Prism 项目规划总纲 v1

> Listary 替代品 · Rust 后端 + C# WPF 前端双进程架构
> 制定日期：2026-07-27
> 内存基线：38MB（红线 40MB，可接受小幅上浮至 ~45MB）

---

## 一、项目定位与现状

**定位**：Everything（极致文件搜索）+ Listary（对话框/资源管理器联动 + 使用历史排序）+ PowerToys Run（应用启动 + 网页关键字）的合体，以**内存极低**为差异化壁垒。

**已成型能力**（不重构）：

- MFT 全量枚举 + USN Journal 实时索引（`ntfs.rs`、`hierarchy.rs`）
- FRN 紧凑层级索引（NodeSlot 12B/项 + 名字池，200 万文件 ~24MB）
- 双进程命名管道 IPC（`\\.\pipe\prism-core` + `prism-indexer-v1`）
- 双击 Ctrl 热键呼出、托盘、深浅色主题、动作面板（4 条基础动作）

---

## 二、核心约束（不可违背）

| 约束              | 红线                                | 来源     |
| ----------------- | ----------------------------------- | -------- |
| 常驻内存          | 40MB 硬红线，可接受 ~45MB 小幅上浮  | 用户决策 |
| 核心目标          | 又快又准                            | 用户决策 |
| 对话框联动        | DLL 注入重方案                      | 用户决策 |
| Hook 语言         | Rust cdylib（不引入 C++ 工具链）    | 用户决策 |
| 位数支持          | 仅 x64                              | 用户决策 |
| 拼音粒度          | 仅首字母（+1MB 以内）               | 用户决策 |
| 使用历史信号      | 打开/执行 + 定位/跳转，落盘         | 用户决策 |

---

## 三、关键技术决策（已锁定，不再讨论）

1. **不做 n-gram 倒排索引**——3600 万 (trigram,doc) 对约 144MB，超内存预算 3.6 倍。"快"靠 SIMD 线性扫 + top-k（学习 Everything 而非搜索引擎）。
2. **DLL 注入走三层叠加**：UIA 检测（C# 前端）→ 注入触发（C#）→ DLL hook（Rust cdylib）。任一层 fallback，避免单点故障。
3. **对话框先 UIA 后 DLL**：Milestone A（UIA）独立可用、零注入风险；Milestone B/C 才上注入。
4. **拼音只存首字母 key**：每中文名 1 字节/字，搜 `wx`→微信 可用，搜 `weixi`→微信 不支持（可接受的妥协）。

---

## 四、内存预算表

| 组件                          | 现状    | 增量       | 归属阶段             |
| ----------------------------- | ------- | ---------- | -------------------- |
| 索引服务（NodeSlot + 名字池） | ~24MB   | 0          | —                    |
| Rust broker + UIA 检测        | ~8MB    | +1-2MB     | P1                   |
| 使用历史 history.json         | 0       | +40KB      | P0                   |
| 首字母拼音 key                | 0       | +~1MB      | P3                   |
| 注入管理器（前端）            | 0       | +0.5MB     | P2（DLL 内存算目标进程） |
| **合计**                      | ~38MB   | **+3-4MB** | → **~41-43MB** ✓     |

---

## 五、里程碑路线

### P0 · 搜索地基（又快又准的最小闭环）— 1-2 周，内存 +40KB

**目标**：让搜索结果"从随机变成可用"。

| 子项                | 改动点                                                                                                       | 验收                                       |
| ------------------- | ------------------------------------------------------------------------------------------------------------ | ------------------------------------------ |
| SIMD 子串扫          | `hierarchy.rs:441` 手写滑窗 → `memchr::memmem`；`Cargo.toml` 加依赖                                          | ASCII 查询 5-10×                           |
| Top-K 小顶堆         | `hierarchy.rs::search` 重构，去掉"满 max 就 break"，全扫 + 堆(容量=200) + 排序                              | 结果质量质变                               |
| NodeSlot 位扩展      | `hierarchy.rs:18` flags 加 `is_executable` + `name_len_bucket`(2-3 bit)                                      | exe 预过滤                                 |
| 使用历史采集         | 新增 `history.rs`：`HistoryEntry { path, last_used, use_count }`，500 条 LRU，落盘 `history.json`           | execute/reveal 时 record                   |
| 排序加权             | `IndexHit` 加 `score`；综合分 = exe(+50)/文件夹(+10)/路径浅/前缀(+30)/名字短/历史加权                       | 单测覆盖排序                               |

**风险**：top-k 全扫可能比现状慢一点，但 SIMD 抵消；连续打字靠 P1 的前端缓存对冲。

**依赖**：无，可立即开工。

### P1 · 核心体验 — 1 周，内存 +1-2MB

**目标**：基本可用性修复 + Listary 60% 价值。

| 子项                                              | 改动点                                                                                                            | 验收                                       |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| 修失焦 bug（原方向 2）                            | `SearchWindow.xaml:7` 去永久 Topmost；删 `:151-153` 三连 hack；补 `LostKeyboardFocus` 兜底                       | 搜到结果点外部立即隐藏                     |
| 前端增量缓存（原方向 8）                          | `SearchViewModel.cs`：输入增长时本地过滤，generation 变/首字母变/删字符才回 Rust                                  | 连续打字不敲管道                           |
| **Milestone A：UIA 对话框检测 + 热键跳转**        | 新增 `FolderNavigationService.cs`；双击 Ctrl 检测前台是对话框/explorer → 弹搜索 → 选文件夹 → UIA SetCurrentFolder | 资源管理器/对话框能跳转                    |

**风险**：UIA 对不同应用的对话框兼容性参差，先支持标准 IFileDialog + Explorer，奇葩目标后补。

**依赖**：P0 的使用历史（Milestone A 跳转时记进 history）。

### P2 · Listary 灵魂（对话框 DLL 注入）— 3-4 周，内存 +0.5MB

**目标**：对话框内真正的 Listary 体验。

| 里程碑                   | 内容                                                                                                          | 验收                                                                                |
| ------------------------ | ------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| Milestone B：注入基础设施 | 新 crate `src/prism-hook/`（Rust cdylib, x64）；前端 `Injector.cs`（CreateRemoteThread + LoadLibraryW）；DLL 内只建管道 `\\.\pipe\prism-hook-<pid>` 回报存活 | 注入成功、通信通、**不做任何 hook**                                                |
| Milestone C：hook IFileDialog | DLL 内 MinHook `IFileDialog::SetFolder` / 订阅 `IFileDialogEvents`；Prism 选中 → DLL SetFolder               | 现代对话框全覆盖                                                                    |

**风险（最高）**：

- 杀软误报（CreateRemoteThread = 木马行为）→ 需数字签名 + 白名单申报
- UAC 跨权限（提权目标进程注入不进）→ 放弃提权目标，文档说明
- DLL 崩溃带崩宿主 → DLL 内全包 SEH + try/catch，绝不外泄

**依赖**：Milestone A（UIA 检测层决定何时触发注入）。

### P3 · 补全 — 1-2 周，内存 +1MB

| 子项                        | 说明                                                                                                |
| --------------------------- | --------------------------------------------------------------------------------------------------- |
| 拼音首字母                  | `Cargo.toml` 加精简拼音字典；`VolumeIndex` 加 `pinyin_keys` 池；query 纯 ASCII 时额外匹配首字母 key |
| Web 引擎图标（原方向 4）    | 新增 `WebEngineIcons.cs`，内置 Google/Bing/百度/GitHub 矢量图标，`ResultList` web 项挂载            |
| 第三方文件管理器（原方向 7）| `actions.rs:74` + `ipc.rs:455` 重复 reveal 抽公共函数；改 `SHOpenFolderAndSelectItems`；config 加 dopus/TC/custom |
| 死代码清理                  | 删 `search.rs`、`index.rs` 旧扁平索引、`ipc.rs:480` 的 `#[cfg(test)]` 旧 dispatch                  |

### P4 · 可选（按需，无时间表）

- Milestone D：对话框内嵌搜索框 UI（跨进程 UI 渲染，月级工程）
- 插件框架（原方向 5）：先做动作配置化（JSON 注册"前缀→命令"），不做 SDK
- x86 注入支持、内存深度优化

---

## 六、原始 8 项优化方向的归属

| 原方向         | 归属                    | 说明                                                              |
| -------------- | ----------------------- | ----------------------------------------------------------------- |
| 1 排序+exe 更新 | P0（排序）+ P3（Start Menu 增量） | Start Menu 增量靠 USN 或定时刷新，放 P3                          |
| 2 失焦不隐藏   | P1                      | 根因已定位（ForceActivate 抢前台 + 永久 Topmost）                 |
| 3 内存优化     | 不单独立项              | 每批后复测；当前 38MB 健康                                        |
| 4 引擎图标     | P3                      | Web 结果 favicon                                                  |
| 5 插件         | P4                      | 等动作生态验证后做                                                |
| 6 拼音         | P3（仅首字母）          | 模糊匹配合入 P0 排序的 score 层                                   |
| 7 第三方 FM    | P3                      | reveal 重构                                                       |
| 8 结果缓存     | P1                      | 前端增量过滤                                                      |

---

## 七、验收流程（每个里程碑结束）

1. `cargo test`（后端单测）全绿
2. `dotnet build`（前端，无测试工程）通过
3. 内存验收（`.trellis/spec/backend/quality-guidelines.md` 流程）：Release 构建 + 手动复制 `prism-core.exe` + 任务管理器读数
4. 手动跑核心场景用例
5. 按你的 Trellis 流程归档任务 + 记 journal

---

## 八、待办（不阻塞，但需记住）

- `.csproj` 没有自动 copy `prism-core.exe` 的 target，发布后需手动复制（开发期用 debug 联调，发布流程不动）
- 前端 `SearchViewModel` 零测试覆盖，P1 改它时建议补单测
- 日志粗糙（`crate::log` 无文件落盘/级别/轮转），P3 顺带补

---

**总工期估算**：P0–P3 合计约 **6-10 周**（取决于 DLL 注入的杀软/UAC 反复程度）。内存终态 **~41-43MB**，DLL 内存不计入 Prism。
