# Prism 全新仓库审计与优化方案（2026-08-19）

本文档基于**从零开始的完整代码阅读**（不依赖既往审计文档的结论），范围：`src/prism-core`（Rust 索引/搜索/IPC 核心，约 15k 行）与 `src/Prism`（WPF 前端，约 13k 行）的关键路径，并对照 voidtools Everything 与 Listary 的公开技术资料（来源见文末）。

实测量基准（本机）：索引缓存 `index-v5.bin` 39.7 MB，拼音 sidecar 128 KB（说明本机中文文件名占比很低；下文内存估算同时给出中文重度用户的数字）。

---

## 1. 对标结论：Prism 与 Everything 的架构映射

Everything 的核心做法（全部有 voidtools 官方来源）与 Prism 现状：

| Everything 做法 | Prism 现状 | 评价 |
|---|---|---|
| 首建一次性顺序读 MFT，之后只读 USN | `ntfs.rs` `FSCTL_ENUM_USN_DATA` 全量枚举 + USN 回放，同构 | 持平 |
| USN Journal 建议 32MB max / 8MB delta | `query_or_create_journal` 同参数 | 持平 |
| 轮询 USN 约 1 秒 | `read_changes` 内核阻塞等待，最长 250ms | **Prism 更好**（CPU 与延迟双赢） |
| DB 退出保存 + 1.5 每日自动保存 | 6 小时 / 50 万事件 checkpoint + 停机保存，原子替换 | 持平偏好 |
| 启动加载 DB → USN 前滚补齐 | `load_cached` → `validate_checkpoints` → 发布 → watcher 前滚 | 持平 |
| 日志包装/卷变更 → 丢弃缓存重建 | 同（`CachedLoad::Miss`） | 持平，见 S2 的改进点 |
| 服务/非服务分离，服务只暴露文件名 | PrismIndexer 服务 + broker + UI 三进程，管道 ACL 收窄 | 持平 |
| 内存 ~100MB/百万文件（含 size/date 元数据） | NodeSlot 12B + 名字池 ~25B/文件 ≈ **~40MB/百万文件**（不含元数据） | **Prism 更省**（代价：不支持按大小/日期过滤，属功能取舍） |
| 名字池按字典序 front-coding 压缩（磁盘 45MB vs 内存 100MB/百万） | 原始 UTF-8 名字池，无压缩 | 见「明确不做」：voidtools 自己试过压缩父指针并放弃；增量 USN 下维护有序压缩池复杂度不成比例 |
| 搜索 = 多线程 SIMD strstr 线性扫全库，无倒排/后缀索引 | **单线程**朴素 `windows()` 字节扫描（`hierarchy.rs::find_case_insensitive`） | **最大差距**，见 N1 |
| 重建期间先展示旧 DB（1.5） | 首建逐卷 `merge_and_publish`，可边建边搜 | 持平 |
| DB 损坏即弃，可随时从 MFT 重建 | 缓存即纯缓存，`validate` + 原子替换 + SCM 重启兜底 | 持平 |

Listary 的教训（官方 changelog / 论坛）：
- 6.3 beta `listary-core.exe` 曾泄漏至 9.65GB；后续版本靠诊断工具与持续修复收敛 → **常驻进程必须有内存趋势可观测性**（见 S5）。
- v6.3.6.99 因**进程间 gRPC 崩溃**把引擎并回主进程。Prism 用命名管道 + 看门狗重连，此前审计已论证三进程拆分合理（且 Prism 管道协议有协议版本握手、有界行读、连接上限），**不跟随合并**。
- Listary 靠全局钩子 DLL 注入宿主实现文件对话框集成，伴随稳定性与 AV 误报争议。Prism 的 HostAdapter 路线（ExplorerHostAdapter / DirectoryOpusHostAdapter，无注入）**保持不变**。

---

## 2. 已确认无需再动的部分（避免重复审计）

以下在本次通读中确认设计良好，后续审计可跳过：USN 内核等待读（复用缓冲、无轮询空转）；MftRecord 池化（R-C1 已做）；槽表尾部回收（R-C3）；名字池死字节计数压缩触发；`parse_usn_buffer` 的 lossy UTF-16；管道 4 listener + 握手超时 + 64 连接上限 + ACL（GA 收窄为 GRGW）；BoundedLineReader 双向上限；checkpoint 期间锁外 clone + 抽样 validate；pinyin 侧加载在索引读锁外 + delta 上限 4096 + 身份哈希校验；前端全局异常三钩子、懒创建窗口、空闲 GC（且不再 TrimWorkingSet）；历史 `weights()` 返回 `Arc<[..]>` 快照；SCM Stop 与长任务 select 竞争关停。

