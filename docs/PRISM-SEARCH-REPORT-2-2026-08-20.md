# Prism 模糊搜索与拼音搜索专项报告 2（2026-08-20 深夜）

上一份搜索报告（PRISM-SEARCH-REPORT-2026-08-20）成稿于实施方案 4 之前；本轮方案 4 的 S1~S4 已全部落地并真机验证，本文在**新基线**上重新盘点：现有做法（含今天全部改动）、联网对标（Everything 1.5a / Listary V7 / IbEverythingExt / pinyin-match 库）、剩余差距与改进路线。目标：**最完善的拼音与搜索功能**。

---

## 1. Prism 现状（2026-08-20 方案 4 落地后）

### 1.1 字面（模糊）搜索

- **匹配**：大小写不敏感子串；S4 起支持**空白 AND 分词**（`抖音 视频` 命中 `抖音-短视频.mp4`，顺序无关），四个匹配点（索引器扫描 / broker 回退 / 应用清单 / 拼音去重）同口径（`NameTerms`）。
- **排序**（S1 新契约）：class（整名精确 0 / 位置 0 前缀 1 / 子串 2）→ kind（字面 > 全拼 > 首字母）→ frecency 桶 → position → history → score；查询记忆（pick）跨 class 置顶。
- **过滤**：`ext:`（逗号 OR）、`path:`（子串 AND），Top-K 之前过滤，`is_truncated` 只统计过滤后集合。
- **范围**：CWD root（Ctrl+G / 宿主捕获），Top-K 之前按记录号祖先链过滤。
- **性能**：基准（g2 基线 vs 今日）：ascii_exact/prefix/contains p50 **44.5→14.5ms（-65%）**、p95 -64%——N1 零分配匹配、N2 并行分块扫描、F2 root 缓存的累积效果，且已含 S2 拆门 + S3 混用 DP 的新增成本。

### 1.2 拼音搜索

- **三策略顺序尝试**（每名字）：① 整名全拼（`douyin`→抖音）→ ② 整名首字母（`dy`→抖音）→ ③ **S3 混用**：逐字三选一（首字母 1 字节 / 全拼 / 尾部部分终态），连续 token 段，位掩码 DP（≤63 字节，掩码归零剪枝）——`wxin`（w+xin）、`wangyiyy`（wang+yi+y+y）今日真机验证通过。
- **存储**：sidecar 只收录含汉字的名字（紧凑编码：每 token 长度+UTF-16 偏移+读音串），增量 delta 掩蔽陈旧编码；M3 起纯英文风暴不触发重建（32k 总量硬上限兜底）。
- **门**：S2 起拼音**无条件**参与（拉丁 ≥2 字母查询），配额完整 max，合并排序后 truncate——`dy`→抖音第 2 位、`wx`→微信第 1 位（真机 A/B）。
- **多音字**：18 条最长匹配词表（重庆/音乐/银行/…），词表变更需 bump 字典版本。
- **去重**：拼音命中若被字面 AND 命中吸收则不出行（口径=NameTerms）。
- **即时路径**：apps / 窗口 / 历史候选走同一三策略（与 sidecar 紧凑编码等价有测试锚定）。

### 1.3 已明确的边界

- 子序列跳字（`dysp`→抖音短视频，跳过「短」）不做——S3 前置调查确认属 fzf 式子序列，弱命中泛滥。
- 拼音与字面跨模式组合（`抖音 dy`）不做——两套口径，收益不明。
- 混用命中借 `Initials` 档，不加协议变体（省 bump 与混装失败面）。

---

## 2. 联网对标（2026-08）

