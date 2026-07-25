# backend-spec.md — Prism 第一轮优化·后端施工规范

> 本文档指导一个独立 AI（Codex / ChatGPT 等）完成 Prism 后端（Rust）第一轮优化的两个工作项：
> **B-W1 USN Journal 实时增量索引**（核心）与 **B-W2 索引构建进度上报**。
> 阅读顺序：§1–§3 建立上下文 → §4 数据结构约束（**必读，决定实现形状**）→ §5 施工细则 → §6 验证。
> **禁止事项见 §8，全程遵守。**

## 1. 项目上下文

- 仓库根：`D:\LS\DM\Listary`。后端 crate：`src/prism-core`（Rust，tokio multi_thread 2 workers，windows crate 做 Win32 调用）。
- 前端：`src/Prism`（WPF），经命名管道 `\\.\pipe\prism-core` 收发 UTF-8 按行 JSON。前端另行施工，本规范**不得**改动 `src/Prism`。
- 语言约定：注释、日志文案**简体中文**；标识符英文。日志用 `crate::log(...)`（main.rs，eprintln 带毫秒时间戳）。
- 构建/验证命令（仓库根执行，必须全绿、clippy 零 warning）：
  ```
  cargo test  --manifest-path src/prism-core/Cargo.toml
  cargo clippy --manifest-path src/prism-core/Cargo.toml -- -D warnings
  cargo build --release --manifest-path src/prism-core/Cargo.toml
  ```
- 内存红线：后端进程常驻 ≤ **70MB**（现状约 22MB）。任何新增常驻结构都要在此预算内说明开销。

### 文件地图

| 文件 | 职责 | 本次动它吗 |
|---|---|---|
| `src/main.rs` | 入口：resolve_data_dir → Config::load → spawn build_or_load + apps::load → ipc::serve | 是（接线） |
| `src/index.rs` | FileIndex（intern 池+条目）、MFT 枚举（`mod mft`）、walkdir 降级、postcard 缓存 v3、build_or_load/定时刷新 | 是（主战场） |
| `src/ipc.rs` | Request/Response enum、serve/handle_connection、search 合并逻辑 | 是（进度字段） |
| `src/config.rs` | Config（serde default，与前端共用 settings.json）、resolve_data_dir | 是（新开关） |
| `src/usn_watch.rs` | **新建**：USN Journal 监听 | 是（新文件） |
| `apps.rs` / `actions.rs` / `websearch.rs` / `search.rs` | 程序清单 / 动作 / 网页搜索 | **否** |

## 2. 工作项与验收

| # | 工作项 | 验收 |
|---|---|---|
| B-W1 | USN 实时增量监听 | 管理员运行：新建/重命名/删除文件后 **≤5 秒**搜索可见/消失。普通权限：不崩溃、搜索可用，新文件最坏 ~60 秒可见（降级定时重建）。journal 溢出自愈。config 可一键关闭回旧行为 |
| B-W2 | 构建进度上报 | 删 data 目录冷启动期间，`results` 响应带 `index_progress` 且数值递增；构建完成后不再携带。旧前端（不识别该字段）不受影响 |

施工顺序：**先 B-W2（小、热身、独立可交付），后 B-W1**。

## 3. 现状机制（为什么改）

- `index::build_or_load`（index.rs:593）：启动先 `load_cache`（postcard v3，日志有「加载耗时 {}ms」行）；无缓存则 `build_full_index()`（index.rs:653）——逐卷 `mft::try_append_volume` 做 **FSCTL_ENUM_USN_DATA 一次性枚举**，任一卷失败整体降级 walkdir。之后若 `refresh_secs > 0`，每 300s（`Config::default`）**全量重建 + 整表换 + save_cache**。
- 问题：没有任何 USN Journal 订阅。新文件感知完全靠 300s 定时全量重建 → 最坏 ~5 分钟盲区，且每次刷新是冗余全量枚举。
- `is_indexing` 判定：`SharedIndex = Arc<RwLock<Option<FileIndex>>>`，`None` 即 indexing（ipc.rs search 分支）。构建期间前端只能拿到布尔值，无进度。

## 4. 数据结构硬约束（实现形状由此决定，先读懂再动手）

