# Prism 全新独立审计与修改方案 4（2026-08-20 深夜）

本轮在实施方案 4（PRISM-IMPL-PLAN-4-2026-08-20，五批次：H1+M1+M2 / M3+matched_count+S1+S2 / S3+S4 / M4+M5 / L×13）全部落地、全量门禁绿（cargo 373 / dotnet 221 / clippy 0 警告 / 基准 -65% / 真机 A/B 验证 / 安装包重建）之后进行。方法与前几轮相同：**两个独立审读代理从零通读**（Rust 25 文件、C# 全量），**刻意不读 docs/、不读 git 历史、不读 .trellis/**，避免锚定；叠加 2026-08 联网对标。

审计当场发现并已修复 2 项（§1）。其余 14 项发现（中 3 / 低 11）形成本方案。

---

## 1. 审计当场修复（已提交、已部署、已带回归锚）

### 修 1（高，批次 5 引入的回归）：history 定时冲刷线程无条件置脏 → 空闲时每 250ms 全量落盘

`should_persist_now()` 无条件 `gate.dirty = true`——这对 record 路径正确（先改内存再标记），但批次 5 的定时线程复用了同一函数，自身没有变更却每次把自己标脏：**无论有无用户动作，每 250ms 全量 clone + JSON 序列化 + fsync + ReplaceFileW 一次，进程全生命期持续**（历史 5000 条 ≈1MB 时约 4MB/s 持续写盘 + 4 次/秒 fsync）。

修法：拆出定时线程专用的只读判定 `persist_if_due_and_dirty()`（到期且脏才落盘并清脏，绝不置脏，另补 `is_enabled` 检查）。回归锚 `periodic_flush_never_marks_dirty_on_its_own`：空闲多拍文件不出现、两条 record 后到期冲刷、冲刷后回到静默。

### 修 2（中）：单卷重建后拼音 sidecar 不失效 → 记录号复用产出错误命中，最长 6h 不自愈

sidecar 按 `(volume 槽位, MFT 记录号)` 键控；单卷重建成功路径只做 `merge_and_publish` + 重启 watcher，无任何拼音动作。重建窗口内变更过的文件：记录号被 NTFS 复用后旧编码配上新名字 → 拼音查询命中无关文件（spans 错位）；新建中文名 → 漏检。自愈要等下一次全量拼音重建（最长 6h checkpoint）。

修法：单卷重建成功后 `begin_pinyin_rebuild()`（立即卸载陈旧 sidecar——宁暂时少结果不错结果）+ 置 `pinyin_needs_rebuild`，交 maintenance tick（M3 的 60s 退避）从 live 索引重建。

---

## 2. 联网对标（2026-08 更新）

| 维度 | Everything 1.5 Beta（1366a~1391a，2026-05~08） | Listary V7（7.0.0.9，2026-08-05） | Prism 现状 |
|---|---|---|---|
| Service 内存 | 1366a 修 Service 泄漏：索引期 ~10MB、空闲 ~1MB | 引擎重写（Rust）：内存 -30%、速度 +20%~100%，主打**峰值内存**下降 | 索引服务 ~74MB 常驻（2.4M 记录），仍占优 |
| 恢复策略 | journal 满 → 全量重建（1.4 起只重建受影响卷）；db "no longer sorted" 即判 corrupt → 自动重建 | 7.0.0.9主打稳定性增强 + 索引故障排查工具 | 按卷缓存/回放/单卷重建/退避——本轮 H1/M1/M2 后收敛性对齐 |
| 架构 | Service + UI | **listary-core 并回主进程**（简化架构、提升稳定性） | 三进程维持（前几轮已论证不并回：SYSTEM 权限分离 + broker 普通用户 IFileOperation 对话框） |
| db 校验 | 结构校验（sorted）+ corrupt 自动重建；1.5 比 1.4 更抗损坏 | — | v5 只有 magic+version+validate，**无内容 checksum**（见发现 6） |

