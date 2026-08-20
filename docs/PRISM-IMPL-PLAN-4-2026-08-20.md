# Prism 实施方案 4（2026-08-20 夜）——搜索/拼音缺陷修复 + 审计 3 三批合并排期

本文档合并三份输入：

1. `docs/PRISM-FRESH-AUDIT-3-2026-08-20.md`（22 项发现，高 1 / 中 5 / 低 16，原分三批）
2. `docs/PRISM-SEARCH-REPORT-2026-08-20.md`（搜索功能对标与改进路线）
3. 用户当日反馈：**搜索 `dy` 应该能出「抖音」，实际出不来**；并提出模糊搜索与拼音搜索需要改进

第 3 项在前两份文档里都没有被识别为缺陷（搜索报告把拼音列为「Prism 独有优势」，审计 3 的 22 项里没有相关条目）。本轮通读代码后确认它是**真实缺陷，且根因有三层，不是拼音算法本身的问题**。因此本方案把它列为新的 S 批次，优先级排在审计 3 的 M/L 批次之前、H+M1 批次之后。

全文的组织原则是用户提出的约束：**对软件的影响降到最低，确保其他功能正常**。为此每一项都标注了影响面、风险、测试锚定与回滚方式，并在 §6 单列「本方案不触碰什么」——协议版本、sidecar 字节格式、拼音词典版本、v5 缓存格式全部不动，用户升级后**不需要重建索引、不需要重建拼音 sidecar**。

---

## 1. 缺陷根因分析：为什么 `dy` 搜不到「抖音」

### 1.1 实测：拼音匹配算法本身是好的

先排除最容易被误判的一层。用 `tools/pinyin-match-test` 直接调 `pinyin::match_name`：

| 名字 | 查询 | 结果 |
|---|---|---|
| 抖音 | dy | `Some(Initials, class=0, position=0)` |
| 抖音 | douyin | `Some(Full, class=0, position=0)` |
| 抖音短视频 | dy | `Some(Initials, class=1, position=0)` |
| 抖音 极速版 | dyjsb | `Some(Initials, class=0, position=0)` |
| 我的抖音 | dy | `Some(Initials, class=2, position=2)` |

`dy → 抖音` 在算法层是**命中的，而且是最高的 class=0**。问题全部在调用它的上层两道门。

### 1.2 根因 A（决定性）：索引器把拼音扫描整段跳过了

`src/prism-core/src/indexer_runtime.rs:746`

```rust
if pinyin_enabled && literal_count < max as u64 {
    // ... 这里才去扫拼音 sidecar
}
```

`literal_count` 是本次查询的**字面子串命中总数**。语义是「字面结果没填满就补拼音」。对 `dy` 这种两字母查询，字面子串命中数远超 `max`：

- `dy` 会字面命中 `body`、`study`、`ready`、`dynamic`、`Kennedy`、`*.dylib` 等等
- broker 向索引器请求的 `max = indexer_request_max(result_slots) = 3 × 8 = 24`（`ipc.rs:1768`，前端初始 8 行见 `SearchViewModel.cs:15`）
- 现网基准（`artifacts/bench/g2-20260801-installed/search-enabled-final`，2.44M 槽 / 665k 名字候选）里 `ism` 这种三字母子串查询字面命中 **4049** 条

所以 `literal_count = 数千 >> 24`，条件为假，**拼音 sidecar 一次都没扫**。索引里的「抖音」目录/文件根本不会出现在响应里。

这是一个纯粹的逻辑缺陷：「字面结果够多」和「拼音结果不该出现」是两件不相关的事。**越是短查询（拼音首字母查询天然是 2-4 个字母），字面噪声越多，拼音越是必然被跳过**——门的触发概率与拼音功能的使用场景完全正相关。

### 1.3 根因 B：排序层 kind 锁死 + Top-8 截断

即使根因 A 修好、抖音被返回了，它仍然显示不出来。

`src/prism-core/src/hierarchy.rs:182-192`：

```rust
impl Ord for MatchMetadata {
    fn cmp(&self, other: &Self) -> Ordering {
        self.kind.cmp(&other.kind)              // ← kind 是第一比较键
            .then(self.class.cmp(&other.class))
            .then(usage_tier(other.history_score).cmp(&usage_tier(self.history_score)))
            .then(self.position.cmp(&other.position))
            .then(other.history_score.cmp(&self.history_score))
            .then(self.score.cmp(&other.score))
    }
}
```

`MatchKind` 声明序为 `Literal < FullPinyin < Initials`（`hierarchy.rs:141-146`），derive 出的 `Ord` 让**任意字面命中无条件压过任意拼音命中**。`ipc.rs:2250-2253` 的最终排序把 `metadata_kind` 又单独提为第一键，语义相同。

