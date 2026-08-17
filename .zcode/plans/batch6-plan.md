# 审计批次6 实施方案：内存主项（P3+M3 / P1 / P4 / P7）

## 参考软件调研结论

### Everything (voidtools)
- **数据库全驻内存**：运行时整个索引在 RAM 中，仅退出时写盘。
- **增量名编码（front-coding / delta encoding）**：排序后的文件/文件夹名只存与前一条的差量片段（code length + code offset + fragment），相同前缀零冗余。这使路径名存储从 O(n×完整路径) 降到 O(n×差量)。
- **父目录用整数偏移量**：文件夹引用父节点是数组下标（DWORD offset），不是路径字符串。文件引用父文件夹也是 offset。
- **USN journal 紧凑追加**：默认 1MB（约 5000 条变更），环形覆盖。
- **Run History 轻量计数器**：按文件名做 key，只存 run count + date，与主索引分离。
- **SDK 暴露的排序模型**：Everything_SortResultsByPath 是对已有结果的二次排序（说明排序是结果集级别的，不回查索引），排序字段由 Everything_SetSort 预设，结果一次性取回（GetResult* 系列）。

### Listary V7
- Rust 重写，引擎在主进程内运行（不用独立子进程），内存降 30%、速度升 20%。
- 推荐系统基于使用频率/最近使用，空查询直接返回最近文件（与 Prism 的 G4 模式一致）。
- 最近文件夹自动去重（与 Prism 的 history_file_candidates 语义一致）。
- 路径排除规则集成进历史结果（与 Prism 的 exclusion_paths 一致）。

### 对 Prism 批次6 的启示
1. **Everything 的 delta-encoding 与 offset 指针**是长期方向，但属于架构级重构（批次9 I3 名字池分段），不在本批做。
2. **Everything 的"数据库全驻内存、退出才写盘"**模式对 Prism 不直接适用——Prism 是服务进程，不能靠退出落盘。但"锁内只做内存操作、出锁再持久化"的核心思想可直接用于 P3+M3。
3. **Everything 的 Run History 轻量分离**模式与 Prism 的 weights() 快照思路一致：读取侧不持有写锁，用快照/引用计数。
4. **Listary V7 "引擎在主进程、内存降 30%"**印证了"减少每击键分配"的优先级——Prism 批次6 正是减少每次搜索的 clone/分配。

---

## P3+M3：record_at 出锁 + 增量 prune/index 维护

### 现状
`record_at`（history.rs:251-302）在整个写锁内完成：内存更新 + prune（全排序 O(n log n)）+ rebuild_index（全量 O(n)）+ persist（JSON 序列化 + fsync + atomic_replace）。每次用户执行/揭示/跳转一个文件，写锁持有时间 = 内存操作 + JSON 编码 + 文件 I/O + fsync，期间所有搜索的 score()/weights() 被阻塞。

### 做法
1. **锁内**：更新 entry 内存（frecency/count/last_used/query_stat），仅做**增量 prune**（不调用全量 sort+rebuild_index），产生一个 `entries: Vec<HistoryEntry>` 的浅克隆（clone entries 切片）。释放写锁。
2. **增量 prune**：新条目或更新后条目只需在 entries 满（>MAX_ENTRIES）时挤出最后一条（最旧的，已在排序末尾）。不需要每次全量排序。仅在新增条目时检查是否超容量，超则 truncate 末尾。
   - **不变量保持**：entries 在初始 load 时已按 last_used 降序排列。每次 record_at 更新一个已有条目后，需要把它移到 entries[0] 位置（MRU 在前）。这不是全量排序——是单条提移。
   - 已有条目更新时：从当前位置 swap_remove 到末尾，然后 insert(0, ...)。这保持 last_used 降序。
   - 新条目：直接 insert(0, ...)。
   - 超容量：truncate(MAX_ENTRIES)。
   - index：swap_remove 改变了被移走条目的下标，需要更新其 index 条目。insert(0, ...) 需要所有 index 值 +1（或改用 BTreeMap/不维护连续 index）。
   - **更简洁的方案**：不移动条目位置，仅在 prune 时全量排序（当前做法）。prune 只在新条目导致超容量时触发（不是每次 record_at），且 prune 只在每 N 次或超容量时执行。
   - **最终决策**：采用"锁内更新 + 仅超容量时 prune + 出锁 persist"方案。prune 仍走全量排序（O(n log n)），但只在 entries.len() > MAX_ENTRIES 时触发（即新增第 5001 条时），日常更新（已有条目）不触发 prune。rebuild_index 同理——只在 prune 改变了下标时执行。
