# Prism 搜索功能现状报告（2026-08-20）

独立成篇的当前版本搜索功能盘点：现状怎么做、对标 voidtools Everything 与 Listary 的做法、差距与改进意见。技术细节以本轮从零通读的代码为准（含当天上午 G1~G5 修改后的状态）。

---

## 1. Prism 搜索栈现状

### 1.1 数据层（prism-core / prism-indexer-service）

- **索引结构**：每卷一张 FRN 记录号索引的紧凑槽表（12 字节/槽：parent_record u32 + name_off u32 + sequence u16 + flags u16），名字集中在一个 NUL 终止的字节池。无倒排、无 trigram——与 Everything 同路线：**全库线性扫描 + Top-K 堆**。
- **构建**：NTFS MFT 全量枚举（FSCTL_ENUM_USN_DATA，记录池化 ~40B/条，名字池 UTF-8 化一次），子先于父到达的乱序用 pending 定点迭代解决，最多 64 轮；名字池容量按记录数×64 预留消除扩容尖峰。
- **更新**：USN journal 增量（read_changes 批读 + 256KB 缓冲复用），child-before-parent 延迟重试；排除边界（node_modules 等进出排除表）触发该卷重建；名字池死字节超阈值时锁外压缩。v5 缓存按卷生效（journal 可回放性逐卷判定），checkpoint 逐卷流式序列化（写侧 2× 内存峰值已消除）。
- **扫描**：非 ASCII/大小写折叠的零分配子串匹配（N1），总槽位 ≥512K 时分块并行扫描（N2，实测 1.2M 记录 ASCII 44ms→10ms、中文 104ms→24ms），Top-K 之前完成 ext:/path: 过滤与范围过滤。

### 1.2 排序与记忆

- **匹配层级**（锁死不可跨越）：字面命中 > 全拼 > 首字母；层内 class（精确=0/前缀=1/包含=2）→ 位置 → 分数 → **使用档位**（frecency 折算）→ 名字 → 路径 → 类型。无元数据的注入项（如历史候选）垫底。
- **历史**：frecency 半衰期折算 + 容量 LRU（5000 条）+ 每条 ≤8 个查询键；**查询记忆**（pick）：`report ext:pdf` 与 `report` 落同一条记忆，上次在该查询选中的项在 kind 内置顶。写盘 250ms 节流 + Drop 兜底。
- **范围搜索**：呼出时识别前台宿主（Explorer/Directory Opus/Total Commander 类）的 CWD，`Ctrl+G` 切换全局/当前目录；root 解析带单槽 generation 失效缓存（F2，本日修），拒绝原因结构化回传（volume_not_indexed/not_found/…）。

### 1.3 查询能力

- **过滤语法**：`ext:pdf,md`（OR）、`path:"Project Docs"`（AND 子串、大小写不敏感）、可组合；过滤时抑制程序/网页行，只留文件/文件夹。
- **拼音**：可选 sidecar（全拼+首字母，UTF-16 高亮区间映射），delta 机制吸收 USN 增量，上限 4096 条触发重建；按卷缓存 + identity 指纹（本日 F3 改增量维护）。
- **窗口模式**：`>` 前缀搜可切换窗口（按 MRU + 使用历史），token 复核防句柄复用。
- **网页模式**：`g/b/bi + 空格` 进引擎搜索（内置 Bing/百度/Google + 自定义引擎），网址类查询词直接打开（本日 Q3），在线联想 800ms 超时静默回退。
- **空查询**：根目录范围内出历史 MRU（磁盘存在性核验）；装满 max 即报可展开。

### 1.4 前端体验

即时搜索（50ms 防抖）+ 前缀缓存 + 完整响应缓存；每行 Ctrl+N 快捷键；"展示更多"8→1000 两级；动作面板（→ 进入，重命名内联编辑，copy_to/move_to 带文件夹选择）；图标按扩展名/路径键 LRU 缓存（128 条，多 DPI 源）；网页 favicon 联网缓存（磁盘 LRU 64 条）。

---

## 2. 对标

| 维度 | Everything 1.5 Beta | Listary V7（7.0.0.9） | Prism | 差距判定 |
|---|---|---|---|---|
| 扫描引擎 | 多线程 SIMD strstr 线扫（官方 t=9463） | 新引擎（宣称 +20%~100%） | 多线程线扫，无 SIMD | **主要差距**：ASCII 子串扫描 SIMD 化（memchr）预估再 2-4×，待依赖审批 |
| 内存 | ~75-100MB/百万（Service 索引期泄漏已修至 ~10MB） | 宣称 -30%（旧基准未公开） | ~40MB/百万 | **Prism 领先**，保持 |
| 索引更新 | 后台更新不阻搜索、fast reindexing、1.5 修"重启后更新停止" | 重做自定义索引 + 故障排查工具 | USN + 按卷缓存 + 流式 checkpoint | 对齐；但恢复策略收敛性有缺口（H1/M1） |
| 搜索语法 | 属性索引/搜索/排序（日期、大小等，属性集可配） | **文本+路径+日期+大小组合高级语法** | ext:/path: | **功能差距**：无日期/大小/属性过滤 |
| 零输入体验 | 空结果即无 | **Launcher 未输入先推荐最近/常用文件** | 仅根目录范围内历史 MRU | **体验差距**：全局空查询无推荐 |
| 结果交互 | 多选、预览面板（1.5） | **多选、升级预览面板** | 单选 | 功能差距（低优先，Prism 是启动器形态） |
| 拼音/中文 | 无 | 无公开拼音支持 | 全拼+首字母 sidecar | **Prism 独有优势** |
| 文件系统 | NTFS + 1.5 FAT | NTFS + 自定义目录索引 | 仅 NTFS | 差距已知（此前评估：USB/FAT 三档优先级路线图在案） |

