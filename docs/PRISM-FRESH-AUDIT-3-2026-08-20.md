# Prism 全新独立审计与修改方案 3（2026-08-20 晚）

本轮在当天早些时候的 G1~G5（FRESH-AUDIT-2 的 31 项 + 小问题 3 项）全部实施完毕、全量门禁绿（cargo 350 / dotnet 221 / clippy 0 警告 / 安装包重建）之后进行。方法与上轮相同：**两个并行审读代理从零通读**（Rust 核心 24 文件 ~18k 行、C# 前端 ~13k 行），**刻意不读 docs/、不读 git 历史**，避免锚定；叠加 2026-08 最新联网对标（Everything 1.5 Beta 1366a / Listary V7 7.0.0.9）。

上一轮的发现（F1~F9 与打磨项）已全部修完，本轮发现与它们不重叠。

---

## 1. 联网对标更新（2026-08）

| 维度 | Everything 1.5 Beta（2026-05~08 构建） | Listary V7（7.0.0.9，2026-08-05） | Prism 现状 |
|---|---|---|---|
| 进程模型 | Service + UI | **7.0.0.9 把 listary-core 并回主进程**（理由：稳定性与性能、减少启动重复扫描） | 三进程维持（见 §4 讨论） |
| 内存 | Service 索引期内存泄漏已修（1364a→1366a），索引期 ~10MB | 新引擎宣称内存 **-30%**、速度 +20%~100% | ~40MB/百万文件，仍占优 |
| 索引更新 | 1.5 Beta 修了"重启后索引更新停止"的问题 | 重做自定义索引 + **索引故障排查工具** | USN watcher + 按卷缓存（本轮 H1 暴露恢复策略缺口） |
| 搜索语法 | 属性索引/搜索/排序（可配置属性集控内存） | **高级语法：文本+路径+日期+大小组合** | ext:/path: 两类过滤（G7），无日期/大小 |
| 启动器 | — | **未输入先推荐最近/常用文件**；多选；预览面板 | 空查询=根目录 MRU（仅历史，无全局推荐） |

结论：Prism 在内存效率与索引纪律上仍领先；差距项变为**恢复策略的收敛性**（本轮高严重度发现所在）与**搜索语法丰富度**（Listary V7 的日期/大小过滤、未输入推荐）。

---

## 2. 本轮发现（Rust 13 项 + C# 11 项，去重合并 22 项）

### 高（1 项）

**H1. rebuild 请求积压清理丢请求：留旧弃新，卷 watcher 死后永不复活**
- 位置：`indexer_runtime.rs:896-899`
- `rebuild_rx.recv()` 取第一条后 `while rebuild_rx.try_recv().is_ok() {}` 把后续请求全部静默丢弃——注释说"合并为最新一条"，实际是**留最旧、丢最新**。两卷 watcher 相近时刻出错时，第二个卷的 SingleVolume 请求被丢，该卷 watcher 已退出且无 liveness 检查 → 该卷索引从此静默陈旧直到服务重启。
- 修法：drain 时按 volume_id 去重保留每卷最新一条（Full 优先），循环处理；补"双卷同时失败"集成测试。
- **影响面**：重建调度循环。**风险**：中低（改动集中于 recv 循环，语义由测试锚定）。**严重度：高**（静默数据陈旧 + 无自愈）。

### 中（5 项）

**M1. 单卷重建无退避、失败无重试**（`indexer_runtime.rs:1595-1613, 951-993, 985-992`）
watcher 出错立即触发全量 MFT 重扫，错误持续重现（清理工具反复删 journal）时形成背靠背全盘扫描热循环；反向：重建两次失败后仅 `set_error`，该卷永久失活。修法：SingleVolume 加退避（复用 `apps::app_scan_retry_delay` 模式）+ 失败后延迟重试。影响面：watcher 错误路径。风险：低。

**M2. 停机路径 checkpoint + 全量拼音重建可能超 SCM 30s wait_hint**（`indexer_runtime.rs:1081-1086, 1804-1819`）
`checkpoint_after_save` 无条件 `rebuild_pinyin_from_live`（整索引 clone + 全节点编码 + fsync + mmap 校验），大索引 + 慢盘时 Stop 超时被强杀 → sidecar 可能写一半 → 下次启动 IndexMismatch → 又一轮全量重建。修法：停机路径跳过拼音重建（只做 v5 落盘），拼音重建挪 maintenance tick。影响面：停机路径。风险：低。

