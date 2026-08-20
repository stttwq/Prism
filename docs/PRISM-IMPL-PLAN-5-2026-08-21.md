# Prism 实施方案 5：审计4修复 + 拼音 P1-P3 + 别名系统（2026-08-21）

三方向合并方案。来源：
- 审计4（`docs/PRISM-AUDIT-4-2026-08-20.md`）：批次 A-D（中3/低11）
- 搜索报告2（`docs/PRISM-SEARCH-REPORT-2-2026-08-20.md`）：P1 多音字、P2 目录链首字母、P3 拼音多 term、P4 大小写
- 别名设想（用户文档）：精确触发、独立召回通道、frecency 仲裁、双入口

优先级判定：**bug 修复 → 拼音 → 别名**。理由：修复项全是已知缺陷且 diff 小，
先落稳定基线；拼音改 sidecar 格式（用户要经历一次全量重建），P1-2 与 P2 共用一次
bump 必须合并做；别名是纯新增面（新存储/协议/UI），放在最后，出问题不牵连前两者。
每任务 = 实现 + 全量门禁（cargo test + dotnet test + clippy + build）+ 独立提交。

---

## 第一部分：审计4修复（批次 A-D）

### 批次 A：正确性/卡顿六小项（一批提交）

| # | 位置 | 修法 |
|---|---|---|
| A2 | HostScopeController.ToggleScope→Revalidate 在 UI 线程持 `_gate` 做同步磁盘 I/O（Directory.Exists + 枚举探权限），网络盘/休眠盘 UI 冻结数秒 | 两段式：`PrepareToggleToCurrentDirectory()` 锁内快查（开关/root 可用性），通过则返回可在后台执行的复验闭包；SearchWindow 经既有常驻 STA 线程执行，结果回 UI 调 `ApplyToggleValidation()` 提交（带 captured-window 串台守卫）。同步 `ToggleScope()` 保留（Global 方向 + 单测），行为不变 |
| A3 | SearchResult.ContainerKey 计算属性每次访问分配新串，展开 1000 行每击键 ~2000 次分配 | 惰性缓存字段 `_containerKey ??=`（record 不可变，同实例恒同键） |
| B7 | SearchWindow 隐藏 3 分钟 GC tick 与再呼出竞态 | Tick 回调先查 `IsVisible \|\| _hiding` 再收集 |
| B10 | PipeClient.AdoptExistingServer 两处复用返回不 Dispose Process（非 broker 名 / 跨会话），Kill 分支也不 Dispose | 三处 return/Kill 前 Dispose（收编成功路径除外——所有权移交 `_backend`） |
| B11 | ResultList.SetWebIconProvider 换 provider 时旧 `Resolved` 订阅未摘 | 先 `_webIcons.Resolved -= DecorateVisibleItems` 再挂新的 |
| B2 | needs_name_compact 标志先消费后竞争失败且卷转安静 → 死名字永不回收 | maintenance tick 直接调 `compact_volumes_off_lock`（内部谓词选目标，每 5s 一拍幂等），删除 AtomicBool 一次性标志与置位点 |

测试：HostScopeController 新增 Prepare/Apply 语义测试（快查失败提示、后台复验成功/失败应用、串台丢弃）；dotnet 全量 + cargo 全量绿。

### 批次 B：A1 拼音 delta 拆锁（单独提交）

`apply_pinyin_records` 的 `Arc::make_mut` 深克隆整份 sidecar → delta 表拆出为独立
`RwLock<BTreeMap<RecordKey, Option<Vec<u8>>>>`（watcher 只写 delta；搜索读主表+delta，
delta 掩蔽语义逐条对齐——`Some` 掩蔽新编码、`None` 掩蔽删除）；M3 汉字计数与 32k
上限逻辑随表迁移。sidecar 字节格式不动（delta 本就不序列化）。现有 delta 测试全保绿。

### 批次 C：边角六项（一批提交）