---

## 3. 修改建议

编号规则：N=核心搜索性能，M=内存，S=稳定性。**顺序即建议实施顺序**（风险递增、收益递减）。

### N1（前置，零风险）——搜索热路径零分配化：非 ASCII 名字的大小写折叠

**现状**：`hierarchy.rs::match_metadata` 与 `find_case_insensitive`（805-848 行）对每个非 ASCII 文件名、每次查询调用 `name.to_lowercase()`——堆分配一个完整副本。中文重度用户（文件名大多含汉字）每次击键对数百万名字产生数百万次 String 分配，直接拖慢扫描并抬高分配器压力。同病：`QueryFilters::path_matches`（hierarchy.rs:912）对每个候选路径做两次 `to_lowercase()`（仅在 `path:` 过滤时触发，影响小）。

**改法**（约 60 行，纯函数级修改）：
1. 查询为纯 ASCII 时：走字节级 `windows()` + `to_ascii_lowercase()` 比较，**不构造 lowered 名字**（UTF-8 自同步性保证 ASCII 字节匹配位置必为字符边界，语义安全）。
2. 查询含非 ASCII 时：先零分配扫描名字 `char::is_uppercase()`——无大写字符（典型中文名）直接 `name.find(query_lower)`；有大写字符才回落现有 `to_lowercase()` 路径。
3. `path_matches` 同理：needle 预降幂一次（循环外），路径仅在含大写字符时降幂。

**收益**：中文环境每次击键消除 ~百万级堆分配，扫描吞吐预计提升 1.5-3 倍（分配占大头时）；英文环境不变。
**影响面**：`hierarchy.rs` 两个函数 + `ipc.rs` 无关路径不动；排序语义不变（position 仍按 UTF-16 计）。
**风险**：低。风险点是 ASCII 快路径的字符边界论证与 `to_lowercase` 长度变化（如 `İ`）——后者只在含大写字符回落路径出现，与现状一致。补 4-6 个单测（纯 ASCII 名 + 中文查询、含大写拉丁的中文名、混合代理对）即可守护。
**验证**：现有 `search_volumes` 全部单测 + 新增用例；cargo test。

### N2（旗舰，中风险）——并行扫描 + memchr 子串搜索

**现状**：`search_volumes`（hierarchy.rs:933）单线程顺序扫全部卷全部槽。Everything 官方口径："optimized multi-threaded strstr on every single filename"——线性行扫描 + 多线程 + SIMD 是它亚毫秒响应的全部秘密，没有倒排索引。Prism 本机 ~1.2M 记录、39.7MB 名字池，单线程一次全扫约 10-40ms（击键间隔 50ms 防抖下勉强够用）；百万级以上中文库会顶到防抖上限，UI 手感下降。

**改法**（分两步，可只做第一步）：
- **第一步（无新依赖）**：`std::thread::scope` 把每卷 `nodes` 按 ~256K 槽分块并行扫描，每块局部 Top-K（复用 `RankedCandidate` 序），最后归并全局 Top-K。RootFilter/QueryFilters 每块独立构造（memo 每块上限已是 4096，语义不变）；`RwLockReadGuard` 留在 spawn_blocking 线程，块内共享 `&IndexState`（`Sync` 成立）。排序确定性由既有 tie-break（name/mount/record）保证，结果与单线程逐字节一致。
- **第二步（新增 memchr crate，~30KB）**：`find_case_insensitive` 的 ASCII 路径用 `memchr::memchr_iter` 定位查询首字节再窗口比对，替代朴素 `windows().position()`。memchr 是 Rust 生态事实标准（ripgrep 同源），SIMD 加速 4-16 倍。