于是 `dy` 的结果表是：几十条字面 `dy` 子串命中（`Kennedy.docx`、`body.css`……）排前，「抖音」（Initials，class=0）排在它们全部之后，然后 `ranked.truncate(result_slots)` 砍到 8 行（`ipc.rs:1640`）——抖音被截掉。

注意：开始菜单里的「抖音」应用走的是 `collect_app_results`（`ipc.rs:1301-1334`），那条路径**不受根因 A 的门限制**，拼音无条件参与。所以如果用户装了抖音客户端，它其实**被返回了，只是被字面命中挤出了前 8 行**。这解释了为什么用户看到的是「完全不显示」而不是「排在很后面」。

根因 A 和 B 是**叠加**关系，必须一起修：只修 A，抖音仍被挤出可见窗口；只修 B，索引里的抖音目录仍然不被返回。

### 1.4 拼音匹配本身的两个真实缺口（实测）

虽然 `dy → 抖音` 能过，但同类输入有明确的失败面：

| 名字 | 查询 | 现状 | 期望 |
|---|---|---|---|
| 微信 | wxin | **None** | 命中（w=首字母 + xin=全拼） |
| 网易云音乐 | wangyiyy | **None** | 命中（wang+yi 全拼 + y+y 首字母） |
| 抖音短视频 | dysp | **None** | 命中（d,y,s,p 四字首字母，第 5 字未覆盖属正常不命中） |
| 腾讯会议 | tenxunhuiyi | **None** | 不命中是对的（`teng` 缺 g，非缺陷） |

根因在 `pinyin.rs:83-84` / `109-112`：匹配只试**两个纯策略**——要么整串全拼（`PinyinMatchKind::Full`），要么整串首字母（`Initials`）——**不允许逐字混用**。`match_compact_kind` 里 `kind` 在整个循环外确定（`pinyin.rs:208-211`），一旦选定就对所有音节生效。

`dysp` 那一行需要单独说明：它不是混用问题，而是「抖音短视频」有 5 个音节而查询只给了 4 个首字母，`d y s p` 对应 `抖 音 短 视`，第 5 个音节 `频` 没被覆盖——`match_compact_kind` 允许**尾部未覆盖**（`class` 判定里的 `at_end`），所以 `dysp` 本应命中 class=1。实测 None 说明这里还有一处独立缺陷需要在实施时定位（怀疑是 `短视频` 走了 `PHRASES` 之外的多音字路径导致音节切分与预期不同）。**这一条列为 S3 的前置调查项，不是已确认的修法。**

### 1.5 模糊搜索：没有空格分词

`parse_query`（`ipc.rs:1087-1175`）剥掉 `ext:`/`path:` 后把剩余词用 `name_parts.join(" ")` 拼回单串，下游 `find_case_insensitive`（`hierarchy.rs:955`）、`literal_match_lowered`（`ipc.rs:2186`）、`apps::match_rank`（`apps.rs:77`）全部把它当**一个整体子串**。

后果：`抖音 视频` 不匹配 `抖音-短视频.mp4`；`prism 报告` 不匹配 `报告-prism-v2.docx`。Everything 与 Listary 都把空格当 AND 分隔符，这是用户预期的默认行为，也是「模糊搜索」诉求里最实际的一块。

---

## 2. S 批次：搜索与拼音修复（本轮新增，四项）

四项之间有依赖：S1 与 S2 必须同批（§1.3 末尾解释），S3、S4 可独立发布。

### S1. 排序比较键改为 class 优先于 kind

**改什么**

`hierarchy.rs:182-192`，比较顺序从 `kind → class → tier → position → history → score` 改为：

```
class → kind → tier → position → history → score
```

`ipc.rs:2242-2262` 的 `sort_search_results_with_picks` 删掉单独提前的 `metadata_kind` 比较键（第 2253 行），只保留「无 metadata 项垫底」的 `is_none()` 判定（第 2250-2252 行，那是 FRESH-AUDIT-2 G1 的修复，必须留），让 `MatchMetadata::cmp` 统一决定。

**语义变成什么**

| 场景 | 改前 | 改后 |
|---|---|---|
| 抖音（Initials, class 0） vs Kennedy.docx（Literal, class 2） | 字面赢 | **拼音赢** |
| 抖音（Initials, class 0） vs dynamic.txt（Literal, class 1） | 字面赢 | **拼音赢** |
| dy.txt（Literal, class 0） vs 抖音（Initials, class 0） | 字面赢 | 字面赢（同 class，kind 决胜） |
| 微信（FullPinyin, class 1） vs 抖音（Initials, class 1） | 全拼赢 | 全拼赢（同 class，kind 决胜） |