3. **出锁 persist**：锁外做 JSON 序列化 + 文件写入 + fsync + atomic_replace。不持锁。

### 详细改动
```rust
pub(crate) fn record_at(&self, target: &ActionTarget, usage: HistoryUse, query: Option<&str>, now: u64) -> Result<(), String> {
    if !self.is_enabled() || !is_recordable(target) { return Ok(()); }
    
    // 锁内：更新内存
    let entries_snapshot = {
        let mut state = self.state.write().map_err(|_| "history lock is poisoned".to_string())?;
        let key = composed_key(&target.kind, &target.value);
        let position = match state.index.get(&key).copied() {
            Some(pos) => pos,
            None => {
                state.entries.push(HistoryEntry { kind: target.kind.clone(), target: target.value.clone(), ..Default::default() });
                let pos = state.entries.len() - 1;
                state.index.insert(key, pos);
                pos
            }
        };
        let entry = &mut state.entries[position];
        // ... frecency/count/last_used/query_stat 更新（不变）...
        
        // 仅在超容量时 prune（新增条目导致超限）
        let needs_prune = state.entries.len() > MAX_ENTRIES;
        if needs_prune {
            prune(&mut state.entries);
            state.rebuild_index();
        }
        state.entries.clone()  // 出锁后持久化
    };
    // 锁外：持久化
    persist(&self.path, &entries_snapshot)
}
```

### 风险与对策
- **防抖落盘**：计划文档明确"本期不做"。如果 record_at 在短时间内被多次调用（如连续窗口切换），每次都触发一次 fsync。这是当前行为，不改变。
- **持久化失败**：如果 persist 失败，内存已更新但磁盘未更新。下次启动会读到旧版本。这与当前行为一致——当前 persist 失败也是返回 Err，内存已改。不变。
- **竞态**：两个并发 record_at，各自拿到写锁更新内存。如果 A 先更新内存释放锁，B 再更新内存释放锁，A 的 persist 和 B 的 persist 可能交叉写入。但由于 atomic_replace 保证原子性，最终磁盘内容是最后一个 persist 的结果。这是安全的——两次记录的语义是"记两次"，最终磁盘内容反映最后一次内存状态。唯一风险是 A 的 persist 写入后 B 的 persist 写入前的窗口——但 atomic_replace 保证文件始终是完整的一个版本。

### 测试
- 已有的 prune 测试和 record 测试保持不变。
- 新增测试：record_at 在 persist 失败时不影响内存状态（与当前一致）。
- 新增测试：并发 record_at（两条不同 target）最终磁盘内容正确。

---

## P1：weights() 命中才拷贝 / Arc 快照

### 现状
`weights()`（history.rs:353-375）在每次搜索时 clone 整个 entries 列表（5000 条 × 每条至少 2 个 String），然后传给 `history_file_candidates`。实际上只有少数命中查询匹配的条目会被用作 SearchResult。

### 做法
**方案选择：Arc 快照**

```rust
pub fn weights(&self) -> Arc<[HistoryWeight]> { ... }
```

但这需要 HistoryWeight 从 owned ActionTarget 改为 Arc 引用，涉及面太大。

**更实际的方案：迭代器 + 回调过滤**

weights() 返回一个轻量快照（只包含 score + last_used_utc + 对 entries 的 Arc 引用），history_file_candidates 在迭代时只对命中条目 clone ActionTarget。

但这改变了 history_file_candidates 的签名（从 `&[HistoryWeight]` 改为需要 history store 引用），且 weights() 当前还在 empty_query_results 里用。