---

## 3. 改进意见（按价值排序）

1. **修恢复策略收敛性（前置）**：H1（rebuild 队列丢新）与 M1（重建退避）不是搜索功能本身，但直接决定"搜出来的是不是最新"——搜索工具的可信度底线。详见 PRISM-FRESH-AUDIT-3 批次 H+M1。
2. **memchr SIMD 子串扫描**（需新依赖审批）：Prism 与 Everything 剩余的核心引擎差距。`find_case_insensitive` 热路径换 `memchr::memmem`（ASCII needle 直接走 SIMD；非 ASCII 保持现零分配折叠路径）。预估 1.2M 记录 ASCII 扫描 10ms→3-5ms。协议/格式零改动，风险集中在依赖引入与字节等价测试（锚定：SIMD 路径与现路径结果逐字节相等）。
3. **日期/大小过滤语法**（对标 Listary V7）：`size:>10mb`、`datemodified:2026` 类。**前提是索引侧加属性**——当前槽表 12B 无 size/mtime 字段，加字段 = v6 格式 + 全量重扫，代价大。**建议降级实现**：利用 USN 记录与 MFT $STANDARD_INFORMATION 在**构建时**落 sidecar 式的稀疏属性表（仅 size/mtime 两列，u64×2 = 16B/条，1M 文件 16MB），过滤时二次筛 Top-K 之后的候选（≤1000 条，逐条查表），扫描路径零改动。风险：sidecar 维护与 delta（与拼音 sidecar 同款机制可复用）。
4. **空查询全局推荐**（对标 Listary V7 Launcher）：空查询 + 无 root 时出"最近使用/常用"历史 MRU（历史已有 weights() 全量数据，只是当前空查询仅在 root 范围启用）。改动集中在 ipc.rs 空查询分支放开 root 限制 + 前端空查询不再停留 Idle。风险低；注意防与"呼出即搜 CWD"的心智冲突（可作为设置项）。
5. **FAT/exFAT 支持**（对标 Everything 1.5）：此前已有三档评估（USB 移动硬盘 0.5 天 → FAT/exFAT 3-5 天 → 网络盘 5-8 天），VolumeIndex 结构与 MFT 解耦可复用，FindFirstFile 枚举 + 变更检测靠定时重扫。维持该路线图，触发条件不变（用户提出需求再做）。
6. **不建议跟进**：预览面板/多选（Prism 定位启动器非文件管理器）；属性全量索引（Everything 自己都靠"限制属性集"控内存，Prism 内存优势不值得换）；进程合并（Listary 7.0.0.9 并回主进程是配合其引擎重写的决策，Prism 三进程的权限分离与管道治理是资产，两轮独立审计均未发现死锁/泄漏实证）。

---

## 4. 一句话结论

Prism 的搜索栈在**内存效率（~40MB/百万文件）、中文拼音（独有）、索引更新纪律（按卷缓存/流式 checkpoint）**上对标不落下风甚至领先；剩余差距按性价比排序是：恢复策略收敛性（必须修）> SIMD 扫描（待审批）> 日期/大小语法（有降级实现路径）> 空查询推荐（小改动）> FAT（按需）。Everything 与 Listary 2026 年的动作（Service 内存修复、引擎重写、Launcher 推荐）没有改变这个格局。

## 5. 来源
- [Everything 1.5 Beta 讨论帖](https://www.voidtools.com/forum/viewtopic.php?t=9787)、[Service 内存泄漏修复 t=14437](https://www.voidtools.com/forum/viewtopic.php?t=14437)、[1.5 官方页](https://www.voidtools.com/everything-1.5/)、[属性索引内存讨论 t=11234](https://www.voidtools.com/forum/viewtopic.php?t=11234)、[索引算法 t=9463](https://www.voidtools.com/forum/viewtopic.php?t=9463)。
- [Listary V7 Beta 公告（7.0.0.9）](https://discussion.listary.com/t/listary-v7-beta-is-here-the-launcher-now-recommends-plus-a-new-engine-multi-select-fresh-themes-updated-to-7.0-0-7-on-july-20/10259)、[官方 changelog](https://dl.listary.net/changelog.html)、[beta changelog](https://help.listary.com/changelog-beta)。
- [Wojciech Muła sse4-strstr](https://github.com/WojciechMula/sse4-strstr)、[SIMD-friendly substring search 讨论](https://news.ycombinator.com/item?id=44274001)。