**收益**：4-8 核上扫描吞吐 3-6 倍；配合 N1 后，3M 记录库单次全扫预计 <10ms，达到 Everything 量级手感。拼音 `PinyinSidecar::search_in_root`（pinyin_sidecar.rs:362，同样线性扫 records）可复制同一分块模式，作为二期。
**影响面**：`hierarchy.rs::search_volumes` 重构 + `Cargo.toml`（第二步）；`SearchOutcome` 统计字段（scanned_nodes 等）需按块累加，协议不变。
**风险**：中。
- 线程启动开销：scope + 4-8 线程每次击键 ~20-50µs，可忽略；但要注意击键风暴下不要每 50ms 防抖都起满线程——块数取 `available_parallelism` 与记录数下限。
- 正确性：全局 Top-K 归并必须保持与单线程相同的序（`RankedCandidate::Ord` 已全序，无并列歧义）；分块边界不撕裂单个名字（名字在池中，槽自包含，天然安全）。
- 死锁面：只读共享，不新增锁；`spawn_blocking` 池并发搜索时线程数 = 连接数 × 块数，64 连接上限下有界（最坏 64×8=512 线程，为 tokio blocking 池默认上限，恰好贴边——实施时给单次搜索的并行度设为 `min(可用核, 8)`，并在文档注明该上限组合）。
**验证**：先加一个确定性基准测试（构造 3M 合成槽，断言并行与串行结果逐字节相等 + 延迟阈值），再切生产路径；现有 2000 行 hierarchy 单测全部保留作为等价性守护。

### M1（低风险）——首建内存峰值削减

**现状**：`ntfs.rs::build_volume_with_progress` 全量物化 `Vec<MftRecord>`（24B/条）+ name_pool，然后在 `VolumeIndex::upsert` 循环里把名字**再复制一份**进 `volume.names`。峰值 ≈ 24B×N + 2×名字字节。3M 记录卷峰值 ~190MB，常驻落定 ~95MB。另外 `names` 池靠 `extend_from_slice` 摊销倍增增长，60MB 池在扩容瞬间翻倍到 120MB 额外尖峰。

**改法**（两处独立小改，合计 ~30 行）：
1. `prepare_initial_capacity` 之后用 `records.len() × 24B` 预留 `volume.names` 容量（枚举完成时记录数已知，均值估计即可），消除倍增尖峰。
2. upsert 完成后 `name_pool` 已可释放——现在它是函数局部变量自然释放，确认无引用泄漏即可（通读确认无泄漏，此项主要是预留容量）。
3. （可选，收益减半即止）不做「边枚举边建索引」：MFT 记录序不保证父先于子，增量 upsert 会把延迟重试列表放大，复杂度换 ~60MB 峰值不划算。**ponytail 决策：只做预留，不重构建库流程。**

**收益**：3M 记录卷建库峰值从 ~190MB 降到 ~150MB（消除 names 倍增尖峰约 40-60MB）；小库收益小。
**影响面**：`ntfs.rs` 单函数。
**风险**：极低。预留过大使小卷多占内存——用 `min(estimate, live_records × 64B)` 封顶。

### M2（中风险，可决策不做）——checkpoint 逐卷流式序列化，消除 2× 内存尖峰

**现状**：`indexer_runtime.rs::checkpoint` / `persist_first_build_with` 在读锁内 clone 整个 `IndexState`（本机 ~40MB，3M 记录库 ~100-190MB），锁外序列化。写侧峰值 = 索引 + clone ≈ 2×。每 6 小时一次 + 停机一次。

**改法**：自定义 `Serialize` 包装（`SnapshotView<'a>(&ServiceState)`）：序列化 envelope 头字段后逐卷序列化，**每卷单独短暂取读锁**，卷间核对 generation——变了就返回错误中止本次保存，外层重试（上限 3 次后回落现有 clone 路径）。postcard 字节流与现有格式逐字节相同（serde 序列化顺序不变），load 侧零改动。

**收益**：大库 checkpoint 期间私有内存少 ~100-190MB；USN 写者被锁时间从「整个 clone」变为「每卷序列化 ~百毫秒」。
**影响面**：`index_cache.rs` 新增包装类型 + `save` 签名从 `&IndexState` 变为回调式；`indexer_runtime.rs` 两个调用点。
**风险**：中。自定义 Serialize 里持锁边界容易写错（必须保证 guard 不跨 `serialize_element` 存活）；USN 洪峰下重试循环可能连续中止——有 3 次回落兜底。**决策点**：如果认为 6 小时一次的 2× 瞬时峰值可接受（本机 40MB 完全无感），此条降级为「3M 记录以上用户出现后再做」，不阻塞其他批次。参考：Everything 1.4 干脆阻塞整个保存（680MB 保存 4 秒），Prism 现状已优于它。