**最终方案：Arc<Vec<HistoryWeight>> 快照发布**

1. HistoryState 内部维护一个 `Arc<Vec<HistoryWeight>>` 缓存，在每次 record_at 更新后失效（设为 None）。
2. weights() 在缓存有效时直接返回 Arc clone（原子操作，零拷贝）；缓存失效时在写锁内重建。
3. 但这引入了写锁内的重建开销——且 weights() 的调用者不在写锁上下文。

**重新审视——最简方案：读锁内只 clone 命中条目**

不改变 weights() 的签名，而是改变调用方式：
- search_service 不再调用 `history.weights()` 获取全量列表
- 而是获取一个只含 `(score, last_used_utc, kind, value_ref)` 的轻量迭代器
- history_file_candidates 在迭代时，只有命中的条目才 clone value

**最终决策：改 weights() 返回 Arc<[HistoryWeight]>，HistoryWeight.target.value 改为 Arc<str>**

这需要：
1. HistoryEntry 的 kind/target 仍为 String（持久化需要）
2. weights() 构建时 Arc::from(entry.target.as_str())
3. HistoryWeight.target 的 kind 改为 &'static str（kind 只有 4 种值，不需要 String）
4. HistoryWeight.target.value 改为 Arc<str>

但这会改变 HistoryWeight 的定义，影响 history_file_candidates 和测试。

**实际最简方案：不改 HistoryWeight 结构，改 weights() 为 Arc<Vec>**

```rust
pub fn weights(&self) -> Arc<Vec<HistoryWeight>> {
    // 读锁内构建，返回 Arc
    // 调用方拿到 Arc clone（仅 refcount++），不再每条 clone
}
```

history_file_candidates 签名从 `weights: &[HistoryWeight]` 改为 `weights: &[HistoryWeight]`（仍用 slice），调用方 `.as_slice()` 获取。零结构变更。

但问题在于：weights() 内部仍然每次 clone 全部 5000 条。Arc<Vec> 只解决了"多次调用 weights()"的重复 clone 问题，没解决"单次 weights() 的 clone 成本"。

**最终最终方案：不改 weights() 结构，改调用模式**

search_service 当前：
```rust
let history_weights = history.weights();  // clone 5000 条
// 传给 spawn_blocking → history_file_candidates
```

改为：在 spawn_blocking 闭包内获取读锁，直接在锁内迭代 entries（零 clone），只对命中条目 clone。这消除了 weights() 全量 clone。

```rust
// 不再调用 history.weights()
let history_candidates = tokio::task::spawn_blocking(move || {
    history.history_file_candidates_live(&name_query_clone, pinyin_enabled, &exclusions, root_clone.as_deref(), result_slots)
}).await.unwrap_or_default();
```

新增 `history_file_candidates_live` 方法在 HistoryStore 上，持有读锁直接迭代 entries：
```rust
pub fn history_file_candidates_live(&self, query: &str, pinyin_enabled: bool, exclusions: &[String], root: Option<&str>, limit: usize) -> Vec<SearchResult> {
    let state = self.state.read().unwrap();
    let now = now_utc();
    // 迭代 entries，只对命中条目构建 SearchResult
    // 消除全量 weights() clone
}
```

empty_query_results 也改用这个方法。

### 风险与对策
- **读锁持有时间**：history_file_candidates_live 在读锁内做磁盘 stat（Path::exists()），会延长读锁持有时间。但读锁是共享的，多个搜索可以并发持有。且 Path::exists() 很快（单次 stat）。当前 weights() 也是在读锁内 clone 后释放锁，stat 在锁外。改后 stat 在锁内——但读锁不阻塞其他读操作，只阻塞写操作（record_at 的 persist 已出锁，写锁持有时间很短）。可接受。
- **Path::exists() 在锁内**：plan 文档强调"Path::exists() 磁盘 stat 必须留在锁外"。需要遵守这个约束。
- **修正**：分两步——读锁内做名字匹配，收集命中条目的 (kind, value, score, last_used) 到小 Vec（命中数远小于 5000），释放读锁，然后在锁外做 stat + 构建 SearchResult。