**M3. 拼音 delta 计数把纯英文名变更也计入，4096 上限后每 5s 一次全量重建**（`pinyin_sidecar.rs:25,324-338` + `indexer_runtime.rs:1032-1047`）
npm install / Windows Update 类风暴必触发周期性"整索引 clone + 全节点拼音编码 + fsync"；叠加 Arc COW 克隆（G4 引入，已有 ponytail 天花板注释）。修法：delta 只统计含汉字编码的条目；风暴期重建退避（如 60s）；如仍热再拆独立 delta 锁。影响面：sidecar 维护路径。风险：中（语义改动需测试锚定"英文风暴不触发重建"）。

**M4. STA Shell worker 无超时：一个模态/挂死调用冻结所有文件动作**（`shell.rs:200-226,183-185`）
properties 模态页 / 死网络路径 / IFileOperation 对话框挂住唯一 STA worker 时，后续动作在容量 32 的 channel 排队，每个占一个 spawn_blocking 线程无限等待。修法：`recv` 加超时（60s 级）返回 System 错误；模态类动词单独标注预期阻塞（前端 F6 已有 5 分钟兜底，这里补 broker 侧）。影响面：动作执行队列。风险：中（需区分合法长对话框，与前端 F6 同样的权衡）。

**M5. IndexerGenerationClient 长轮询读无超时，半死 indexer 让 generation 推送永久哑掉**（C# `IndexerGenerationClient.cs:151,125-139,52-57`）
与 broker 通道形成对照——那边握手读专门做了 WhenAny 竞速 + DisposeStreamOnly，这里裸 `ReadLineAsync` 永久挂起后 `_loop` 永不完成，`SetActive(true)` 直接 return 不换新连接 → 文件变更后结果列表静默陈旧直到重启。修法：照抄 `PipeChannel.HandshakeAsync` 模式（31s 竞速 + Dispose 底层流 + 走既有 1s 退避重连）。影响面：仅该类。风险：低。

### 低（16 项摘要）

**Rust**：maintenance tick 在 async runtime 上同步抢 `index.read()` 三处（`indexer_runtime.rs:998-1008,1051-1053`，挪 spawn_blocking）；`rollback_mutations` 不回滚 names_fingerprint/dead_name_bytes/present_slots（`hierarchy.rs:778-784`，回滚后被重建覆盖、影响有限，快照多存三个计数即可修正）；流式 checkpoint 三次竞争失败回落整态 clone 在 USN 洪峰期成常态（`indexer_runtime.rs:1711-1737`，可放宽为按卷 next_usn 比对）；broker 侧无连接数上限/空闲超时（`ipc.rs:434-500`，照搬 indexer 的 try_admit_connection，上限 8 即可）；搜索全程持 index.read() 属设计权衡（观察项）；拼音 matched_count 把已删记录计入致 is_truncated 虚高（`pinyin_sidecar.rs:416 vs 559`，PRESENT 检查前移）；history 节流脏数据无定时器兜底（`history.rs:343-370`，崩溃丢最近动作，可加 250ms 定时 flush 或接受）；首建单卷瞬时 ~230MB（枚举产物与构建中 VolumeIndex 同驻，可两遍 MFT 扫描换内存或接受）。

**C#**：全局异常兜底无条件 Handled + 异常处理内同步 AppendAllText（`App.xaml.cs:375-381,354-366`——持续性异常变 UI 冻结 + 日志风暴，加限流 + 后台写日志）；空查询转换 UI 线程 Gen1 压缩 GC（`SearchWindow.xaml.cs:166-171`，可降为 Optimized 或留给隐藏定时器）；Combo 热键注册失败降级双击 Ctrl 时漏装 60s 钩子自愈定时器（`HotkeyService.cs:80-91,245-269`，fallback 分支补 StartHookRefresh）；三处后台→UI 用同步 Dispatcher.Invoke（`SearchWindow.xaml.cs:154-158,796-799,831-833`，统一改 BeginInvoke）；握手超时竞速的 Task.Delay 未释放（`PipeClient.cs:1086-1091`）；管道写路径无超时（现状安全，协议演进时注意）；favicon 探测完成后需等下一次按键才换图标（`WebIconProvider.cs:121-152`，Finish 后补一次 Decorate）；每次呼出新建一次性 STA 线程做宿主识别（`SearchWindow.xaml.cs:282-294`，可换单一常驻 STA 工作线程）；AllowsTransparency 动画帧率（既定 DWM 圆角方向）；管道名无会话限定，RDP 双会话可能跨会话互杀 broker（`PipeClient.cs:149`，管道名加 `Local\` 前缀或比对 session id 再杀）。