结论：Prism 的恢复策略与内存纪律在对标下不落下风；本轮剩余差距集中在**边角正确性**（发现 2/5/6/7 类）与**前端卡顿面**（C# 发现 1/2）。

---

## 3. 剩余发现（中 3 / 低 11）与实施方案

### 中（3 项）

**A1. USN 增量与拼音搜索并发时整份 sidecar COW 深克隆**（`indexer_runtime.rs` apply_pinyin_records 的 `Arc::make_mut`；`ponytail:` 注释已自认天花板）
击键驱动搜索几乎总在飞 + 浏览器/WU 持续写盘 → USN 批次与 Arc 快照交叠时每批次深克隆整个 sidecar（几十 MB），分配 churn + 瞬时 2× 常驻。
**修法**：delta 表从 `PinyinSidecar` 拆出为独立 `RwLock<BTreeMap<RecordKey, Option<Vec<u8>>>>`（搜索侧读主表+delta，watcher 只写 delta），COW 彻底消失；M3 的汉字计数与上限逻辑随表迁移。
**影响面**：sidecar 数据结构、search 路径、apply 路径三处。**风险**：中——delta 语义（掩蔽陈旧编码）必须逐条对齐，现有 delta 测试全部保持绿即可控。字节格式不动（delta 本就不序列化）。

**A2. Ctrl+G 切回「当前目录」在 UI 线程持锁做同步磁盘 I/O**（C# `HostScopeController.ToggleScope` → `RootValidation.Validate`：Exists + 目录枚举探权限）
网络盘掉线/机械盘休眠/SMB 超时时 UI 冻结数秒，且 `_gate` 锁把 STA 线程的 Ctrl+Enter 联动一并拖住。同类的 `ComputeCapture` 已在 STA 后台线程，唯独这条漏了。
**修法**：`Revalidate` 投递到既有 STA 常驻线程，结果回 UI 提交变更。**影响面**：HostScopeController 单入口。**风险**：低——Revalidate 是纯查询，换线程不改语义；`Changed` 订阅方已 BeginInvoke 安全。

**A3. ResultList 行同步键比较每次分配新字符串，最坏 O(n²)**（C# `SearchResult.ContainerKey` 计算属性 + `HasSameKey`）
展开 1000 行态每击键 ~2000 次字符串分配（~200KB+/键）；重排最坏 O(n²/2) 比较。表现为「展示更多」后继续输入掉帧。
**修法**：ContainerKey 惰性缓存（每行 1 次分配）。O(n²) 的 FindByKey 前缀有序路径天然少触发，缓存落地后可接受。**风险**：极低——同实例恒同键。

### 低（11 项摘要）