一句话：**「整名精确 / 前缀」这个更强的信号不再被「碰巧含子串」压制；同强度下字面仍然优先于拼音、全拼仍然优先于首字母。** 搜索报告里「匹配层级锁死不可跨越」的设计意图（字面优先、全拼优先）在同 class 内**完整保留**，只是让 class 这个更能反映匹配质量的维度先说话。

**影响面**：`MatchMetadata::cmp` 是索引器合并排序（`indexer_runtime.rs:777`）、broker 最终排序（`ipc.rs:1638`）、窗口模式取最佳（`ipc.rs:2033`）三处共用的唯一比较器。`hierarchy.rs:227-239` 的 `RankedCandidate::cmp`（索引器字面 Top-K 堆）本来就是 class 先比、且不含 kind（字面路径全是 Literal），**无需改动**；`pinyin_sidecar.rs:522-530` 的 `Candidate::cmp` 复用 `MatchMetadata::cmp`，自动跟随。

**风险**：中低。逻辑改动只有比较键换序（两处、共约 4 行），无新状态、无新分配、无协议变化。真正的风险是「排序观感变了」——这是用户明确要求的方向。

**测试锚定**（现有断言里恰好只有 2 条会翻转，其余全部保持绿）：

- `hierarchy.rs:2128` `assert!(literal_contains < full_exact_with_history)` —— 需改为断言相反方向，并把测试名与注释改成新契约（`class` 跨 `kind`，`kind` 在同 class 内锁死）
- `hierarchy.rs:2156` `assert!(rank(Literal,2,9,MAX) < rank(FullPinyin,0,0,0))` —— 同上翻转
- `hierarchy.rs:2129/2133/2137/2150/2152/2154` —— 全部仍然成立（同 class 或同 kind 内比较）
- `ipc.rs:2645 literal_title_match_beats_pinyin_match` —— **仍然成立**：`报告` 字面命中是 class 1，`bg` 拼音命中也是 class 1（`报告.docx - Word` 后面还有 token，`at_end` 为假），同 class 由 kind 决胜，字面赢。这条契约不需要改，是个好消息
- **新增**：`dy` 场景的端到端排序断言——构造「抖音」（Initials class 0）与「Kennedy.docx」（Literal class 2）两条候选，断言前者排前

**回滚**：把两处比较键换回原顺序，改回两条断言。单 commit 可逆，无数据/格式残留。

### S2. 索引器拼音门：`literal_count < max` 无条件化

**改什么**

`indexer_runtime.rs:746`：

```rust
- if pinyin_enabled && literal_count < max as u64 {
+ if pinyin_enabled {
```

`indexer_runtime.rs:759` 的配额同步放开：

```rust
- max.saturating_sub(items.len())
+ max
```

理由：拼音候选必须以**完整的 max 配额**参与后面第 777 行的合并排序，否则「字面已占满 items」时拼音仍然只能拿到 0 个槽位，等于门没拆。合并后第 784 行的 `items.truncate(max)` 保证最终条数不变。

`pinyin_sidecar::search_in_root` 内部已有 `normalize_query` 短路（`pinyin_sidecar.rs:373`）——非拉丁或不足 2 个拉丁字母的查询直接返回空，无扫描成本。所以「无条件」实际是「拉丁且 ≥2 字母的查询无条件」，中文查询、单字母查询的成本零变化。

**成本与影响面**

新增成本 = 每次拉丁查询多一遍 sidecar 全表扫描（`pinyin_sidecar.rs:388-419` 的 `self.disk.records` 循环 + `420-446` 的 delta 循环）。sidecar 只收录**含汉字的名字**，量级远小于 665k 名字候选。

现网基准里有一个可用的间接证据：`rare_term` / `no_hit` 两个查询字面命中 0 条（`literal_count < max` 为真），**它们本来就走了完整的拼音扫描**，p50 分别是 36.3ms / 34.3ms，而不扫拼音的 `ascii_contains` p50 是 47.2ms（字面命中多，Top-K 与路径构造更贵）。这说明拼音扫描在现网数据规模下**不是主要成本项**。

但这是间接证据（两组查询的字面工作量不同，不能直接相减），所以 **S2 必须带一次基准复测才算完成**，见 §5 验收门禁 G-S2。

**风险**：中低。改动 2 行，无新状态。真实风险是延迟回归——由基准门禁兜住。若复测超预算，退路是「按 sidecar 记录数分块并行扫描」（复用 `hierarchy.rs:1352 parallel_scan` 的同款分块 + 原子游标模式），而不是把门加回来。

**与审计 3 的耦合（重要排期约束）**