---

## 3. 实施方案（三批，按风险递增）

### 批次 H+M1（恢复策略收敛性，最高优先）
H1（rebuild 队列按卷去重）+ M1（退避与重试）+ M2（停机跳过拼音重建）。三者同属"恢复路径收敛性"，一起修一起测：新增"双卷同时失败 → 两卷都重建"与"journal 持续删除 → 重建有退避"两条集成测试。
**影响面**：indexer_runtime 重建调度与停机路径。**风险**：中——H1 改语义但方向明确（丢新→留新），M1 复用既有退避模式；测试锚定后风险可控。

### 批次 M3+M4+M5（资源尖峰与挂死面）
M3（拼音 delta 只计汉字 + 重建退避）、M4（STA worker recv 超时）、M5（GenerationClient 读超时自愈）。
**影响面**：sidecar 维护、动作队列、世代推送客户端。**风险**：中低——M3 需锚定"英文风暴不触发重建"；M4 与前端 F6 的 5 分钟超时取齐（broker 侧 60s 对非模态动作）；M5 照抄既有模式。

### 批次 L（打磨，按价值挑拣）
Rust：tick 读锁挪 spawn_blocking、rollback 回滚派生计数、broker 连接上限、拼音 matched_count 修正。C#：异常兜底限流 + 后台日志、fallback 钩子定时器、Dispatcher.Invoke→BeginInvoke、Task.Delay 释放、favicon 完成即重绘、常驻 STA 线程、RDP 会话限定管道名。
**影响面**：单点局部。**风险**：低。

### 明确不做 / 维持
1. **三进程维持**：Listary 7.0.0.9 并回主进程是它的引擎重写配套决策（其旧架构权限/稳定性问题 Prism 不存在——Prism 管道协议有超时/配对/ACL/收编治理，本轮 C# 审计确认无死锁实证）。并回会损失 broker 以普通用户运行（IFileOperation 对话框）与索引服务 SYSTEM 权限的分离，得不偿失。
2. **memchr SIMD**：仍为 Everything 差距项（其多线程线扫官方帖 t=9463），~30KB 新依赖需人工批准后再做，预估 ASCII 扫描再 2-4×。
3. **倒排/trigram、v6 缓存迁移、属性索引**：维持前轮"不做"结论（Listary 的日期/大小语法属功能级决策，见搜索功能报告）。

---

## 4. 来源
- 代码：两个独立审读代理从零通读（Rust 24 文件 / C# 全量），未读 docs/ 与 git log。
- Everything：[1.5 Beta 讨论帖 t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)、[Service 内存泄漏修复 t=14437](https://www.voidtools.com/forum/viewtopic.php?t=14437)、[1.5 官方页](https://www.voidtools.com/everything-1.5/)、[属性索引内存讨论 t=11234](https://www.voidtools.com/forum/viewtopic.php?t=11234)。
- Listary：[V7 Beta 公告（7.0.0.9，2026-08-05）](https://discussion.listary.com/t/listary-v7-beta-is-here-the-launcher-now-recommends-plus-a-new-engine-multi-select-fresh-themes-updated-to-7.0-0-7-on-july-20/10259)、[官方 changelog](https://dl.listary.net/changelog.html)、[beta changelog](https://help.listary.com/changelog-beta)。
- SIMD 参考：[Wojciech Muła sse4-strstr](https://github.com/WojciechMula/sse4-strstr)、[SIMD-friendly substring search](https://news.ycombinator.com/item?id=44274001)。