| 维度 | Listary 6/V7 | Everything 1.5a（1320a+） | IbEverythingExt 插件 | pinyin-match（JS 库） | Prism 现状 |
|---|---|---|---|---|---|
| 首字母 | ✅（早期版本唯一模式） | ✅ `pinyin:` 修饰符（官方说明**仅首字母**起步） | ✅（正则转换实现） | ✅ | ✅ |
| 全拼 | ✅ 6.0.11.35 起 | ✅ 后期 build `pinyin_type=quanpin` | ❌ | ✅ | ✅ |
| **逐字混用**（wxin） | ✅（全拼+简拼自由组合，官方宣传「全拼、简拼、英文在全路径下自由组合」；社区经验「全拼在前简拼在后最稳」） | 部分（社区称 mixed/tone，官方文档保守） | ❌ | ✅（库的招牌能力） | ✅（S3，今日落地） |
| 空格分词 | ✅（自由组合含英文 term） | ✅（空格 AND 是基本语法） | ✅ | ❌（库本身单串） | ✅（S4，字面侧；拼音侧整串） |
| 全路径拼音 | ✅（全路径模糊） | ❌（只文件名） | ❌ | ❌ | ❌（只文件名；`path:` 走字面） |
| 多音字 | 未明示 | 同音字全命中（拼音匹配的固有语义） | ✅ 明示支持多音字 | ✅（heteronym） | ⚠️ 18 条词表 + 单读音 |
| 大小写语义 | — | — | ✅ 小写=拼音或字母，大写=仅字母 | — | ❌（一律大小写不敏感） |
| 高亮定位 | ✅ | ✅ | — | ✅（返回匹配位置） | ✅（UTF-16 spans，含混用/多 term） |
| 开关方式 | 设置项 | 查询修饰符 / 高级设置 | 常驻中文系统默认开 | — | 常开（SetPinyinEnabled 可关） |