S2 让 sidecar 扫描频率从「偶发」变成「每次拉丁查询」，这会**放大三个已知的 sidecar 侧问题**：

- **M3**（拼音 delta 计数把纯英文名变更也计入 → 4096 上限后每 5s 全量重建）：重建期间 `pinyin.read()` 与搜索侧的 Arc 快照 clone 竞争变频繁
- **G4 引入的 Arc COW clone**（`indexer_runtime.rs:434-436` 已有 ponytail 天花板注释）：并发交叠概率上升
- **拼音 `matched_count` 把已删记录计入致 `is_truncated` 虚高**（审计 3 低批次，`pinyin_sidecar.rs:416 vs 559`）：现在每次查询都会暴露

**结论：M3 与那条 matched_count 修正必须与 S2 同批或先于 S2 落地**，不能留到 L 批次。这是本方案对审计 3 原有批次划分的唯一实质性调整。

**测试锚定**

- 新增：构造「字面命中数 > max」+「存在拼音候选」的索引夹具，断言响应里同时含字面与拼音项（现在这个夹具会拿到 0 条拼音项——是可复现的回归锚点）
- 现有 `pinyin_sidecar.rs` 的 root/exclusion/filter 断言全部保持
- `is_truncated` 语义：`matched_count` 现在恒含拼音计数，「展示更多」出现频率会略升。这是正确行为（确实还有更多结果），不需要补偿

**回滚**：改回 2 行。

### S3. 拼音支持全拼与首字母逐字混用

**前置调查（必须先做，不做完不动手）**

`抖音短视频 / dysp → None` 的原因需要先定位。预期它应该命中（4 首字母覆盖前 4 音节，尾部允许未覆盖）。可能是 `短视频` 的音节切分与预期不同，也可能是 `match_compact_kind` 的 `at_end` / 尾部处理有独立缺陷。**这一项的结论决定 S3 是「新增混用能力」还是「新增混用能力 + 修一处切分缺陷」。**

**改什么**

`pinyin.rs`：在现有 `Full` / `Initials` 两条纯策略之后，追加第三条**混用匹配**，只在前两条都失败时才跑：

```
match_compact_normalized:
    Full  →  命中则返回（现路径，零变化）
    Initials → 命中则返回（现路径，零变化）
    Mixed  →  新增
```

混用匹配用**位掩码 DP**，不用递归、不分配：

- 状态 = 「已消费的查询字节数」的集合，用一个 `u64` 位掩码表示（查询长度 ≤ 63 由 `normalize_query` 加长度上限保证；超长查询跳过混用、退回现有两条路径）
- 逐 token 推进：对掩码里每个置位 `p`，尝试两种消费——`query[p] == reading[0]`（首字母，前进 1）与 `query[p..].starts_with(reading)`（全拼，前进 `reading.len()`），以及尾部部分匹配（`reading.starts_with(&query[p..])`，终态）
- 掩码变 0 即该起点无解，立即 break（**这是关键的剪枝：绝大多数条目在第一个 token 就掩码归零，成本与现有两条路径同量级**）
- 掩码的第 `query.len()` 位置位 = 命中

`kind` 上报：混用命中**上报为 `MatchKind::Initials`**（最弱的拼音档），**不新增 enum 变体**。

> `ponytail:` 混用命中借用 Initials 档位而不新增 `MatchKind::MixedPinyin`。省掉的是：`MatchKind` 新变体 + serde 线格式新取值 + `INDEXER_PROTOCOL` 版本 bump + 新旧 broker/indexer 混装时的反序列化失败面。代价是 `wxin` 与 `wx` 同档排序。升级路径：若实测出现「混用命中被全拼命中不合理压制」的具体案例，再插入 `MixedPinyin` 变体（声明序放在 `FullPinyin` 与 `Initials` 之间）并 bump 协议版本。

**影响面**：`pinyin.rs` 单文件。`encode_compact` 不动 → **sidecar 字节格式不变** → `PINYIN_DICTIONARY_VERSION` 不 bump（`pinyin.rs:5`）→ `pinyin_sidecar.rs:653` 的版本校验通过 → **用户升级后不触发拼音全量重建**。这是「影响最小」最重要的一条：混用是纯查询侧能力，存储侧零改动。

`match_tokens`（`pinyin.rs:278`，即时匹配路径，供 apps / 窗口 / 历史候选用）需要同款混用，与 `match_compact_kind` 保持判定一致——`apps.rs:433 precomputed_pinyin_matches_the_on_the_fly_encoder` 那条测试就是锚定这两条路径等价的，必须继续绿。

**风险**：中。这是本批次唯一新增算法的一项。风险集中在两处：召回变宽后的误命中，以及 DP 的热路径成本。