| # | 修法 |
|---|---|
| B1 | root 解析缓存失效粒度从 generation 放宽到「被解析卷自身 next_usn」（与流式 checkpoint 同思路）；缓存槽 1→4 |
| B3 | index_cache envelope 加 names/nodes 内容哈希（缺省 0 跳过校验，旧文件兼容；不匹配→拒绝缓存走重建） |
| B4 | zip Shell COM 回退传 FOF_WAITUNTILDONE + 轮询目录条目数稳定；输出已存在报 Conflict 而非覆写 |
| B6 | SearchViewModel.PollUntilReadyAsync 轮询 SearchAsync 传入 `_searchCts?.Token`（只影响等锁与业务丢弃，不破配对读纪律） |
| B8 | FaviconCache.KeyToFileName 统一 `Replace(':', '_')`（端口 origin 落盘名含 `:` 是 NTFS ADS 语义） |
| B9 | IconCache 键计算（无扩展名路径的 Directory.Exists 探盘）整体挪进 GetAsync 的 Task.Run 体，UI 线程零探盘 |

### 批次 D：B5 ipc 并发（单独提交）

现状：broker 单连接请求串行，一次慢搜索冻结后续全部请求（前端读超时 8s 触发断连自愈，
期间无任何响应）。修法（审计倾向项）：**仅 Search 请求 tokio::spawn**（查询通道唯一
无界计算请求；actions/resolve/ping 等本就快），响应经 mpsc 发给连接级 writer 任务，
writer 按请求序号 BTreeMap 缓冲、严格按序写出（协议无请求 id，保序是前端配对前提）。
EOF 后 read 循环等 writer 排干在途响应（search 内部自有超时，有界）。在途 search 任务
上限 16/连接（Semaphore try_acquire，超限内联回 error 仍走保序通道）。P15 的响应缓冲
复用移入 writer 任务（唯一消费者）。

测试：mock 管道上 client 连发 search+ping+search，断言响应严格按请求序到达且 ping
计算不被首 search 阻塞（观察响应间隔/完成顺序）；既有握手测试全保。

---

## 第二部分：拼音 P1-P3（P4 记录决策）

### P1-1：多音字词表扩充（独立小步）

PHRASES 18→~200 常用多音词条（数据源：开源词表人工核对，含重庆/银行/银行类地名姓氏、
打车/长大/重逢/重量/重新/重要/重复等高频）。字典版本 bump 一次（sidecar 编码变化触发
一次全量拼音重建，v5 索引不动）。负例表防召回放宽。

### P1-2 + P2：sidecar 格式 bump（合并一次重建，一批提交）

两项共用 `pinyin-v1.bin` → `pinyin-v2.bin` 的格式变更：

1. **多读音 token**：encode 时每 token 存多读音（`readings: [&[u8]]`），匹配 DP 对
   多读音 token 每读音各试一次转移（位掩码 DP 天然容纳）。即时路径（apps/窗口/历史）
   同口径。
2. **目录链首字母**：构建时对目录节点编码全链首字母串（只目录，量级 ~1/10），文件名
   三策略未中时按父目录链首字母匹配（只做首字母，全拼路径代价不成比例）。目录
   rename 的 delta 级联失效：delta 粒度到目录子树置空（宁少勿错），交 maintenance
   重建兜底。

门禁：rare_term/pinyin 基准 p95 回归 ≤15%；delta/掩蔽/去重测试全绿；编码决策等价锚
（compact vs on-the-fly）保绿。

### P3：拼音多 term（`wx 报告` 型，term ≤3）

normalize 后按原始空白切分多 term，每 term 独立过三策略（混用 DP 状态按 term 前进），
全部命中才算数；class 取各 term 最佳；spans 合并排序。term >3 整体退化为现状单串。

### P4：大小写消歧——**不做**，记录理由

搜索报告2 明确「建议先不做，观察用户是否报告拼音噪声挤占字面结果再做」：S1 class
优先已大幅减少该类抱怨，而大写=仅字面会改变全部用户的既有语义，教育成本 > 收益。
维持观察项。

---

## 第三部分：别名系统

### 语义（照设想文档）

- **多词绑定**：一个目标（文件/文件夹/应用，按 ActionTarget 落盘）可绑多个词
  （`weixin.exe` 绑 `wx`、`微信`）；持久化 `aliases-v1.json` 于用户数据目录
  （history 旁，VersionedEnvelope 版本化，corrupt→空表重来）。