```rust
pub fn history_file_candidates_live(&self, query: &str, pinyin_enabled: bool, exclusions: &[String], root: Option<&str>, limit: usize) -> Vec<SearchResult> {
    // Phase 1: 读锁内做名字匹配，收集命中条目的轻量快照
    let hits: Vec<HitSnapshot> = {
        let state = self.state.read().unwrap();
        let now = now_utc();
        let empty_query = query.is_empty();
        let query_lower = query.to_lowercase();
        // ... iterate entries, match, collect to Vec<HitSnapshot> ...
    };
    // Phase 2: 锁外做 stat + 构建 SearchResult
    // ...
}
```

HitSnapshot 只包含构建 SearchResult 需要的最小数据：(kind: String, value: String, score: u32, last_used: u64, metadata, match_spans)。这比 clone 5000 条 HistoryWeight 少得多——只有命中条目（通常 < 100 条）。

### 测试
- 已有的 history_file_candidates 测试改用新方法。
- 新增测试：weights() 仍保留（可能有其他调用方），但不再用于 search_service。
- 排序等价性测试：新方法产出的候选顺序与旧方法一致。

---

## P4：最终排序去 clone（Option::take 法）

### 现状
`sort_search_results_with_picks`（ipc.rs:2104-2128）在排序后重建列表时 clone 了所有 SearchResult（每个含 4-5 个 String）：
```rust
let sorted: Vec<SearchResult> = indices.iter().map(|&index| items[index].clone()).collect();
```

### 做法
```rust
fn sort_search_results_with_picks(items: &mut Vec<SearchResult>, picks: Option<&[bool]>) {
    let lowercased: Vec<String> = items.iter().map(|item| item.title.to_lowercase()).collect();
    let mut indices: Vec<usize> = (0..items.len()).collect();
    indices.sort_by(|&a, &b| { /* comparator unchanged */ });
    
    // 用 Option::take 按序重建，零 String clone
    let mut source: Vec<Option<SearchResult>> = items.drain(..).map(Some).collect();
    for (slot, &index) in indices.iter().enumerate() {
        items.push(source[index].take().unwrap());
    }
}
```

注意签名从 `&mut [SearchResult]` 改为 `&mut Vec<SearchResult>`，因为需要 drain + push。

### 关键约束（plan 文档）
- 保 items/picks/indices 三者下标对齐
- 保 kind→picked→metadata 层级
- rank_window_list 入口同步（它调用 sort_search_results）

### 风险与对策
- `aabb5fe` 的环 bug：Option::take 不做原地交换，直接按序 take，无环问题。
- picks 数组与原 items 对齐：排序后用 indices[index] 取出，picks[index] 仍是对应的——正确。
- drain 后 items 为空，push 重建。items 的容量不变（drain 保留容量）。

### 测试
- 已有的 `sort_search_results_orders_items_across_match_tiers` 和 `query_pick_promotes_within_kind_but_not_across` 保持不变，验证正确性。
- 新增测试：大列表排序（1000 条）的内存/性能验证（可选）。

---

## P7：SearchResult 路径字段 Arc<str> 化

### 现状
SearchResult 有 4 个 String 字段：title, subtitle, execute_id, target(ActionTarget{kind: String, value: String})。在很多构造点，这些字段共享同一个值（如 history_file_candidates 中 subtitle = execute_id = target.value = weight.target.value），但每个都做了独立 clone。

### 做法
将 SearchResult 的 String 字段改为 Arc<str>：
```rust
pub struct SearchResult {
    pub kind: SearchResultKind,
    pub title: Arc<str>,
    pub subtitle: Arc<str>,
    pub execute_id: Arc<str>,
    pub target: ActionTarget,  // ActionTarget.value 也改为 Arc<str>?
    pub match_spans: Vec<i32>,
    pub match_metadata: Option<MatchMetadata>,
}
```