**测试锚定**

- 扩充 `pinyin.rs:341 fixed_query_contract` 表：`微信/wxin → Some`、`网易云音乐/wangyiyy → Some`、`抖音短视频/dysp → Some`（视前置调查结论）
- **负例同等重要**（防召回过宽）：`微信/eix → None`（现有）、`腾讯会议/tenxunhuiyi → None`、`微信开发/wxkaifa → None`（现有，混用后仍须 None——`wx` 是前两字首字母、`kaifa` 是后两字全拼，若这条变成 Some 说明 DP 允许了不该允许的跨越，需要重新审视语义）
- `apps.rs:433` 两路径等价断言保持绿
- 新增：超长查询（> 63 字节）退回两条纯路径、不 panic

> `微信开发/wxkaifa` 这条现有负例值得单独说明：混用 DP **会**让它变成命中（w+x 首字母 + kai+fa 全拼）。这正是「混用」的定义，所以**这条断言的期望值需要在实施时明确决策**：要么接受它变成 Some（承认这就是用户想要的混用），要么给 DP 加「首字母段与全拼段不能交替超过 N 次」的约束。**建议接受变 Some**——`wxkaifa` 是一个真实用户会打出的输入。这条决策必须在改测试时显式记录在测试注释里，不能默默翻转。

**回滚**：移除第三条策略的调用（一行），DP 函数留着不调用或一并删除。sidecar 无残留。

### S4. 空格分词 AND

**改什么**

在 `hierarchy.rs` 新增一个共享的查询表示（承载「已降幂的 term 列表」），并让**所有名字匹配点**都走它：

| 调用点 | 文件:行 | 说明 |
|---|---|---|
| 索引器字面扫描 | `hierarchy.rs:920 match_metadata` | 热路径，逐 term 调 `find_case_insensitive` |
| broker 字面匹配 | `ipc.rs:2186 literal_match_lowered` | apps / 窗口 / 历史候选 / 索引器回退高亮共用 |
| 应用清单 | `apps.rs:77 match_rank` | 目前是 `name_lower.contains(query_lower)` |
| 拼音去重 | `pinyin_sidecar.rs:463 key_is_literal` | 判「已被字面命中」的口径必须与字面路径一致，否则会重复出行或漏行 |

**不改** `QueryFilters::path_matches`（`hierarchy.rs:1074`）——`path:` 已有自己的 AND 语义，与名字分词无关。

**语义定义**（单 term 时必须与今天逐字节等价）

- terms = 查询按空白切分、丢弃空串、逐个降幂
- 命中 = **每个 term 都是名字的子串**（AND，顺序无关）
- `class`：单 term 时与今天完全一致（整名相等=0 / 位置 0 =1 / 否则 2）；多 term 时 = 「任一 term 在位置 0」→1，否则 2（多 term 不产生 class 0）
- `position` = 各 term 命中位置的最小值
- `score` = 名字 UTF-16 长度（不变）
- 高亮 spans = 每个 term 一段，**按起点升序合并重叠段**后输出（`Controls/ResultList.xaml.cs:527-540` 用 `cursor = Math.Max(cursor, end)` 推进，要求 spans 升序且不倒退，必须满足）

**这是能力扩张而非行为改变**：今天 `抖音 视频` 匹配 0 条，改后匹配「同时含两段」的名字，原先能匹配的（名字里真的有空格的）仍然匹配（两个 term 都在里面）。另外顺手修掉一个长期毛刺：尾随空格的查询（`prism ` ）今天不匹配 `prism.exe`，改后匹配。

**影响面**：4 个调用点 + 1 个共享类型。热路径成本：单 term 时与今天相同（一次 `find_case_insensitive`）；N term 时 N 次。`parse_query` 已经把 `ext:`/`path:` 剥离并 `join(" ")`（`ipc.rs:1174`），无需改动。**协议零变化**——查询仍以单个字符串过管道，分词发生在两端各自的匹配层。

**风险**：中低，但**调用点分散是主要风险**。漏掉 `key_is_literal` 会导致同一条结果同时以字面项与拼音项出现两次（用户可见的脏结果）。所以实施时必须用「查全所有 `find_case_insensitive` / `contains(query_lower)` 调用者」而非只改 ticket 点名的那一处。

**测试锚定**

- 单 term 等价性：现有 `hierarchy.rs` / `ipc.rs` / `apps.rs` 全部字面匹配断言保持逐字节绿（这是主要安全网）
- 新增：`抖音-短视频.mp4` × `抖音 视频` → 命中，spans 两段升序不重叠
- 新增：`prism ` （尾随空格）→ 与 `prism` 同结果
- 新增：`a b` 只含 `a` 的名字 → 不命中（AND 不退化成 OR）
- 新增：`key_is_literal` 与 `match_metadata` 多 term 口径一致 → 同一条不同时出现在字面与拼音结果里
- `ext:`/`path:` 组合：`报告 视频 ext:mp4` 三者同时生效