来源：[Listary 官网](https://www.listary.net/feature/search-files)、[6.0.11.35 全拼公告](https://discussion.listary.com/t/listary-6-0-11-35/7946)、[知乎：Listary 全拼/简拼/英文自由组合](https://zhuanlan.zhihu.com/p/1923675892787446270)、[Listary 论坛：模糊+优先级拼音搜索](https://discussion.listary.com/t/major-file-search-window-update-6-3-0-55-beta/8756?page=3)、[Everything pinyin: 修饰符 t=12073](https://www.voidtools.com/forum/viewtopic.php?t=12073)、[小众软件：1.5a 拼音搜索](https://www.appinn.com/everything-1-5-a/)、[zh-everything.cn](https://www.zh-everything.cn/news25.html)、[IbEverythingExt t=10541](https://www.voidtools.com/forum/viewtopic.php?t=10541)、[GitHub Chaoses-Ib/IbEverythingExt](https://github.com/Chaoses-Ib/IbEverythingExt)、[pinyin-match 介绍](https://adg.csdn.net/6970a6c0437a6b40336b0ec9.html)。

**对标结论**：S3/S4 之后，Prism 的拼音匹配能力（全拼+首字母+**任意逐字混用**+空格 AND+高亮）已达到或超过 Listary 的公开描述、显著超过 Everything 官方实现；剩余差距集中在**多音字覆盖**与**全路径拼音**两点，外加两个可选增强（大小写语义、拼音侧多 term）。

---

## 3. 改进建议（按优先级）

### P1. 多音字覆盖：词表扩充 + 逐字多读音尝试

**现状**：`to_pinyin()` 只取第一读音，18 条 PHRASES 词表兜常见多音字。`重庆人` 若写作「种庆」类未覆盖词，`chongqing` 命不中。
**对标**：IbEverythingExt 与 pinyin-match 均明示支持多音字。
**修法**（两步，可分开做）：
1. **词表扩充**（低风险）：PHRASES 从 18 条扩到 ~200 条常用多音词条（长度/字典版本 bump 一次，sidecar 重建一次）。数据源：现成开源多音字词表（如 pinyin-pro 的词表数据）。
2. **逐字多读音**（中风险）：encode 时每 token 存多读音（`readings: [&[u8]]`），匹配时对多读音 token 分支尝试——位掩码 DP 天然容纳（每个 token 的转移对每个读音各试一次）。sidecar 字节格式需 bump（encoding 变更），**拼音 sidecar 全量重建一次**（v5 索引不受影响）。
**收益**：消除「明明是常见词却搜不到」的静默失败面。
**风险**：方案 2 改存储格式与热路径（每 token 多分支），需基准门禁（rare_term/pinyin p95 回归 ≤15%）+ 负例表（防召回放宽）。

### P2. 全路径拼音（目录名拼音检索）

**现状**：拼音只编码文件名；`xz` 搜不到 `下载\资料` 里的 `资料.pdf`（按目录名）。Listary 的「全路径自由组合」覆盖此场景。
**修法**（推荐 A）：
- **A（增量、推荐）**：sidecar 增加「路径拼音首字母串」字段——构建时对**目录节点**（只目录，量级 ~1/10）编码全链首字母（`xz` = 下载\资料 的链首字母），文件查询未中时按父目录链匹配。内存增量 = 目录数 × 平均链长字节（2.4M 记录库约 30 万目录 × ~20B ≈ 6MB）。匹配只做首字母（全拼路径代价不成比例）。
- **B（重）**：每文件编码全链全拼——内存翻数倍，不建议。
**收益**：中文目录结构下的核心场景（中国用户目录名多为中文）。
**风险**：A 改 sidecar 格式（同 P1-2 一并 bump 一次）；目录链变更（rename）时 delta 需级联失效子项的路径串——建议 delta 粒度到目录子树置空重建，实现前需专门设计文档。

### P3. 拼音侧多 term（`wx 报告` 型查询）

**现状**：S4 的 AND 分词只在字面口径；拼音侧整串归一（`wx2024` 可命中 `微信2026` 型名字因数字 token 在编码里，但 `wx zfb`→`wxzfb` 要求连续）。
**修法**：normalize 后按原始空白切分多 term，每个 term 独立过三策略（混用 DP 状态按 term 前进），全部命中才算数；class 取各 term 最佳。
**风险**：中——DP 状态机从「单查询游标」变「term 游标 + 查询游标」，复杂度上一档；且跨 term 的 token 段组合爆炸需限制（term ≤3）。建议在 P1/P2 之后评估真实需求再做。

### P4. 大小写消歧（可选，对齐 IbEverythingExt 语义）

小写=拼音或字母、大写=仅字母：`DY` 只搜字面 `DY`，`dy` 才含拼音。给重度用户一个精确开关。改 `normalize_query` + 字面路径的大小写敏感分支，风险低但用户教育成本在——**建议先不做**，观察用户是否报告「拼音噪声挤占字面结果」再做（S1 的 class 优先已经大幅减少了这类抱怨）。

### P5. 观察项（不建议现在动）

- **子序列跳字**（dysp→抖音短视频）：Listary/Everything 均无此语义；fzf 式召回的误命中需要新的打分维度。维持不做。
- **音调**：无对标支持，成本高收益无。不做。
- **memchr SIMD 字面扫描**：维持「批准依赖后做」的结论（预估 ASCII 再 2-4×）。

---

## 4. 总结

今日方案 4 之后，Prism 的搜索/拼音能力矩阵已并列第一梯队（混用匹配与高亮甚至领先）。通往「最完善」的剩余三步按序为：**P1 多音字**（正确性缺口）→ **P2 目录链首字母**（场景缺口）→ **P3 拼音多 term**（便利缺口，需求验证后再做）。P1-1（词表扩充）与 P2-A 均可独立小步落地；P1-2 与 P2 共用一次 sidecar 格式 bump，应合并规划，避免用户经历两次拼音全量重建。

---

## 5. 来源
- Listary：[官网功能页](https://www.listary.net/feature/search-files)、[6.0.11.35 全拼支持公告](https://discussion.listary.com/t/listary-6-0-11-35/7946)、[6.3.0 beta 讨论（模糊+优先级拼音）](https://discussion.listary.com/t/major-file-search-window-update-6-3-0-55-beta/8756?page=3)、[用户混合输入经验](https://discussion.listary.com/t/topic/5004)、[知乎：全拼/简拼/英文自由组合](https://zhuanlan.zhihu.com/p/1923675892787446270)。
- Everything：[pinyin: 修饰符官方帖 t=12073](https://www.voidtools.com/forum/viewtopic.php?t=12073)、[1.5a 拼音搜索（小众软件）](https://www.appinn.com/everything-1-5-a/)、[zh-everything 1.5a 更新](https://www.zh-everything.cn/news25.html)、[pinyin_type 设置](https://www.isharepc.com/50859.html)、[拼音+全文攻略（知乎）](https://zhuanlan.zhihu.com/p/2028521888788292740)。
- 扩展/库：[IbEverythingExt（voidtools 论坛 t=10541）](https://www.voidtools.com/forum/viewtopic.php?t=10541)、[IbEverythingExt GitHub](https://github.com/Chaoses-Ib/IbEverythingExt)、[pinyin-match 库介绍](https://adg.csdn.net/6970a6c0437a6b40336b0ec9.html)。
- Prism 内部：`src/prism-core/src/pinyin.rs`（三策略+位掩码 DP）、`hierarchy.rs`（NameTerms/MatchMetadata 契约）、`pinyin_sidecar.rs`（紧凑编码/delta/去重口径）、`docs/PRISM-IMPL-PLAN-4-2026-08-20.md`（S1~S4 全文）。