### S1（中低风险）——名字池压缩挪出写锁

**现状**：`watch_volume`（indexer_runtime.rs:1440）在**索引写锁内**调用 `compact_names_if_needed`——压缩是对整卷 nodes+names 的 O(N) 重建（3M 记录卷 ~100MB memcpy，数百毫秒）。期间所有搜索（读锁）与兄弟卷 watcher（写锁）全部停摆。删除密集场景（git clean、构建产物清理）会规律性触发。

**改法**：压缩改为「克隆 → 锁外压缩 → 短锁换入」：
1. watcher 只置 `needs_compact` 标志（写锁内 O(1)）。
2. maintenance tick（5 秒节拍）读锁下 clone 目标卷 → 锁外 `compact_names_if_needed` → 写锁内校验该卷 `next_usn` 未变则整卷 `swap`（两个 Vec 指针交换，微秒级），变了则丢弃重试（有界重试 3 次，放弃本轮）。
3. USN 事件在克隆与换入之间到达导致的不一致由 `next_usn` 校验兜住，语义与现有 `snapshot_mutations` 回滚同族。

**收益**：消除写锁最长持锁段；删除风暴期间搜索无感知。
**影响面**：`indexer_runtime.rs` watcher 分支 + maintenance 分支；`hierarchy.rs` 不动。
**风险**：中低。克隆-换入窗口的 USN 丢失是核心风险，靠 next_usn 比对 + 重试放弃闭环；压缩从「每万事件即时」变为「最迟 5 秒后」，内存回收延迟可忽略。单测：压缩窗口内注入 USN 事件，断言事件不丢（next_usn 前移后换入被拒）。

### S2（中风险）——缓存按卷生效，卷集变化不再全量弃缓

**现状**：`load_cached` / `validate_checkpoints`（indexer_runtime.rs:1309）整体校验：卷数不同、任一卷 journal 不可回放 → 整份缓存作废 → 全盘 MFT 重扫（本机几分钟，大库更久）。Everything 的粒度是**每卷独立**：只重建坏卷，好卷照常服务。

**改法**：`validate_checkpoints` 改为逐卷判定，返回「可回放卷集合 + 需重建卷集合」；可回放卷直接 `merge_and_publish` + 起 watcher，需重建卷走现有首建循环（该循环已支持逐卷发布）。卷数增加/减少、单卷 journal 包装（长期关机后常见）都只付单卷代价。

**收益**：新增硬盘、U 盘换插（若纳入索引）、长期关机后重启的恢复时间从「全盘」降到「受影响卷」。
**影响面**：`indexer_runtime.rs::load_cached`/`acquire_initial_index` 的分支逻辑；`CachedLoad` 枚举加 `Partial` 变体。
**风险**：中。部分发布期间的状态机（building 标志、R2 持久化门、pinyin 身份哈希按卷数计算）都要过一遍——pinyin `index_identity` 含 `volume_count`，部分发布后重建期间卷数会变，sidecar 会多触发一次重建（正确但慢一点，可接受）。需补「缓存 2 卷、现场 3 卷」的集成态单测。
**优先级说明**：固定盘场景触发频率低（加分块 U 盘不在索引范围），实际收益集中在长期关机用户。排在 S1 后。

### S3（低风险）——apps 清单扫描重试放开

**现状**：`main.rs` 开始菜单扫描 5 次×30s 后**永久放弃**，应用搜索空到进程重启。安装器锁目录的窗口期（系统刚启动、MSI 正在装软件）可能超过 2.5 分钟。

**改法**：5 次后改为指数退避到 1 小时一次，成功即停，永久重试。~10 行。
**收益**：消除「装完软件搜索不到应用必须重启」类反馈。风险：极低。

### S5（低风险，观测性）——索引内存趋势入事件日志

**现状**：`status()` 已算 `memory_bytes`，但只回应前端显示，历史无沉淀。Listary 9.65GB 泄漏正是靠用户截图才发现。