**回滚**：共享类型保留单 term 快路径，回滚 = 让构造函数不再切分（一行）。

---

## 3. 合并后的实施顺序

批次内一起改一起测一起提交；批次之间是硬顺序，前一批全量门禁绿才动下一批。

### 批次 1：H1 + M1 + M2（恢复策略收敛性）—— 不变，仍是最高优先

来自审计 3 §3。这批与搜索功能无关，但决定「搜出来的是不是最新的」，是搜索工具的可信度底线，且**先做能避免后面调试搜索问题时被陈旧索引误导**。

- **H1** `indexer_runtime.rs:896-899` rebuild 队列 drain 按 volume_id 去重保留每卷最新（Full 优先），循环处理
- **M1** `indexer_runtime.rs:1595-1613, 951-993` SingleVolume 重建加退避（复用 `apps::app_scan_retry_delay` 模式）+ 失败后延迟重试
- **M2** `indexer_runtime.rs:1081-1086, 1804-1819` 停机路径跳过 `rebuild_pinyin_from_live`，只做 v5 落盘；拼音重建挪 maintenance tick

新增集成测试：「双卷同时失败 → 两卷都重建」、「journal 持续被删 → 重建有退避」。

**风险**：中（H1 改语义，方向明确：丢新 → 留新）。

### 批次 2：M3 + 拼音 matched_count 修正 + S1 + S2（让 `dy` 出「抖音」）

M3 和 matched_count 从审计 3 的原批次里**提前**到这里，理由见 S2 的「与审计 3 的耦合」。

- **M3** `pinyin_sidecar.rs:25,324-338` + `indexer_runtime.rs:1032-1047`：delta 只统计含汉字编码的条目；风暴期重建退避（60s 级）
- **matched_count** `pinyin_sidecar.rs:416 vs 559`：`FLAG_PRESENT` 检查前移，已删记录不计入 → `is_truncated` 不再虚高
- **S1** 排序 class 优先于 kind
- **S2** 索引器拼音门无条件化

**顺序（批次内）**：M3 与 matched_count 先落，再落 S2（拆门），最后 S1（排序）。这样每一步都能独立观察：拆门后能在响应里看到拼音项，改排序后能在前 8 行看到。

**验证**：改完必须在真机上用 `dy` / `wx` / `bdwp` 三个查询确认「抖音 / 微信 / 百度网盘」进入前 8 行。**这一步不能只靠单元测试宣布完成**——单元测试用的是小夹具，而缺陷的成因正是「真实磁盘上字面噪声足够多」。

### 批次 3：S3 + S4（拼音混用 + 空格分词）

两项都是能力扩张，与批次 2 的修复解耦，单独一批便于回滚。S3 的前置调查（`dysp` 为什么 None）先做。

### 批次 4：M4 + M5（挂死面）

- **M4** `shell.rs:200-226` STA worker `recv` 加 60s 级超时返回 System 错误；模态类动词单独标注预期阻塞（与前端 F6 的 5 分钟兜底取齐）
- **M5** `IndexerGenerationClient.cs:151,125-139` 长轮询读加 31s 竞速 + Dispose 底层流（照抄 `PipeChannel.HandshakeAsync` 模式），走既有 1s 退避重连

**风险**：中低。M5 是照抄既有模式，M4 需要区分合法长对话框。

### 批次 5：L（打磨，按价值挑拣）

审计 3 §3 批次 L 的剩余项（M3、matched_count 已提前到批次 2）：

- **Rust**：maintenance tick 三处读锁挪 `spawn_blocking`（`indexer_runtime.rs:998-1008,1051-1053`）；`rollback_mutations` 回滚 `names_fingerprint`/`dead_name_bytes`/`present_slots`（`hierarchy.rs:778-784`）；流式 checkpoint 竞争回落改按卷 `next_usn` 比对（`indexer_runtime.rs:1711-1737`）；broker 侧连接上限 8 + 空闲超时（`ipc.rs:434-500`，照搬 indexer 的 `try_admit_connection`）；history 节流脏数据加定时 flush（`history.rs:343-370`）
- **C#**：全局异常兜底限流 + 后台写日志（`App.xaml.cs:375-381,354-366`）；空查询 GC 降为 Optimized（`SearchWindow.xaml.cs:166-171`）；Combo 热键 fallback 分支补 `StartHookRefresh`（`HotkeyService.cs:80-91`）；三处 `Dispatcher.Invoke` → `BeginInvoke`（`SearchWindow.xaml.cs:154-158,796-799,831-833`）；`Task.Delay` 释放（`PipeClient.cs:1086-1091`）；favicon 完成即重绘（`WebIconProvider.cs:121-152`）；常驻 STA 线程做宿主识别（`SearchWindow.xaml.cs:282-294`）；管道名加会话限定防 RDP 跨会话互杀（`PipeClient.cs:149`）