- **精确触发独立通道**：查询（trim 后）与某词**完全相等**才触发；`weix` 不触发别名，
  走正常字面。别名命中是额外召回行，与正常结果合并展示，不改变正常搜索本身。
- **排序**：别名行 MatchMetadata 取 class 0（整名精确档）+ position 0，进入既有
  `MatchMetadata::cmp` 体系；同词多目标在 alias 通道内按 frecency（history 榜）
  排序，无 history 记录的冷启动按绑定时间倒序。execute 带 query 走既有 history
  记录路径——frecency 自动反映真实使用。
- **路径失效**：命中时 `Path::exists()` 复验（history stale paths 教训），失效静默
  跳过；设置页展示时同样过滤但可显式删除。
- **入口**：① 结果行右键菜单「设置别名…」（文件/文件夹/应用行）；② 设置页别名区块
  （列表：词组、路径、绑定时间；删除/批量删除；编辑词组）。不做导入导出。
- **边界**：词 trim、非空、≤32 字符、每目标 ≤8 词、全局 ≤2000 条（超限拒绝并提示）；
  词与词/目标冲突允许（仲裁规则已定）；web 关键词优先级高于别名（词撞 `bi` 等时
  web 行在前，alias 行仍出）；`ext:`/`path:` 过滤激活时应用类别名行被抑制（G7 语义：
  过滤态只出文件/文件夹）；窗口模式（`>`）不触发。

### 后端（broker）

- 新模块 `alias.rs`：`AliasStore`（`RwLock` 内表 + 持久化 + 版本 envelope），
  set/delete/list/lookup(word)。写路径 spawn_blocking 落盘（对齐 ClearHistory 纪律）。
- 协议新增：`alias_set {target, words:[..]}`（整体替换该目标词表）、
  `alias_delete {target}`、`alias_list {}` → `alias_items`。搜索响应不变——
  alias 行并入 items（search_service 在 apps/files 合并阶段插入）。
- search_service 集成点：parse_query 得 plain text → 精确匹配词表 → 存在性复验 →
  构造 SearchResult（kind 按目标、title=文件名、subtitle=路径、class 0、
  spans 空）→ 去重（同 (kind,path) 已在字面/apps 命中则保 class 0 的别名行）→
  进既有排序（picks 仍最优先）。

### 前端（WPF）

- PipeClient：`AliasSetAsync/AliasDeleteAsync/AliasListAsync`。
- SearchWindow 右键菜单：file/folder/app 行追加「设置别名…」→ 轻量输入对话框
  （预填该目标现有词，逗号/空格分隔）→ alias_set → 状态栏提示成功。
- SettingsWindow 新区块「文件别名」：DataGrid/ItemsControl 列表（词组、路径、时间），
  行内删除 + 全选批量删；打开设置时 alias_list 拉取。
- 别名行无特殊渲染（图标按路径走 IconCache；高亮无 spans）；展示层零动画新增。

### 测试

- Rust：store 增删查/持久化往返/损坏回退/上限；search 集成（精确触发、weix 不触发、
  失效路径静默、排序 class 0、去重、web 关键词优先、过滤态抑制）；IPC 三命令解析。
- C#：PipeClient payload 形状；别名行解析（无 match_spans 容错）；设置页 VM 逻辑
  （加载/删除/批量）。

---

## 总流程（用户指令逐条对应）

1. 每任务完成 → cargo test + dotnet test + clippy（0 警告）+ 双端 build → git commit。
2. 全部完成 → 总测试：全量门禁 + 三进程部署真机回归（呼出/搜索/拼音/网页/动作/
   别名/热键；重点回归历史问题：界面卡顿（长列表展开、Ctrl+G、右键）、闪退
   （进程存活、panic 日志）、broker 生命周期、内存占用）。
3. 摒弃旧文档干扰，从零重读全仓（Rust 25 文件 + C# 全量，不读 docs/ 与 git 历史），
   新发现按风险分级，修中/高；复审直到无中高。
4. 最终完整提交；dist 三件套 + 安装包重建（`scripts\prism-build.ps1` / ISCC）。