| # | 位置 | 问题 | 修法 | 风险 |
|---|---|---|---|---|
| B1 | root_scope + root_bound_cache | 洪峰期 root 解析缓存按 generation 失效，每次击键全表扫描持读锁 | 缓存失效粒度放宽到「被解析卷自身 next_usn」（与流式 checkpoint 放宽同思路）；缓存槽 1→4 | 低 |
| B2 | needs_name_compact 标志 | 标志先消费后竞争失败且卷转安静 → 死名字内存永不回收 | tick 直接调 `compact_volumes_off_lock`（内部谓词选目标），不依赖一次性标志 | 极低 |
| B3 | index_cache.rs | v5 缓存无内容 checksum（sidecar 有）：名字池单字节静默损坏跨重启存活 | envelope 加 names/nodes 哈希（缺省 0 跳过校验，旧文件兼容） | 低 |
| B4 | zip.rs Shell COM 回退 | CopyHere 异步当成功上报；先覆写已有 zip | 传 FOF_WAITUNTILDONE + 轮询目录条目数稳定；输出已存在报 Conflict | 低 |
| B5 | ipc.rs handle_connection | 前端单连接串行：一次 8s 慢搜索冻结后续全部请求 | search 请求 tokio::spawn + 按序号写互斥（保响应顺序），或前端多连接（broker 已支持 8 连接） | 中（乱序响应需前端配合，倾向按序号互斥） |
| B6 | C# SearchViewModel 轮询 | 索引构建轮询 SearchAsync 无取消令牌，wedge 时占通道锁 8s | 快照 `_searchCts?.Token` 传入（只影响等锁，不破配对读纪律） | 低 |
| B7 | C# SearchWindow 隐藏 3 分钟 GC | 阻塞 Gen2 压缩与再呼出竞态（违背 A4 承诺） | Tick 先查 `IsVisible \|\| _hiding` 再收集 | 极低 |
| B8 | C# FaviconCache | 端口 origin 落盘名含 `:`（NTFS ADS 语义）→ favicon 静默失效且脱离 LRU | `Replace(':', '_')` 统一 KeyToFileName | 极低 |
| B9 | C# IconCache | 无扩展名路径的图标键在 UI 线程 Directory.Exists 探盘 | 键计算整体挪进 GetAsync 的 Task.Run 体 | 低 |
| B10 | C# PipeClient.AdoptExistingServer | 两处复用返回不 Dispose Process，句柄延迟回收 | return 前 Dispose | 极低 |
| B11 | C# ResultList.SetWebIconProvider | 换 provider 时旧 Resolved 订阅未摘除（当前无实际触发，防御性） | 开头 `_webIcons.Resolved -=` | 极低 |

### 实施排期建议

- **批次 A（正确性/卡顿，下轮首选）**：A2 + A3 + B7 + B10 + B11（C# 五项小 diff 一起）＋ B2（Rust 一处）。
- **批次 B（拼音 delta 拆锁）**：A1 单独一批（结构性改动，测试锚全保）。
- **批次 C（边角）**：B1 + B3 + B4 + B6 + B8 + B9。
- **批次 D（协议并发，需设计确认）**：B5——响应序号方案先在文档定稿再动。

### 明确不做 / 维持

1. **三进程维持**（Listary 并回主进程是它的引擎重写配套决策，Prism 的权限分离收益仍在）。
2. **不做属性索引**（Everything 的 t=11234 结论：属性索引内存代价大，按需限定文件夹才划算——Prism 的日期/大小走稀疏 sidecar 路线已在搜索报告定稿）。
3. **memchr SIMD / 倒排 / trigram**：维持前几轮结论。

---

## 4. 来源
- 代码：两个独立审读代理从零通读（Rust 25 文件 / C# 全量），未读 docs/ 与 git log。
- Everything：[1.5 Beta 讨论 t=9787](https://www.voidtools.com/forum/viewtopic.php?t=9787)、[Service 泄漏修复 t=14435](https://www.voidtools.com/forum/viewtopic.php?t=14435)、[t=14437](https://www.voidtools.com/forum/viewtopic.php?t=14437)、[1391a 内存报告 t=16351](https://www.voidtools.com/forum/viewtopic.php?t=16351)、[属性索引内存 t=11234](https://www.voidtools.com/forum/viewtopic.php?t=11234)、[journal 尺寸与重建 t=14153](https://www.voidtools.com/forum/viewtopic.php?t=14153)、[db corrupt t=8931](https://www.voidtools.com/forum/viewtopic.php?t=8931)、[官方故障排查](https://www.voidtools.com/support/everything/troubleshooting/)。
- Listary：[V7 官方页](https://www.listary.com/v7)、[beta changelog](https://help.listary.com/changelog-beta)、[官方 changelog](https://dl.listary.net/changelog.html)、[V7 Beta 公告](https://discussion.listary.com/t/listary-v7-beta-is-here-the-launcher-now-recommends-plus-a-new-engine-multi-select-fresh-themes-updated-to-7.0-0-9-on-august-5/10259)。