### 约束（plan 文档）
- **execute_id 必须继续序列化**（wire 契约）：Arc<str> 实现 Serialize（serde 对 Arc<str> 的处理与 String 一致，序列化为 JSON 字符串）。✓
- **三处所有权来源不同，统一 Arc**：7 处构造点。
- **测试内路径断言跟随调整**：测试中 `item.title.as_str()` 仍可用（Arc<str> deref 到 str）。

### 影响面分析
- SearchResult derive Clone：Arc<str> clone 是 refcount++，零拷贝。✓
- serde Serialize：Arc<str> 序列化为 JSON 字符串。✓（serde 自动支持 Arc<str>）
- 测试中比较：`item.execute_id == "..."` → 需改为 `item.execute_id.as_ref() == "..."` 或 `&*item.execute_id == "..."`。或者用 `item.execute_id.as_str()`（如果加方法）。
- 比较函数 `compare_search_results` 中 `left.subtitle.cmp(&right.subtitle)`：Arc<str> 的 Ord 是字典序比较（比较内容而非指针），与 String 一致。✓
- `sort_search_results_with_picks` 中 `lowercased[a].cmp(&lowercased[b])` 不受影响。✓
- `app_resolved_paths.insert(r.subtitle.clone())`：HashSet<String>，需要改 r.subtitle.as_ref().to_owned() 或改 HashSet<Arc<str>>。更简单：`r.subtitle.as_ref().to_owned()`。

### ActionTarget 是否也改 Arc<str>？
plan 文档说"SearchResult 路径字段 Arc<str> 化"，没有说改 ActionTarget。ActionTarget 是 wire 类型（Deserialize），从前端 JSON 反序列化进来时是 owned String。如果只改 SearchResult 而不改 ActionTarget，那 SearchResult.target 仍是 ActionTarget{kind: String, value: String}，每次构造 SearchResult 时 target.value.clone() 仍在。

**决策：只改 SearchResult 的 title/subtitle/execute_id 为 Arc<str>，target 保持 ActionTarget（String）。**

理由：
1. ActionTarget 是跨进程 wire 类型，从 JSON 反序列化进来。改它影响面太大（resolve_target, ShellOperation, history record 等）。
2. SearchResult 中 target 的 clone 只在构造时发生一次，不是热路径。
3. 主要收益在 title/subtitle/execute_id 三个字段的共享——history_file_candidates 中 subtitle = execute_id = weight.target.value，可以用 Arc 共享。

### 构造点改动（7 处）
1. `ipc.rs:1183` collect_app_results（字面命中）：title/subtitle/execute_id 来自 app.name/app.target_path/app.launch_path
2. `ipc.rs:1214` collect_app_results（拼音命中）：同上
3. `ipc.rs:1297` process_indexer_reply：title/subtitle 来自 item.name/item.path，execute_id = item.path
4. `ipc.rs:1785` history_file_candidates：subtitle/execute_id 来自 weight.target.value
5. `ipc.rs:1935` window_result：title/subtitle 来自 entry.title/entry.app_name
6. `ipc.rs:2023` recent_windows：同 window_result
7. `websearch.rs:71` into_search_result：title/subtitle/execute_id 来自 self

### 测试改动
- 测试中构造 SearchResult 的地方（`item = |...| SearchResult { title: title.into(), ... }`）改为 `title: title.into()`（Arc<str>: From<&str>）。
- 断言 `item.title.as_str()` 或 `&*item.title` 仍可用。

---

## 实施顺序

1. **P4**（最独立，零跨文件影响）→ 验证测试
2. **P7**（SearchResult 结构变更，影响 ipc.rs + websearch.rs + 测试）→ 验证测试
3. **P3+M3**（history.rs 内部）→ 验证测试
4. **P1**（依赖 P3+M3 的 history.rs 上下文）→ 验证测试
5. 质量门 + 安装包

## 质量门
- `cargo fmt && cargo clippy && cargo test`（src/prism-core）
- `dotnet build && dotnet test`（src/Prism、src/Prism.Tests）
- ISCC 重建安装包（批次6是里程碑批次）