1. **`IndexEntry` 不存 FRN**（index.rs:22）：仅 `dir_off/name_off/kind`，9 字节。USN 事件只给 `(FRN, ParentFRN, 文件名, reason)`——**不含完整路径**。要把事件解析成 `父目录路径 + 文件名`，必须有 **ParentFRN → 目录路径** 的映射。
2. **MFT 枚举时的 FRN map 是临时物**（index.rs:395 `append_mft` 内的 `map: HashMap<u64,(u64,String,bool)>`），`into_index` 后即丢。→ B-W1 必须在枚举时**顺手留下目录 FRN 表**（只留目录，不留文件：全盘目录数约为条目数的 5–10%，百万条目 ≈ 数万~十几万目录，每条 ~40–80B，常驻 **个位数 MB**，预算内。留全部文件则会爆预算，禁止）。
3. **字符串池只增不减**（`PoolBuilder.intern`，构建后 intern 表丢弃）：增量 upsert 只能**追加**池尾（允许重复串，不必重建 intern 表——churn 量小）；删除条目后池里留死串。→ 池垃圾靠**周期性压缩重建**回收（复用现有定时刷新路径，见 §5.3.6）。
4. **缓存 v3 不含 USN 位点**：重启后若只加载 v3 缓存，停机期间的文件变更全部丢失。→ 缓存升 **v4**，加入每卷 `{journal_id, next_usn, 目录 FRN 表}`；加载后从 `next_usn` **重放**追平停机窗口。v3 及更早版本一律按现有逻辑丢弃重建（load_cache 已有版本检查，改 `CACHE_VERSION = 4` 即可自然淘汰）。

## 5. 施工细则

### 5.1 B-W2 构建进度上报

**新结构**（放 index.rs）：

```rust
/// 全量构建进度。scanned 由 MFT 记录循环递增；total_estimate 取上次成功
/// 构建的条目数（首装无缓存时为 0 = 未知）。构建完成后 active=false。
pub struct IndexProgress {
    pub active: AtomicBool,
    pub scanned: AtomicU64,
    pub total_estimate: AtomicU64,
}
pub type SharedProgress = Arc<IndexProgress>;
```

**接线**：

1. `main.rs` 创建 `SharedProgress`，同时传给 `index::build_or_load` 与 `ipc::serve`（两者签名各加一参）。
2. `build_full_index` 改为接收 `&SharedProgress`：进入时 `active=true`、`scanned=0`；`mft::append_mft` 的 USN_RECORD 解析循环里每条 `scanned.fetch_add(1, Relaxed)`（walkdir 降级路径同样在 entry 循环里递增）；完成时 `active=false`。`total_estimate` 在 build_or_load 里设置：load_cache 成功 → 设为缓存条目数；失败 → 0。定时刷新/自愈重建前把上一版 `FileIndex::len()` 写入 `total_estimate`。
3. `ipc.rs` `Response::Results` 增加可选字段：
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   index_progress: Option<IndexProgressDto>,   // { scanned: u64, total_estimate: u64 }
   ```
   仅当 `is_indexing == true` 且 `progress.active` 时填充。DTO 字段名 serde 输出必须是 `index_progress` / `scanned` / `total_estimate`（snake_case，与前端契约一致，见任务目录 frontend-spec.md §4.4）。
4. 所有构造 `Response::Results` 的位置补 `index_progress: None`/实值（编译器会带你找全）。
5. 单测：至少一例验证 `Results` 序列化——`is_indexing:true` 带 `index_progress` 时 JSON 含该对象；`None` 时 JSON **不含**该键（`skip_serializing_if` 生效）。

### 5.2 首步实测（B-W1 动工前做，10 分钟）

运行现有 release 后端一次（有缓存状态），从 stderr 抓「加载耗时 {}ms」实际值，写入 `.trellis/tasks/07-25-prism-optimize-round1/research/load-cache-ms.md`（一行数值+机器说明即可）。该值 >2000ms 时在同文件注明"重启加载期也需要前端提示"。

### 5.3 B-W1 USN 实时增量监听

#### 5.3.1 config.rs

```rust
/// USN Journal 实时监听总开关。false = 回到 1.0.0 纯定时重建行为。
pub usn_watch: bool,          // Default: true
pub index_refresh_secs: u64,  // Default 由 300 改为 60（B1 兜底）
```
`#[serde(default)]` 结构已就位，加字段即可；`Default` impl 同步改。注释写明两者关系（watch 全卷成功时定时重建自动放宽，见 5.3.6）。

#### 5.3.2 index.rs — 目录 FRN 表与增量接口

**目录 FRN 表**（每卷一份，随缓存持久化）：

```rust
/// 单卷目录 FRN → (父目录 FRN, 目录名)。用于把 USN 事件解析成完整路径。
/// 只存目录不存文件（内存红线，见 backend-spec §4.2）。
#[derive(Serialize, Deserialize)]
pub struct DirMap { pub drive: char, pub map: HashMap<u64, (u64, String)> }
```

- `mft::append_mft` 枚举循环中 `is_dir` 的记录同时写入 DirMap（根 FRN 解析不到父时按现有 `build_path` 的 root_prefix 逻辑落到 `X:`）。
- 提供 `DirMap::resolve_path(&self, frn: u64) -> Option<String>`：沿父链拼 `X:\a\b`，深度上限 64（与现有 build_path 一致），断链返回 None（事件丢弃并记日志一次性计数，不 panic）。

