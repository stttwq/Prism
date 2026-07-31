# G9 首建可用性与进度

## Goal

消除首次安装后"文件搜索分钟级完全不可用"的窗口：把索引首建从"全部卷建完才发布"改为"每卷建完即发布并立即开始 USN 监听"，并让前端能显示真实进度而不是无限期"请稍候"。本阶段不改变搜索排序语义，不改变缓存格式的兼容性承诺。

## Background

`0296463` 实装 USN 实时监听后，稳态新增文件延迟已降到秒级，但**首建本身没有被加速**——USN watcher 只在首建完成后才启动。核实到的当前行为（`indexer_runtime.rs`、`ntfs.rs`）：

- `run()` 用 `spawn_blocking(load_or_build).await` **等全部卷建完**，才执行 `state.publish(index)`；在此之前 `state.index` 是 `None`。
- `status()` 因此返回 `ready: false`，`search()` 直接返回 `Err("file index is not ready")`。
- broker 的 `indexer_client::search` 见 `!status.ready` 就早退回空 items，**连 Search 请求都不发**。
- `build_all()` 是**串行 `for` 循环**逐卷调用 `ntfs::build_volume`，没有卷优先级，也没有并行。
- `IndexerStatus` 完全没有进度字段（只有 `ready/building/degraded/generation/volumes/memory_bytes/message`），前端只能显示不确定文案。
- 前端已有 `is_indexing` 提示与 `PollUntilReadyAsync`（30 × 500ms）自动补结果，属于已完成的缓解，不是本阶段工作。

三个使本方案可行的结构性事实：

1. **`build_volume` 在枚举之前捕获 USN checkpoint**：先 `query_or_create_journal(&handle)` 拿到 `checkpoint.next_usn`，再 `enumerate_mft`，最后 `replay_until(current.next_usn)` 补齐枚举期间的变化。因此某个卷发布后再启动它的 watcher **不会漏事件**，watcher 从 `volume.next_usn` 续读即可。
2. **`IndexState.volumes` 是 `Vec<VolumeIndex>`，`IndexState::search` 只遍历现有卷**，发布一个只含部分卷的 `IndexState` 在结构上是合法的。
3. **首建期间应用与网页结果已可用**：broker 的 `prefix_results` 在调用 indexer 之前执行，所以缺的只有文件/文件夹。

## Requirements

### R1 逐卷增量发布

- 首建期间每完成一个卷，立即把该卷并入 live index 并发布，使该卷的文件立刻可搜；不再等待全部卷。
- 每个卷发布后立即为该卷启动 USN watcher，不等首建整体结束。
- 发布顺序对搜索结果的唯一影响是"尚未建完的卷暂时没有结果"，不得改变已建卷的排序语义。

### R2 半份索引不得落盘

- 只有全部卷完成后才写 v5 缓存。首建未完成时的任何退出路径（SCM Stop、异常、崩溃）都不得把部分索引保存为看起来完整的缓存。
- 当前 `run()` 退出前无条件调用 `checkpoint(&state, &data_dir)`，必须加"首建完成"门；不能只依赖 `validate_checkpoints` 的 `volumes.len() != descriptors.len()` 兜底。
- 首建中途退出后重启，必须走完整重建，且不得静默使用残缺缓存。

### R3 卷优先级

- 系统卷（`%SystemDrive%`，通常 `C:`）显式排在首位建索引，不依赖 `discover_volumes()` 的字母序巧合。
- 其余卷的顺序稳定可预测，便于基准复跑。

### R4 真实进度上报

- `IndexerStatus` 增加**可选**进度结构，至少含：卷总数、已完成卷数、当前正在建的卷标识、可选的已扫描记录数与总量估算（估算可来自上一次缓存的记录数）。
- 所有新字段可选，旧 broker/前端缺失时行为不变。
- broker 透传给前端；前端把"索引加载中，请稍候…"升级为可解释进度（例如"正在建立索引：C: 已完成，D: 扫描中"）。
- 进度采样不得成为热路径开销：计数用原子量，上报走 status 查询，不新增推送通道。

### R5 与 G1 契约的交互（正确性关键）

- 首建期间返回的结果集是**不完整**的，但这与 G1 的 `is_truncated`（因 `max` 截断）是两种不同的不完整，不得混用同一字段表达。
- 前端增量缓存必须不会在"索引未建完"的结果集上做本地过滤后漏结果。逐卷发布每次 `generation + 1`，而缓存键含 generation，理论上会自然失效——本阶段必须**显式测试**这条路径，不能假设。
- 若 G1 已实现，`is_truncated` 的语义定义中要写明"索引未就绪/未建完不属于 truncated"。

### R6 枚举加速（数据驱动，可为空）

- 以 G0 基线为准评估以下候选，**只实施有实测收益的项**：`enumerate_mft` 的 256KB 缓冲区上调；`build_volume` 中最多 64 遍 pending 重试循环的实际遍数；多卷并行枚举（需区分单物理盘/多物理盘）。
- 任何一项不达预期即记录为"已评估、无收益"，不得为凑数保留复杂度。
- 本项允许全部落空——R1 已能把用户可感知等待从"全部卷"降到"系统卷"，R6 只是加分。

### R7 首建期间既有可用性不得回归

- 应用与网页结果在首建期间保持可用，加测试锁定，防止后续重构把 `prefix_results` 挪到 indexer 调用之后。

## Acceptance Criteria

- [x] 构造多卷环境验证：系统卷建完后即可搜到该卷文件，此时其余卷仍在建索引，`status` 如实反映。
- [x] 卷 A 发布、卷 B 仍在建索引期间，在卷 A 创建/改名/删除文件能被搜索到（证明逐卷 watcher 无事件丢失）。
- [x] 首建中途 SCM Stop 或异常退出后重启，走完整重建，磁盘上不存在被误认为完整的残缺 v5 缓存。
- [x] 系统卷排在首位有确定性测试，不依赖字母序。
- [x] 进度字段为可选，缺失时旧 reader 行为不变；前端显示可解释进度而非无限期等待文案。
- [x] 前端增量缓存在首建期间不会在不完整结果集上本地过滤漏结果，有专门测试。
- [x] 应用与网页结果在首建期间可用，有测试锁定。
- [x] 与 G0 同口径对照：报告"首建到系统卷可搜"与"首建到全部卷可搜"两个时间，以及改动前的"首建到任何文件可搜"。
- [x] R6 各候选项逐条记录实测结果与取舍理由，未采纳项写明原因。
- [x] 三进程总内存仍满足 G0 改写后的门槛；逐卷发布不引入常驻内存增长。
- [x] Rust tests、Clippy `-D warnings`、C# tests 与 WPF Release build 全部通过。

## Out Of Scope

- 改变搜索排序、评分或 Top-K 语义（属 G1）；
- 改变 v5 缓存的持久结构或升级缓存版本（除非 R6 证明必须，且需单独审批）；
- 首建期间的推送式进度通道（保持 status 拉取模型）；
- 目录遍历降级路径的性能优化；
- 拼音 sidecar 的首建（属 G2，但本阶段的逐卷发布框架应可复用）。

## Dependencies

强依赖 G0（首建时间基线与 `opt-level` 对照数据，R6 的唯一依据）与 G1（`is_truncated` 语义、稳定协议形状、C# 测试地基）。

建议排在 G1 之后、G3 之前：它是用户可感知的体验缺口，改动面集中在 `indexer_runtime.rs` / `ntfs.rs`，与 G3 的 Shell/日志改动基本不重叠。G2 的 sidecar 首建可复用本阶段的逐卷发布框架，因此本阶段也应先于 G2。