**风险**：低，单点局部。

### 批次 6+：搜索报告的功能路线（本方案不实施，只固化触发条件）

| 项 | 触发条件 |
|---|---|
| memchr SIMD 子串扫描 | 需人工批准 ~30KB 新依赖。批准后做，锚定「SIMD 路径与现路径结果逐字节相等」 |
| 空查询全局推荐（对标 Listary Launcher） | 改动小（`ipc.rs` 空查询分支放开 root 限制 + 前端不停留 Idle），但与「呼出即搜 CWD」有心智冲突，需先定成设置项。**建议在 S 批次全部稳定后再做**——先让有输入的搜索正确，再谈无输入的推荐 |
| 日期/大小过滤语法 | 走搜索报告 §3.3 的降级实现（稀疏属性 sidecar，16B/条，Top-K 之后二次筛），不加 v6 格式、不全量重扫 |
| FAT/exFAT | 维持三档路线图，用户提出需求再做 |

---

## 4. 对「影响降到最低」的具体保证

### 4.1 不动的东西（逐项确认过）

| 面 | 结论 | 依据 |
|---|---|---|
| `INDEXER_PROTOCOL` | **不 bump**（保持 2） | S1/S2/S4 无线格式变化；S3 借用 Initials 档不新增 enum 取值 |
| broker 协议 | 不变 | 查询仍为单字符串，分词在两端匹配层 |
| 拼音 sidecar 字节格式 | 不变 | `encode_compact` 零改动，混用是查询侧能力 |
| `PINYIN_DICTIONARY_VERSION` | **不 bump** | 存储的音节编码未变 → `pinyin_sidecar.rs:653` 校验通过 → **用户升级后不重建拼音** |
| v5 索引缓存格式 / `names_fingerprint` | 不变 | 无字段增删（对比：搜索报告的日期/大小语法会动，那一项本方案不做） |
| C# 前端 | 除非高亮 spans 需要 → **无改动**。`MatchKind` 在 C# 侧完全不存在（全量 grep 确认），排序全在 broker | `src/Prism` 无 `MatchKind`/`match_kind` 引用 |
| 三进程架构 | 维持 | 审计 3 §3「明确不做」第 1 条 |
| 倒排 / trigram / 属性全量索引 | 维持不做 | 审计 3 §3「明确不做」第 3 条 |

**净结果：用户升级后，索引与拼音 sidecar 都不需要重建，冷启动路径与内存占用不变。**

### 4.2 「确保其他功能正常」的检查清单

S1（排序）与 S4（分词）触碰的是全局共用路径，所以这些**共用同一比较器/匹配器但不属于本次诉求**的功能必须逐项回归：

- 窗口模式（`>` 前缀）：`rank_window` 用同一个 `MatchMetadata::cmp` 取最佳 → 验 `ipc.rs:2645/2653/2661/2667/2674` 五条测试 + 手工验 `>` 搜窗口
- 网页模式（`g/b/bi + 空格`）：`websearch::try_match` 在 `parse_query` 之后、分词之前介入（`ipc.rs:1516`）→ **`g 抖音` 这类「引擎前缀 + 空格 + 词」的输入必须先验**，确认 S4 的分词没有改变引擎前缀识别
- `ext:` / `path:` 过滤：与分词组合（`报告 视频 ext:mp4`）
- 范围搜索（`Ctrl+G` / CWD）：root 过滤在 Top-K 之前，与 S2 的配额放开无交互，但要验拼音项也受 root 约束（`pinyin_sidecar.rs:395` 已有，验回归）
- 查询记忆（pick 置顶）：`ipc.rs:1630-1638`，pick 标记在 `metadata_kind` 之后、`match_metadata` 之前插入 —— **S1 删掉 `metadata_kind` 这一层后，pick 的相对位置变了**（原来在 kind 之后 class 之前，现在在 `is_none` 之后 `MatchMetadata::cmp` 之前，即在 class 之前）。这让 pick 的权重**变强**（跨 class 置顶）。方向与「上次选的就是第一条」的设计意图一致，但必须显式确认并补一条测试
- 历史 frecency 档位：`usage_tier` 位置从「kind、class 之后」变成「class、kind 之后」，跨越关系不变（仍不跨 class、不跨 kind）→ `hierarchy.rs:2141` 那组断言全绿即可
- 空查询根目录 MRU：`empty_query_results` 走 `MatchKind::Literal, class 0` 齐平（`ipc.rs:1892-1898`），S1 后仍齐平 → MRU 顺序不变
- 应用/文件跨 kind 去重：`app_resolved_paths`（`ipc.rs:1570-1579`）与 `injected_history_targets`（`ipc.rs:1544`）→ S2 让拼音项变多，去重集合的覆盖必须验（拼音命中的应用与拼音命中的文件指向同一 exe 时不能出两行）