**FileIndex 增量方法**（保持现有字段私有，新增 pub 方法）：

```rust
/// 追加一条（允许池串重复；churn 由周期压缩回收）。已存在同 (dir,name) 时先删旧。
pub fn upsert(&mut self, dir: &str, name: &str, kind: u8);
/// 按 (dir,name) 精确匹配删除（大小写不敏感比较与 search 一致）。返回是否删到。
pub fn remove(&mut self, dir: &str, name: &str) -> bool;
```

- `remove` 允许 O(n) 线性扫（百万条目 ~ms 级，事件已批量化）；`swap_remove` 不可用——`search` 按 entries 顺序即排序语义，用 `Vec::remove` 或标记后批量 retain（**批内多删用 retain 一次遍历**）。
- upsert/remove 必须复用现有噪声过滤：`is_skipped_name(name)` 与 `path_is_excluded(&full_path)` 命中则直接忽略事件。
- 单测（新增，参照现有 tests 的 `make_test_index` 风格）：upsert 新增可搜到；upsert 覆盖同名不重复；remove 后搜不到；remove 不存在项返回 false；噪声路径事件被忽略。

#### 5.3.3 缓存 v4（index.rs）

```rust
const CACHE_VERSION: u32 = 4;
#[derive(Serialize, Deserialize)]
struct CacheEnvelope {
    version: u32,
    index: FileIndex,
    /// v4 新增：每卷 USN 位点 + 目录表。空数组 = 无 watch 状态（walkdir 构建等）。
    volumes: Vec<VolumeMeta>,   // { drive: char, journal_id: u64, next_usn: i64, dirs: DirMap }
}
```

- `save_cache`/`load_cache` 同步改签名携带 `volumes`。现有 v3/损坏缓存 → `load_cache` 返回 None → 全量重建（无迁移代码）。
- 现有 cache 相关单测更新到 v4；`old_version_cache_rejected` 改写为 v3 被拒。

#### 5.3.4 usn_watch.rs（新文件）

每卷一个监听任务，`main.rs`/`build_or_load` 完成首建或缓存加载后启动：

```
pub async fn watch_volumes(
    volumes: Vec<VolumeMeta>,      // 首建来自 MFT 枚举；缓存加载来自 v4 envelope
    shared: SharedIndex,
    data_dir: PathBuf,
    progress: SharedProgress,      // 自愈重建时复用
) -> WatchOutcome                  // { watched: usize, failed: usize } 供调用方决定定时重建间隔
```

单卷任务（`tokio::task::spawn_blocking` 内同步循环，卷句柄打开方式同 `mft::try_append_volume`）：

1. **校验**：`FSCTL_QUERY_USN_JOURNAL` 取 `{UsnJournalID, NextUsn, ...}`。`journal_id` 与保存值不符，或保存的 `next_usn` 已低于 `FirstUsn`（被截断）→ 该卷标记 **需全量重建**（见 4 步自愈）。
2. **重放 + 长轮询**：`READ_USN_JOURNAL_DATA_V0 { StartUsn, ReasonMask, BytesToWaitFor: 1, Timeout: 5(秒), UsnJournalID }` 循环 `FSCTL_READ_USN_JOURNAL`。ReasonMask 只订阅：`FILE_CREATE | FILE_DELETE | RENAME_OLD_NAME | RENAME_NEW_NAME`（属性/内容写入不订阅，降噪）。
3. **事件 → 增量**：解析 USN_RECORD_V2（解析方式抄 `append_mft` 的指针遍历）：
   - `FILE_CREATE`、`RENAME_NEW_NAME` → `dirs.resolve_path(ParentFRN)` 得 dir → 攒 `Upsert{dir,name,is_dir}`；若记录本身是目录，同步更新 DirMap（insert/改名）。
   - `FILE_DELETE`、`RENAME_OLD_NAME` → 同上得 dir → 攒 `Remove{dir,name}`；目录记录则从 DirMap 移除。
   - **目录级事件的子树问题**：目录改名/删除后，其所有后代条目的 dir 串已失效（池存的是完整父路径串）。**MVP 不做子树重写**：凡 `is_dir` 的 create/delete/rename 事件，除维护 DirMap 外，将该卷标记 `dirty`，由**去抖的自愈重建**兜底（10 秒无新目录事件后触发一次全量重建）。文件级事件（绝大多数）走纯增量。此取舍必须写进代码注释。
   - 每批攒 **500ms 或 512 条**（先到为准）后一次性拿 `shared` 写锁应用（`retain` 删 + 逐条 upsert），随后更新内存中的 `next_usn`。
