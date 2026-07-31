# Design

## Current Shape And The Single Blocking Point

```text
run()
  ├─ create_pipe + spawn(serve)          ← 管道已可用，但 status.ready=false
  └─ spawn_blocking(load_or_build).await ← 阻塞点：等全部卷
       └─ build_all(): for descriptor in descriptors { build_volume(d) }
     state.publish(index)                ← 唯一一次发布
     start_watchers(所有卷)              ← 全部卷一起起 watcher
```

阻塞点只有一处：`build_all` 收集完整 `Vec<VolumeIndex>` 才返回，`publish` 才发生。改造的全部要点是把"一次发布"变成"逐卷发布"，其余结构不动。

## Target Shape

```text
run()
  ├─ create_pipe + spawn(serve)
  ├─ discover_volumes() → 按系统卷优先重排
  ├─ 尝试 load_cache：命中且 checkpoint 有效 → 与现状一致，一次发布（快路径不变）
  └─ 未命中 → 首建循环（在 spawn_blocking 内逐卷推进）
       for descriptor in ordered_descriptors {
         volume = build_volume(descriptor)      ← 单卷，内部已先取 USN checkpoint
         state.merge_and_publish(volume)        ← 新增：并入 + generation+1 + notify
         start_watcher(单卷)                    ← 新增：该卷立即进入实时监听
         progress.volume_done()                 ← 新增：原子计数
       }
       首建完成后才 index_cache::save
```

关键正确性依据：`build_volume` 的顺序是 `query_or_create_journal` → `enumerate_mft` → `replay_until(current.next_usn)` → `finish_initial_build`，返回时 `volume.next_usn` 已推进到枚举结束时刻。watcher 从该 `next_usn` 续读，**枚举期间与发布之后的空档都不会丢事件**。这一点使"发布后再起 watcher"成立，不需要提前抢占 journal。

## State Changes

`ServiceState` 需要两处扩展，都不改变现有字段语义：

- `merge_and_publish(volume: VolumeIndex)`：取写锁，把卷 push 进 `index`（`None` 时先建空 `IndexState`），`generation += 1`，`notify_waiters()`。与现有 `publish()` 并存——`publish()` 保留给缓存命中与整体 rebuild 的整表替换路径。
- `first_build_complete: AtomicBool`：R2 的落盘门。`checkpoint()` 与 `run()` 退出前的 `checkpoint(&state, &data_dir)` 都要先检查它。

`building` 语义保持不变（首建/重建期间为 true），但现在它可以与 `ready: true` 共存——这是本阶段引入的新组合，协议上必须允许 `ready && building` 同时为真，前端据此显示"可搜但仍在补充"。

## Progress Contract

`IndexerStatus` 增加一个可选字段，不改动既有字段：

```text
build_progress: Option<BuildProgress> {
  volumes_total: usize,
  volumes_done: usize,
  current_volume: Option<String>,     // mount_path
  records_scanned: Option<u64>,       // 可选，来自枚举循环的原子计数
  records_estimate: Option<u64>,      // 可选，上次缓存的记录数
}
```

- 计数用 `AtomicUsize`/`AtomicU64` 存在 `ServiceState` 或独立 `BuildProgress` 结构里；`enumerate_mft` 的批次循环每批累加一次，不是每条记录，避免热循环里的原子争用。
- `records_estimate` 从上一次缓存读出；首次安装无缓存时为 `None`，前端退化为"已完成 N/M 卷"而不显示百分比。
- 全字段 `Option` + `skip_serializing_if`，旧 reader 缺失即视为无进度信息。

broker 侧把它并入现有 `Response::Results` 的可选索引状态字段（G1 已定的可选匹配/状态元数据形状），不新增顶层消息类型。

## Volume Ordering

`discover_volumes()` 保持 A→Z 扫描（它同时负责 NTFS/固定盘过滤，不宜改动），在 `run()` 里对返回的 `Vec<VolumeDescriptor>` 做一次稳定重排：系统卷（`%SystemDrive%` 的盘符）提到首位，其余保持原相对顺序。稳定重排保证基准可复跑。

## Interaction With G1 Semantics

三种"结果不完整"必须在协议上可区分，这是本阶段最容易埋雷的地方：

| 原因 | 表达方式 | 前端反应 |
| --- | --- | --- |
| 因 `max` 截断 | `is_truncated = true`（G1） | 禁止在其上本地过滤 |
| 索引完全未就绪 | `ready = false` | 显示等待，轮询 |
| 索引部分就绪（本阶段新增） | `ready = true && building = true` | 可用但需随 generation 刷新 |

第三种是新组合。前端增量缓存的失效依据是 generation，而逐卷发布每卷 `+1`，因此缓存会自然失效。**但这是推理不是保证**，必须写一条针对性测试：部分索引下取得结果 → 追加字符 → 断言重新请求后端而非本地过滤。同时 `is_truncated` 的文档定义要显式排除"索引未建完"。

## Persistence Gate (R2)

```text
checkpoint(state, data_dir):
  if !first_build_complete { skip + log }   ← 新增门
  else 现有逻辑
```

现有的 `validate_checkpoints` 用 `index.volumes.len() != descriptors.len()` 能挡住大多数残缺缓存，但它是巧合式防护——若首建中途卷集恰好也发生变化，两个数字可能相等。显式门比依赖巧合可靠，且 log 一条可诊断信息。

## Enumeration Tuning (R6, Data-Gated)

三个候选，全部以 G0 数据判定，任一项无收益即记录并放弃：

1. **`enumerate_mft` 输出缓冲区** 256KB → 1MB/4MB。减少 `FSCTL_ENUM_USN_DATA` 往返次数。风险低，先测。
2. **`build_volume` 的 pending 重试循环**（最多 64 遍）。记录实际遍数：records 已按 FRN 排序，父通常先于子，预期 1–2 遍。若实测远高，说明排序假设不成立，值得单独优化；若是 1–2 遍则不动。
3. **多卷并行枚举**。与 R3 的优先级冲突需要明确取舍：建议系统卷单独先跑（保住"最快可用"），其余卷之后再并行。只在多物理盘上可能有收益，单盘上并行会互相抢 IO——必须区分测量，不能一概而论。

`opt-level = "z"` 对 parse/sort/upsert 热循环的影响由 G0 提供对照；若差值显著，取舍归 G1，本阶段只引用结论。

## Rollback

- `merge_and_publish` 与 `publish` 并存，回滚只需把首建循环改回收集完整 `Vec` 再一次 `publish`，watcher 启动点回到循环之后。
- 进度字段全可选，回滚后旧 reader 无感。
- 落盘门是单独一个布尔判断，可独立回滚，但**不建议**回滚——它修的是真实的残缺缓存风险。
- R6 的每项调优各自独立提交，便于单独回退。