---

## 5. 验收门禁

每批次收尾都要全绿，不允许「先提交后修测试」。

| 门 | 内容 |
|---|---|
| G-全量 | `cargo test`（现 350）+ `dotnet test`（现 221）+ `cargo clippy -- -D warnings` 零警告 |
| G-S1 | 排序契约测试改写完成且**在测试注释里写明新契约**（class 跨 kind、kind 在同 class 内锁死）；§4.2 清单逐项手工验过 |
| G-S2 | **基准复测**：`tools/bench/Invoke-SearchBaseline.ps1` 全查询集，`ascii_exact` / `ascii_prefix` / `ascii_contains` 的 p50/p95 相对 `g2-20260801-installed` 基线的回归 ≤ 15%；超预算则改走分块并行扫描，不把门加回来 |
| G-S2-真机 | `dy` → 抖音、`wx` → 微信、`bdwp` → 百度网盘，三者都在**前 8 行**（不是「在 1000 行里」）。用未修复版本做一次 A/B 对照，证明缺陷可复现、修复有效 |
| G-S3 | 正例与负例表都绿；`apps.rs:433` 两路径等价断言绿；`微信开发/wxkaifa` 的期望值决策写进测试注释 |
| G-S4 | 单 term 等价性断言全绿（主安全网）；`key_is_literal` 口径一致性测试绿；`g 抖音` 网页模式手工验过 |
| G-交付 | 重建 `dist` 与安装包；更新 `.trellis/spec` 相关契约文档 |

`G-S2-真机` 的 A/B 要求是刻意的：本缺陷的成因是「真实磁盘上字面噪声足够多」，小夹具单元测试**天然测不出来**——这正是它躲过前两轮独立审计的原因。

---

## 6. 明确不做（本轮）

1. **不新增 `MatchKind::MixedPinyin`**：见 S3 的 ponytail 注释。省协议 bump 与混装反序列化失败面，代价是 `wxin` 与 `wx` 同档。
2. **不做子序列模糊匹配（fzf 式）**：`dyshp → 抖音短视频` 这类跳字匹配召回强但弱命中泛滥，需要新的打分维度，热路径与排序改动都远大于空格分词。空格分词 + 拼音混用先上，观察是否还有真实缺口。
3. **不做拼音与字面的混合查询**（`抖音 dy`）：拼音侧 `normalize_query` 剥空格后按整串处理，与字面侧的 term AND 是两套口径。跨口径组合需要统一的 term 模型，收益不明。
4. **不为拼音结果单独留可见槽位**：S1 的 class 优先已经让强拼音命中自然胜出，配额是绕过排序问题而不是解决它。
5. **不动三进程架构、不做倒排/trigram/属性全量索引、不做预览面板与多选**：沿用审计 3 §3 与搜索报告 §3.6 的结论。
6. **memchr SIMD 与日期/大小语法本轮不实施**：前者等依赖批准，后者等 S 批次稳定。

---

## 7. 一句话结论

`dy` 搜不到「抖音」不是拼音算法的问题（实测 `match_name("抖音","dy")` 返回最高档 `Initials/class=0`），而是上层两道门叠加：**索引器在字面命中数超过 max 时整段跳过拼音扫描**（`indexer_runtime.rs:746`，而短拼音查询天然伴随大量字面噪声，所以这道门几乎必然触发），以及**排序把 kind 放在 class 之前**（`hierarchy.rs:184`）让任意字面子串命中压过整名精确的拼音命中，再被 Top-8 截断。修法是 S1+S2 两项共约 6 行改动，不动协议、不动 sidecar 格式、不重建索引；顺带把实测存在的拼音混用缺口（`wxin`/`wangyiyy` 不命中）与模糊搜索缺口（无空格分词）在独立批次补上。审计 3 的三批仍按原优先级执行，唯一调整是 **M3 与拼音 matched_count 修正从 L 批次提前到与 S2 同批**——因为拆掉拼音门会显著提高 sidecar 扫描频率，放大这两个已知问题。