4. **自愈**：journal 校验失败 / `ERROR_JOURNAL_ENTRY_DELETED` / 目录事件 dirty 去抖到期 → 调用与现有定时刷新相同的重建路径（`build_full_index` + 换表 + save_cache v4 含新位点），日志注明原因；重建后回到第 2 步继续 watch。同一时刻全局至多一个重建在跑（用 `tokio::sync::Mutex` 或 AtomicBool 串行化，防多卷同时触发）。
5. **降级**：卷句柄打开失败或 QUERY/READ 返回拒绝访问（普通权限常见）→ 该卷 `failed+1`，任务退出，日志一条「卷 X: USN 监听不可用（原因），依赖定时重建」。**不 panic、不影响其它卷。**
6. **位点持久化**：不必每批写盘。跟随现有 save_cache 时机（重建后）+ 每 10 分钟若有增量则 save_cache 一次（复用去抖思路）。进程被杀最多丢 10 分钟位点——v4 重放机制会在下次启动追平，可接受。

#### 5.3.5 main.rs / build_or_load 接线

- `Config` 增读 `usn_watch`；`build_or_load` 签名扩展（或新增 `build_or_load_with_watch`），完成加载/首建后：
  - `usn_watch == false` → 行为与现状完全一致（300/60s 定时重建）。
  - `usn_watch == true` → 启动 `watch_volumes`；根据 `WatchOutcome`：**全部卷 watch 成功** → 定时重建间隔改用 3600s（仅作池压缩+位点落盘，见下）；**部分/全部失败** → 维持 `index_refresh_secs`（缺省 60s）。
- walkdir 降级构建（无 MFT 权限）时 `volumes` 为空 → watch 自然零卷全失败 → 定时重建路径，无需特判。

#### 5.3.6 池压缩

现有定时刷新（全量重建+换表）天然就是压缩，保留即可；watch 全成功时它以 3600s 低频跑，兼任「池垃圾回收 + 位点校准」。**不要**另写压缩算法。

## 6. 验证清单

1. `cargo test` 全绿（新增：upsert/remove/噪声过滤/v4 roundtrip/v3 拒绝/Results 进度序列化）。
2. `cargo clippy -- -D warnings` 零告警；unsafe 块只出现在 Win32 调用处，逐块带中文注释说明不变量（与现有 mft 模块风格一致）。
3. 手测矩阵（release 构建）：
   | 场景 | 期望 |
   |---|---|
   | 管理员 + 有缓存启动 | 日志出现各卷「USN 监听已启动」；新建 `D:\test-usn-{随机}.txt` ≤5s 可搜到；改名 ≤5s 新名可搜、旧名消失；删除 ≤5s 消失 |
   | 目录改名 | ~10s 内（去抖重建后）新目录名下文件可搜 |
   | 普通权限启动 | 每卷一条降级日志，进程存活，搜索正常；新文件 ≤60s 可见 |
   | 停机窗口重放 | 退出后端 → 新建文件 → 启动 → 无需全量重建即可搜到（日志显示重放条数） |
   | settings.json 置 `"usn_watch": false` | 无 watch 日志，行为同 1.0.0 |
   | 冷启动（删 data） | 构建期 `results` 带递增 `index_progress`；完成后消失 |
4. 内存：空闲（watch 常驻）任务管理器私有工作集 ≤70MB，与 22MB 基线相比增幅应 ≈ DirMap 体量（个位数 MB）。
5. 双前端兼容：新后端 + 现有前端（不识别 index_progress）运行正常。

## 7. 通用编码规范

- 风格向 index.rs 看齐：模块头 `//!` 中文注释、中文日志、`crate::log`、错误兜底降级绝不 panic（用户机器上 stderr 无人看，崩溃=功能消失）。
- Win32 调用统一走已依赖的 `windows` crate（Cargo.toml 现有 features 不够时可加 feature，**不得**新增其它依赖）。
- 锁纪律：`shared.write()` 持锁只做换表/批量应用，**不做 I/O**；长循环在 `spawn_blocking`，不阻塞管道 worker（main.rs 顶部注释解释过教训）。
- 每个工作项一个 commit 粒度改动面；不顺手重构无关代码。

## 8. 禁止事项

- ❌ 不得修改 `src/Prism`（前端）任何文件；IPC 契约以 §5.1.3 字段定义为准。
- ❌ 不得改变现有 IPC 消息的既有字段名/语义、`SearchResult`/`ActionItem` 结构、管道名与协议（按行 JSON）。
- ❌ 不得在 FileIndex 常驻结构里保存全量文件级 FRN 映射（内存红线）。
- ❌ 不得引入新第三方依赖（windows crate feature 追加除外）。
- ❌ 不得改动 `.trellis/`、`dist/`、`README`。
- ❌ 不得执行任何 git 操作（施工完成交回人工审查提交）。