**改法**：maintenance tick（已有 5 秒节拍）每小时把 `(generation, events_since_checkpoint, memory_bytes, pinyin_status)` 写一行到事件日志（logging::event_detail 已有设施）。
**收益**：泄漏/异常增长可在用户报告前从日志定位；零常驻成本。风险：极低（注意日志量，1 小时 1 行）。

---

## 4. 明确不做（附理由，防止未来重复提案）

1. **名字 front-coding 压缩**：Everything 磁盘格式用它省一半，但那是**静态有序**库；Prism 名字池随 USN 高频追加，维护有序压缩偏移的成本（voidtools 自己都放弃了父指针压缩，「no meaningful ram usage decrease for the added complexity」）远超 ~25B/文件的收益。Prism 每 file 常驻已比 Everything 少 60%。
2. **倒排/trigram/后缀索引**：Everything 用多线程线扫就达到了目标；倒排让内存翻倍且增量维护复杂，与「40MB/百万文件」的内存优势直接冲突。
3. **broker 并回 UI 进程**：Listary 合并是因为 gRPC 崩溃面；Prism 命名管道 + 协议握手 + 看门狗重连没有该问题，拆分的权限隔离（broker 用户态、indexer 服务态）是安全资产。
4. **DLL 注入式文件对话框集成**：Listary 路线的稳定性与 AV 误报代价已被社区反复验证；Prism 无注入 adapter 是正确取舍。
5. **UI 轮询改 WaitGeneration 推送**：`PollUntilReadyAsync` 的每次轮询同时携带进度与部分结果，WaitGeneration 只能发信号，改了反而丢进度刷新；轮询有 30 次上限，成本可控。
6. **MFT_ENUM_DATA 升级 V1/V2**：对纯名字枚举收益微小，V0 + 256KB 缓冲已达标（本机 39.7MB 缓存几分钟内建成）。

---

## 5. 实施批次与总体风险控制

| 批次 | 内容 | 预估规模 | 风险 |
|---|---|---|---|
| B1 | N1 零分配化 + S3 + S5 | ~150 行 | 低 |
| B2 | N2 并行扫描（第一步 std::thread::scope；memchr 第二步视 B2 实测决定） | ~250 行 + 基准测试 | 中 |
| B3 | S1 压缩出锁 + M1 建库预留 | ~200 行 | 中低 |
| B4 | M2 流式 checkpoint、S2 按卷生效 | ~350 行 | 中（两条互相独立，可只做其一） |

每批次收尾：`cargo test`（prism-core 约 29 文件全绿）+ 本机安装包重建（Inno Setup，见 reference-inno-setup-path）+ 推送前 `--release` 冒烟（呼出、搜索、删除文件后 USN 收敛、重启缓存命中）。

**全局回归守护**：N2 的等价性基准测试（并行 vs 串行逐字节相等）是本方案唯一新增的长期测试资产，其余全部复用现有单测。所有改动不触碰 IPC 协议（BROKER_PROTOCOL / INDEXER_PROTOCOL 不升级）、缓存格式（v5 不变）与排序语义（MatchMetadata::Ord 不变），前后端可独立发布。

**建议先做的一次性测量**（B1 前，半小时）：临时在 `search_volumes` 计时打点，本机实测单次全扫延迟与 `to_lowercase` 占比，用于校准 N1/N2 的预期收益——若本机实测全扫已 <5ms，N2 可降级为「中文重度用户报告卡顿后再做」。

---

## 6. 调研来源

Everything：voidtools FAQ（faq/）、DB 格式文档（support/everything/db/）、Options 文档、Everything Service 文档、1.5 页面；论坛帖 t=9463（索引算法与「multi-threaded strstr」原话）、t=12779（USN 机制）、t=8318/t=14053/t=7700/t=8623（内存数字）、t=14107（保存速度/自动保存）、t=10670/t=14486（journal 尺寸建议）、t=11234（父指针压缩放弃）、t=14335/t=14435（服务与泄漏修复）。

Listary：官方帮助（options-index、changelog-beta）、论坛（discussion.listary.com t=8875 9.65GB 泄漏、t=9405 v6.3.6.99 合并进程公告、t=3923 MFT 需管理员、t=5082 钩子 DLL）、HN item=33820859。

对比项：PowerToys Run（GitHub issues #3365/#20825、Low Memory Mode 报道）、Flow Launcher（issue #2940）、ueli（Electron 基线）。
