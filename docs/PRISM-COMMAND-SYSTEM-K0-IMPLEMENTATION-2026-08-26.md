# Prism 统一命令系统 K0 实施方案（施工手册）

> **文档性质**：施工手册。承接《PRISM-COMMAND-SYSTEM-DESIGN-2026-08-22.md》（v3，2026-08-26 修订）的 §12「K0：兼容地基」，只覆盖 K0，不涉及 K1–K4。
> **日期**：2026-08-26。基线提交 `41df5b3`（feature 分支）。
> **输入**：① 设计文档 v3；② 2026-08-26 对工作树的逐文件实测（行号以本日工作树为准）；③ `cargo clippy --all-targets` 实跑基线。
> **适用对象**：负责施工的模型/开发者。本文给出「关键点在哪 / 怎么做 / 为什么这么做」，与设计文档冲突处以本文为准，偏差已在 §2 逐条登记。
> **本文不改产品代码**。工作树中现有 `artifacts/*.ps1` 与本方案无关，不要动。

---

## 0. 一页速览

**K0 是什么**：把命令系统的类型、存储、能力协商、目录读路径放进仓库，且**不产生任何用户可见变化**。

**K0 完成后的可观测事实**（这三条就是验收的实质）：

1. 未协商 `commands_v1` 的连接，其 `hello` 回包与 search 回包 JSON 与 K0 前**逐字节一致**；
2. 已协商的连接能拉到一份非空命令目录（1 条不可达的内置命令）与一个单调递增的 generation；
3. 搜索结果里永远不出现 `kind=command`（K0 无生产者），命令 target 在 broker 的每一条既有执行路径上都被显式拒绝。

**K0 不是什么**：不执行任何命令、不路由关键字、不改动排序、不动 `history-v2.json` / `ActionId` / 网页模式 / 暂存区 / `settings.json`。

**工作量**：Rust 8 个任务 + C# 2 个任务，4 个提交，3–5 天。

---

## 1. 总体施工原则

| # | 原则 | 具体约束 | 为什么 |
|---|---|---|---|
| **P1** | **纯加法，不顺带重构** | 不动 `ipc.rs` 现有的六跳穿参（`lib.rs`→`main.rs`→`serve`→`BrokerShared`→`handle_connection`→`dispatch_non_search`），不引入 `BrokerContext`，不整理既有 `#[allow(clippy::too_many_arguments)]` | K0 的全部价值是「零行为变化地放好地基」。任何重构都会把回归面从「新代码」扩大到「全部搜索/动作路径」，而这些路径已经承载了 window token、别名置顶、前缀缓存、mutation 超时等一批用血换来的约束 |
| **P2** | **零用户可见变化** | 判定标准不是「看起来没变」，而是「未协商连接的响应 JSON 逐字节一致」，用测试锚定（§5 测试 9、11、17） | 「肉眼没看出变化」在 IPC 层不可验证。字节级断言是唯一能在回归里失败的形式 |
| **P3** | **能力协商是唯一开关** | 没有 `commands_v1` 交集 → 一行命令都不产生、一个命令请求都不受理 | 实测 `ActionTarget.FromLegacy`（`SearchResult.cs:19-31`）把未知 kind 映射成 `"file"`——旧前端**不会**安全忽略 `kind=command`，它会把命令 id 当文件路径交给 Shell 链路。设计 P6 不是洁癖，是必需 |
| **P4** | **新执行面先关着** | K0 只接目录**读**路径（`CommandList`）。不接 `ExecuteCommand`，不接目录 mutation 的 IPC 端点 | broker 管道对同用户的任何本地进程开放（`current_user_sid` ACL，`ipc.rs:410`）。一个没有消费者的 mutation/执行端点是纯攻击面，收益为零 |
| **P5** | **类型收口在编译期** | `TargetKind` 加变体后，靠 `match` 穷尽让编译器把所有必须拒绝的点逼出来；禁止用运行时字符串判断补漏 | `allowed_actions`（`actions.rs:190`）、`as_str`、`wire_str` 都是 `match`，加变体即编译失败。让编译器当检查表比人肉 grep 可靠 |
| **P6** | **存储照抄 `AliasStore` 家规** | `RwLock<Data>` + `Mutex<()> persist_lock` + tmp 文件 + `write_all` + `sync_all` + `fs_util::atomic_replace` + 损坏隔离留档并回写合法空表 | `alias.rs:39-101, 213-243` 的每一行注释都对应一个已修复的真实缺陷（撕裂 JSON、静默清零、掉电零填充）。发明第二套存储等于把这些缺陷重做一遍 |
| **P7** | **一个契约留一个会失败的测试** | 每条新边界都要有断言，且断言必须在契约被破坏时**真的**失败 | 参见 §8 陷阱 7：给 `shell_execute` 的拒绝写单测会真的调 Win32，测出来的是环境不是契约 |
| **P8** | **回滚零数据损失优先于存储纯粹性** | 命令数据一律进新文件，绝不写入 `history-v2.json` / `settings.json` | `persistence.rs:98-103` 的 kind 校验是封闭的，`history.rs` 校验失败即隔离重置。往 history 里塞 `"command"`，回滚到旧 broker 的表现是**用户全部使用历史被清空** |

> **施工纪律**：P1 与 P4 会让你几次想「顺手做了更省事」。不要。K0 的产出物是「K1 只需加消费者」，不是「K1 少写两行」。

---

## 2. 复核结论：设计文档与当前代码的 7 处偏差

全部经本日实测确认，施工按本节结论执行。

| # | 设计文档表述 | 实测事实 | K0 处置 |
|---|---|---|---|
| **D1** | §12 K0 验收门含「clippy `-D warnings` 通过」 | `cargo clippy --all-targets` 当前 **exit 0 但有 2 条既存 warning**：`src/ntfs.rs:130` 与 `src/zip.rs:160` 的 `chunks_exact_to_as_chunks`。仓库也没有 clippy 门禁——`scripts/hooks/pre-commit` 只跑 `cargo fmt -- -l --check` | 门槛改为**基线对比：不新增 warning**（基线 2 条）。清理这 2 条属无关改动，若要做请开独立提交，不进 K0 diff |
| **D2** | §9.1「新增持久化文件时同阶段更新 `dist/prism.iss` 的保留/卸载策略」 | `dist/prism.iss:124` 的 `[UninstallDelete]` 只删 `{app}\data`（便携安装整目录）、`{commonappdata}\Prism`、`{localappdata}\Prism\favicons`。`%LocalAppData%\Prism\history-v2.json` / `aliases-v1.json` **本就在卸载后保留**，是既定行为 | **K0 无需改 `.iss`**。命令两个文件落在同一 `data_dir`（`config.rs:98` 的探测结果），便携安装随 `{app}\data` 一并清理，用户级安装与 history/alias 待遇一致。要改就是改 history/alias 的既定策略，不属 K0 |
| **D3** | §12 K0 含「目录读写往返、并发 mutation 不撕裂」验收项，§9.3 列出 `CommandSet`/`CommandDelete`/`CommandSetEnabled` | 这些请求在 K0 没有任何消费者（用户命令设置页在 K3） | mutation 在 **store 层**实现并测试（覆盖该验收项），**不接 IPC**。理由见 P4。IPC 端点随 K3 的设置页一起上 |
| **D4** | §9.3 的 IPC 增量清单 | 该清单是命令系统**全量**协议面，不是 K0 清单 | K0 只做：`hello` 的 capabilities/features、`Search.command_context`（只解析不消费）、`CommandList`/`CommandCatalog`、`Results.cacheable` + `command_catalog_generation`、`SearchResultKind::Command`。`ExecuteCommand`、`RecordCommandOutcome`、`ValidateTriggerNamespace`、`ActionItem.invocation_kind` 全部不在 K0 |
| **D5** | §12 K0「broker 内置目录骨架」未说是否为空 | 空目录会让 `CommandList` 往返、DTO 容忍解析、三组兼容矩阵**全部没有非空数据可测** | K0 内置目录放 **1 条**：`prism.settings.open`（`owner=ui`、`danger=normal`、**所有 binding 为 null**）。全 binding 为 null 使它在 K0 结构上不可达，同时给测试提供真实数据。K1 再填 `root_search`/`keyword` |
| **D6** | §12 K0 含 `SearchCommandContext` 类型 | 若 K0 只定义类型不上线，K1 要同时调试「WPF 构造 + 线路 + broker 解析 + 消费」四件事 | K0 **上线发送**（仅协商后的连接），broker 解析 + 校验 + 丢弃，不消费。K1 只加消费者。未协商连接的 search payload 保持逐字节旧格式（测试 17 锚定） |
| **D7** | 设计未提 `SearchContext.IsEquivalentTo` | `TryFilterCompleteCache`（`SearchViewModel.cs:1497-1523`）用 `cached.Context.IsEquivalentTo(_searchContext)` 判定缓存可用。K1 上线根搜索命令 lane 后，command_context 变化（换宿主再呼出）会导致**旧缓存被跨 current_folder 复用**：上次无命令行的响应被子串过滤后继续服务，用户看不到本该出现的命令行 | K0 把 `CommandContext` 加进 `SearchContext` 但**不**纳入 `IsEquivalentTo`（K0 无消费者，纳入只会造成无谓重搜），并在该方法上留明确注释：**K1 必须纳入**。`Results.cacheable=false` 只覆盖「含命令行的响应」，覆盖不到这个场景 |

---

## 3. 施工顺序与提交切分

**Rust 全部先于 C#**。理由：C# 侧只能对着**已经确定的线上字段**写解析与逐字节兼容测试；反序施工会产出对不上的 DTO，然后用「改 DTO 迁就测试」收场。

```
提交 1  feat(commands): 命令目录与使用记录的存储与 schema（K0-1）
        T1  persistence.rs 两个 schema + commands.rs 存储层
        自测：cargo test（store 层全绿），不接任何 IPC

提交 2  feat(commands): TargetKind::Command 与全部拒绝点（K0-2）
        T2  shell.rs 变体 + 5 处拒绝点 + IPC 层显式守卫
        自测：cargo test（编译器逼出的每个 match 分支都有断言）

提交 3  feat(ipc): hello 能力协商 + CommandList + Results.cacheable（K0-3）
        T3 → T4 → T5 → T6 → T7 → T8（顺序内部有依赖，见下）
        自测：cargo test + protocol_tests + pipe-roundtrip 手动夹具

提交 4  feat(commands): 前端 DTO / 目录服务 / 命令行安全化（K0-4）
        T9 → T10
        自测：dotnet test + dotnet build -c Release + 三组兼容矩阵
```

每个提交必须**自身可编译、可测、零行为变化**（P2 在每个提交边界都成立）。提交 3 内部依赖：T3（连接级能力状态）是 T6 的前置；T4 的 `SearchResultKind` 是 T5 sanitize 测试的前置。

---

## 4. 任务详解

### 4.1 T1 — 命令目录与使用记录的存储层

**落点**：新建 `src/prism-core/src/commands.rs`；`lib.rs:3-25` 按字母序加 `pub mod commands;`（在 `pub mod config;` 之前）；`persistence.rs` 加两个 schema。

#### 4.1.1 `persistence.rs` 增量

```rust
pub const COMMANDS_SCHEMA_VERSION: u32 = 1;
pub const COMMAND_USAGE_SCHEMA_VERSION: u32 = 1;

// 上限常量（独立命名，不复用 ALIAS_*，理由见下）
pub const COMMAND_MAX_ENTRIES: usize = 512;
pub const COMMAND_ID_MAX_BYTES: usize = 128;
pub const COMMAND_MAX_KEYWORDS: usize = 4;
pub const COMMAND_KEYWORD_MAX_CHARS: usize = 16;
pub const COMMAND_TITLE_MAX_CHARS: usize = 64;
pub const COMMAND_SUBTITLE_MAX_CHARS: usize = 128;
pub const COMMAND_USAGE_MAX_ENTRIES: usize = 2000;
```

`CommandData { commands: Vec<UserCommandDefinition> }` 与 `CommandUsageData { entries: Vec<CommandUsageEntry> }`，各 `impl VersionedData`，走既有 `VersionedEnvelope`（`persistence.rs:10-43`：`new()` 写前 validate，`into_compatible()` 拒绝未来版本再 validate）。

`UserCommandDefinition` 的字段集**刻意不含 `owner`**：用户命令的 owner 恒为 broker、trust 恒为 `user`，由代码赋值，不从 JSON 读。

`CommandUsageEntry { id, success_count, last_success_utc, frecency_milli }`——**不存参数、不存 query**（设计 §14 R14）。

**为什么不复用 `ALIAS_*` 常量**：别名词是用户词表（≤32 字符、≤8 个、精确整词匹配）；命令关键字进的是与网页引擎共享的**独占路由命名空间**（≤16 字符、≤4 个）。共享常量会让任一侧的调参意外改动另一侧的路由语义。

**为什么 title/subtitle 按 `chars().count()` 而不是字节**：标题是中文为主的展示文本，字节上限会让「在此打开终端」这类正常标题在 UTF-8 下提前触顶。路径类字段继续用字节（对齐 `MAX_PATH_BYTES`）。

#### 4.1.2 `CommandStore`

```rust
pub struct CommandStore {
    path: PathBuf,              // commands-v1.json
    usage_path: PathBuf,        // command-usage-v1.json
    state: RwLock<CommandData>,
    usage: RwLock<CommandUsageData>,
    persist_lock: Mutex<()>,        // 目录写盘串行
    usage_persist_lock: Mutex<()>,  // 使用记录写盘串行
    generation: AtomicU64,
}
```

**为什么两份文件塞进一个 store**：`BrokerShared`（`ipc.rs:375-389`）→ `handle_connection`（8 参）→ `dispatch_non_search`（7 参）是六跳穿参；两个 store 就是两个新句柄穿六跳。一个句柄、两把 persist 锁，是在 P1（不重构）下的最小代价。两把锁而不是一把：目录写与使用记录写没有共享不变式，共用一把锁会让 K1 的高频 usage 写阻塞目录读写。

必须实现的方法（K0 全部有测试，只有 `catalog`/`generation` 接 IPC）：

| 方法 | K0 是否接 IPC | 要点 |
|---|---|---|
| `load(data_dir) -> Self` | — | 照抄 `alias.rs:53-101`：读不出/解不开/未来版本 → `crate::history::isolate(&path, now)` 留档（`history.rs:801`，`pub(crate)`，同 crate 可直接调）→ 空表起步 → **立即回写**合法空表 |
| `catalog() -> Vec<CommandDescriptor>` | ✅ | 内置静态表 + 用户表合并；过滤 `enabled=false`；`owner=broker` 的项还要过滤「无注册 handler」（见 T8） |
| `generation() -> u64` | ✅ | `load` 后为 **1**（0 保留给「未知/未协商」语义） |
| `set(def) / delete(id) / set_enabled(id, bool)` | ❌（D3） | 校验前置（照抄 `alias.rs:108-121` 的教训：**先校验目标再改内存态**，否则内存改成功、落盘失败，此后每次 mutation 都卡在同一条坏数据上直到重启）；成功后 `generation.fetch_add(1, Release)` |
| `clear_usage() -> Result<(), String>` | ✅（经 `ClearHistory`） | 清内存 + 删/重写文件 |
| `record_success(id, now)` | ❌（K1） | K0 只留签名与单测；写盘受 `HistoryEnabled` 门控 |
| `persist() / persist_usage()` | — | 锁内取快照 → `VersionedEnvelope::new` → `to_vec_pretty` → tmp `write_all` + **`sync_all`** → `fs_util::atomic_replace`（`fs_util.rs:19`）。`sync_all` 不能省：`alias.rs:231-241` 记录了掉电后目标文件零填充、下次启动被隔离清零的真实路径 |

#### 4.1.3 `ClearHistory` 联动（必须在 K0 做）

`ipc.rs:1114-1134` 的 `Request::ClearHistory` 目前只 `history.clear()`。K0 改为在同一个 `spawn_blocking` 里清两份，**两者都成功才回 `Status`**，任一失败回 `Error`。

**为什么 K0 就要做**：K0 没有 usage 写入方，所以现在做是 5 行；K1 一上执行路径就有写入方，那时忘了做就是一个隐私缺陷（设计 §10 第 9 条、§14 R14）。顺序上「先补消费者、再上生产者」是本方案的一贯手法。

---

### 4.2 T2 — `TargetKind::Command` 与全部拒绝点

**这是 K0 安全性的核心任务。** 命令 id 一旦被当成路径或 URL 交给 Shell 层，capability gating 就白做了（设计 §5.2）。

#### 4.2.1 `shell.rs` 变体与校验

- `TargetKind`（`shell.rs:18-25`）加 `Command`；`as_str()`（`shell.rs:537`）→ `"command"`；`parse()`（`shell.rs:547`）→ `"command" => Some(Self::Command)`。
- `ActionTarget::validate()`（`shell.rs:482-533`）两处改动：
  1. 字节上限选择（`shell.rs:503-507`）当前是 `if kind == Web { MAX_WEB_BYTES } else { MAX_PATH_BYTES }`，改成 `match`，`Command => 128`。**Web 与其余分支的取值必须逐字不变**。
  2. 末尾 `match kind` 加 `Command` 分支：值必须匹配 id 语法——`prism.` 或 `user.` 前缀，其余字符仅 `[a-z0-9._-]`，无空白。通用前置检查（空、NUL、双引号、控制字符）已在 `shell.rs:489-498` 覆盖，不要重复写。

**为什么给 Command 单独 128 字节上限**：它不是文件系统资源，是调用身份。沿用 32 KiB 会让一条 32 KiB 的「命令 id」通过校验后流进日志、错误文案和目录比较。

#### 4.2.2 五处拒绝点（编译器会逼出前三处）

| 位置 | 现状 | K0 动作 |
|---|---|---|
| `actions.rs:190` `allowed_actions(kind)` | `match kind` 全枚举 | 加 `TargetKind::Command => vec![]` → `list_actions`（`actions.rs:177-187`）因 `allowed.is_empty()` 返回 `Err(Unsupported)` |
| `shell.rs:343-349` `execute_run_action` | 拒绝 `Window \| Web` | 改为拒绝 `Window \| Web \| Command` |
| `shell.rs:599-605` `shell_execute` | 只拒绝 `Window` | 加 `Command`（任何 verb 都不得对命令 id 触发 `ShellExecuteExW`） |
| `file_ops.rs:22-32` `validate_file_target` | allowlist `File \| Directory` | **无需改动**——allowlist 天然拒绝。不要「顺手补一句」，那是噪声 |
| `zip.rs:198-206` `validate_zip_request` | allowlist `File \| Directory` | **无需改动**，同上 |
| `shell.rs:681-690` `reveal` | allowlist `File \| Directory \| Application` | **无需改动**，同上 |
| `history.rs:734-741` `is_recordable` | 字符串白名单，不含 `"command"` | **无需改动**，但**必须加测试**锚定：这条是 `history-v2.json` 不被污染的唯一防线（P8） |
| `alias.rs:264-271` `target_kind` | 拒绝未知 kind | **无需改动** |

#### 4.2.3 IPC 层显式守卫（新增，不可省）

`dispatch_non_search`（`ipc.rs:1027`）的 `Execute` / `Reveal` / `Actions` / `RunAction` 四个分支（`ipc.rs:1050-1087`）在 `resolve_target` 之后、进 `run_shell`/`list_actions` 之前加统一守卫：

```rust
// 命令身份只能经 execute_command 调用（K1）。这里显式拒绝，不依赖下游
// Shell 层的类型守卫——新增一个操作时忘记加守卫，不应该等于开一个洞。
if TargetKind::parse(&target.kind) == Some(TargetKind::Command) {
    return Response::Error {
        message: "命令不能通过该请求执行".into(),
        category: Some(ShellErrorKind::Unsupported),
    };
}
```

**为什么在 Shell 层已经拒绝的情况下还要加**：
1. **威胁模型**：管道对同用户任意本地进程开放，攻击者可以直接构造 `{"type":"execute","target":{"kind":"command","value":"prism.exit"}}`，不经过 WPF。
2. **纵深**：Shell 层的拒绝散在 6 个函数里，将来任何新增操作都要记得加。IPC 入口的单点守卫让「忘记」的后果从「开洞」降级为「多一层拒绝」。
3. **可测**：Shell 层拒绝路径要真跑 Win32（见 §8 陷阱 7），IPC 守卫是纯函数路径，能进 CI。

### 4.3 T3 — hello 能力协商与连接级状态

#### 4.3.1 协议形状

```rust
// Request（ipc.rs:74-76）
Hello {
    protocol: u32,
    #[serde(default)]
    capabilities: Vec<String>,
}

// Response（ipc.rs:195-201）
Hello {
    protocol: u32,
    version: String,
    build_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_catalog_generation: Option<u64>,
}
```

**为什么两个新字段都 `skip_serializing_if`**：未协商的连接拿到的 hello 回包**逐字节等于 K0 前**，P2 由构造保证；`PipeClientHandshakeTests` 里 `{"type":"hello","protocol":1}` 的既有断言也不需要动。

**为什么不升 `BROKER_PROTOCOL`（`ipc.rs:35`）**：升版本号意味着新旧混用直接拒绝握手，用户在灰度期会看到「后端未连接」。加法字段 + 能力协商能让三组组合都正常降级。设计 §9.2 的备选方案（整体升 v2）只在 serde/握手状态难以维护时启用——本方案的状态只有一个 `bool`，不构成那种情形。

#### 4.3.2 连接级状态放哪

`Request::Hello` 目前在 `dispatch_non_search`（`ipc.rs:1037-1045`）里处理，而它是**无状态自由函数**，存不住连接能力。

做法：
1. 在 `handle_connection`（`ipc.rs:819`）的循环外声明 `let mut caps = ConnectionCaps::default();`；
2. 在请求 `match`（`ipc.rs:879`）里，于 `Ok(Request::Search{..})` 分支之后、通用 `Ok(req)` 分支之前，插入 `Ok(Request::Hello { protocol, capabilities })` 分支：校验 protocol、**覆盖**写入 `caps`、构造回包（`features` 取 `caps` 与 broker 支持集的交集，`command_catalog_generation` 仅在协商成功时为 `Some`）；
3. `dispatch_non_search` 里的 `Hello` 两个分支改为返回 `Response::Error { message: "hello must be handled by the connection loop", .. }`——**照抄 `ipc.rs:1197` 的 `Request::Search{..}` 不可达分支写法**，仓库已有这个先例；
4. `dispatch_non_search` 增加第 8 个参数 `caps: ConnectionCaps`（`Copy`，一个 bool），供 T6 门控。

```rust
#[derive(Debug, Clone, Copy, Default)]
struct ConnectionCaps { commands_v1: bool }
```

**为什么 `Copy`**：`Search` 分支把工作 spawn 到独立任务（`ipc.rs:911`），`Copy` 让它按值捕获，不需要再包一层 `Arc`。

**重复 hello 的规则**：每次 hello **覆盖**本连接能力。理由：覆盖是 1 行，「首次生效」需要额外标志位；本地进程降级自己的连接能力无危害。protocol 不匹配的分支**不得**写入 caps。

**两条连接各自握手**：`PipeClient` 有 `_query` 与 `_action` 两个 `PipeChannel`（`PipeClient.cs:32-38`），各自 `HandshakeAsync`（`PipeClient.cs:1305`）。broker 侧天然每连接一份 `caps`，无需额外工作；C# 侧必须**按通道**记录（T9）。

---

### 4.4 T4 — `SearchResultKind::Command` 与 `Results` 的两个新字段

#### 4.4.1 `SearchResultKind`（`ipc.rs:296-318`）

**追加到枚举末尾**（`Window` 之后），`wire_str()` 加 `Command => "command"`（`match`，编译器会逼你补）。

**为什么必须是末尾而不是设计示意的「Folder 之后」**：该枚举 `derive(Ord)`，序号直接参与两处最终 tiebreak——`compare_search_results`（`ipc.rs:2999`）与 `sort_search_results_with_picks`（`ipc.rs:3039`）。插在中间虽然保持既有变体的相对顺序不变，但会让命令行在「元数据、标题、副标题全相等」时**赢过** Web/Window；追加到末尾则给命令行最弱的 tiebreak，与设计 §6.2「命令不得越过文件强匹配」一致。K0 无生产者，但 K1 直接依赖这个顺序，现在定对，K1 不用回头改。

#### 4.4.2 `Response::Results` 两个新字段（`ipc.rs:206-241`）

```rust
#[serde(skip_serializing_if = "is_true")]
cacheable: bool,
#[serde(skip_serializing_if = "Option::is_none")]
command_catalog_generation: Option<u64>,
```

在 `ipc.rs:188` 的 `is_false` 旁边加对称的 `fn is_true(value: &bool) -> bool { *value }`。

**为什么 `cacheable` 反向省略（true 时不出现在线路上）**：K0 的每一个响应都是 `cacheable: true`，省略即让 search 回包**逐字节等于 K0 前**（P2）。同时语义自解释：这个字段只在「有限制」时出现。`Status.cancelled` 用 `is_false` 是同一手法（`ipc.rs:247`）。

**构造点**（枚举变体不支持 `..Default`，必须逐处补 `cacheable: true, command_catalog_generation: None`）：
- 生产：`ipc.rs:2128`、`2151`（`window_results`）、`2223`、`2441`（`path_response`）
- 测试：`ipc.rs:3607`、`4908`

`command_catalog_generation` 在 K0 恒为 `None`（无命令行 → 前端无需比对代际）。K1 起在协商连接上回填当前 generation，前端据此决定是否显式拉 `CommandList`。

### 4.5 T5 — `SearchCommandContext`（上线发送，只解析不消费）

#### 4.5.1 broker 侧

```rust
// Request::Search 追加（ipc.rs:80-92）
#[serde(default)]
command_context: Option<SearchCommandContext>,

#[derive(Debug, Clone, Deserialize)]
pub struct SearchCommandContext {
    #[serde(default)] pub current_folder: Option<String>,
    #[serde(default)] pub host_kind: Option<String>,
    #[serde(default)] pub host_capabilities: Vec<String>,
}
```

`sanitize(self, caps: ConnectionCaps) -> Option<Self>`，在进 `search_service` 之前执行：

1. `!caps.commands_v1` → 整体丢弃（返回 `None`）。**单点门控**：非协商连接无权影响命令适用性；
2. `current_folder`：非绝对路径 / 含 NUL、控制字符、双引号 / 超 32 KiB / UNC（`\\` 前缀）→ 置 `None`；
3. `host_kind`：白名单 `none|explorer|system_file_dialog|directory_opus`，未知 → `None`；
4. `host_capabilities`：≤8 项、每项 ≤32 字节，未知值丢弃；
5. K0 消费者为空：`search_service` 收下后 `let _ = command_context;`，注释写明 K1 接入点。

**为什么不复用 `root_scope` 的 root 校验**：`root` 是文件搜索范围，要求路径**在索引中**（`RootRejection::VolumeNotIndexed`）；`current_folder` 只是命令适用性输入，非 NTFS 卷上的目录照样能「在此打开终端」。复用会让命令在非索引卷上莫名失效。设计 §5.1 已明确「它只用于命令适用性，绝不改变文件搜索 root」。

**为什么 UNC 在 K0 就拒**：设计 §5.3 第 5 条——`spawn_blocking` 里的路径校验取消不了卡死的离线 UNC 访问。K0 就把口子关掉，比 K2 再补容易。

#### 4.5.2 C# 侧

- `SearchContext`（`SearchAbstractions.cs:7-29`）追加**末位带默认值**的参数 `CommandSearchContext? CommandContext = null` → 现有全部构造点（含 `HostScopeController.Apply` 的 `with { Root = ... }`）零改动。
- `SearchPayload`（`PipeClient.cs:509-533`）仅在**通道已协商 `commands_v1` 且 `CommandContext` 非 null** 时加 `command_context` 键。其余情况 payload 逐字节等于旧格式。
- **`current_folder` 的取值来源（极易搞错）**：必须取 `HostScopeController.Host.Root`（`HostScopeController.cs:39`，条件 `Host.HasUsableRoot`），**不是** `HostScopeController.Root`（`HostScopeController.cs:50`）——后者在用户按 Ctrl+G 切回全局后返回 `null`，命令上下文会莫名消失。`host_kind` 取 `Host.Kind`。检测失败时 `Host` 已是 `HostContext.Cleared(...)`，`Root` 为 null，天然满足设计「检测失败必须为空，绝不沿用上次目录」。
- `IsEquivalentTo`（`SearchAbstractions.cs:24-28`）**K0 不纳入** `CommandContext`，并在其上加注释登记 D7：K1 上线根搜索命令 lane 时必须纳入。

### 4.6 T6 — `CommandList` / `CommandCatalog`

```rust
// Request（查询通道）
CommandList,

// Response
CommandCatalog { generation: u64, items: Vec<CommandDescriptor> },
```

门控：`dispatch_non_search` 里若 `!caps.commands_v1` → `Response::Error { message: "commands are not negotiated on this connection", category: Some(Unsupported) }`。

**为什么回 Error 而不是空目录**：能力声明不一致时，前端必须进入「命令功能整体关闭」的降级态（设计 §9.2 末行），而不是以为「目录恰好是空的」。空目录会让前端把一个协商故障误解成一个正常状态。

`CommandDescriptor` 按设计 §5.1 形状，**Serialize-only**，与持久化用的 `UserCommandDefinition`（Deserialize）是**两个类型**。

**为什么必须拆成两个类型**：这是「导入 JSON 无法获得内置特权 handler」（设计 §10.2 第 6 条）的**结构性**保证。共用一个类型的话，`owner: "broker"` + 某个内置 handler id 就是一次成功的提权导入，只能靠运行时校验拦——运行时校验会被将来的重构绕过，类型不会。

K0 内置表（D5）：

```jsonc
{
  "id": "prism.settings.open", "title": "打开设置", "subtitle": "",
  "icon_glyph": "", "owner": "ui", "trust": "builtin",
  "keywords": [], "input": { "kind": "none", "required": false, "prompt": "" },
  "bindings": {},                       // K0 全部 binding 缺省 = 不可达
  "danger": "normal", "enabled": true
}
```

`bindings` 内各 surface 用 `Option` + `skip_serializing_if`，缺省即「该 surface 关闭」。**为什么 K0 全 null**：K0 既没有 Router 也没有执行器，声明 binding 只会造成「协议说可以、代码不认」的假象。K1 填 `root_search` 与 `keyword`。

### 4.7 T7 — `CommandInvocationContext`（类型 + 校验，不执行）

`commands.rs` 内 `pub` 定义（`pub` 项在 lib crate 中不触发 `dead_code`，无需 `#[allow]`）：

```rust
pub struct CommandInvocationContext {
    pub command_id: String,
    pub source: InvocationSource,          // root|keyword|action_panel|staging|shortcut
    pub arguments: CommandArguments,
    pub selection: Option<CommandSelection>,
    pub staged_paths: Vec<String>,
    pub current_folder: Option<String>,
    pub host_kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]              // 只加在这一层
pub struct CommandArguments {
    pub text: Option<String>,              // ≤8 KiB
    pub destination: Option<String>,
    pub output_path: Option<String>,
}
```

`validate()` 覆盖设计 §5.3 的全部上限：command_id ≤128 字节 + 语法、staged ≤128 项、单路径 ≤32 KiB 且绝对且非 UNC、text ≤8 KiB、`selection.title/subtitle` 有界、`approximate_json_len()` >512 KiB 拒绝。

**`deny_unknown_fields` 只加在 `CommandArguments`**：加在外层结构上，新版 WPF 增加一个字段就会被旧 broker 整体拒绝（K1→K2 之间必然发生）。参数结构则必须严格——设计 §5.3 第 1 条要求「未知/多余字段拒绝」，防的是「把 ZIP 输出路径塞进自由文本」这类越权。

**K0/K1 分界（写清楚，否则会被当成已完成）**：K0 交付 `validate()` + 全部边界单测；**512 KiB 的线路级检查**（在读到行之后、parse 之前按行字节数拒绝，与 `MAX_REQUEST_LINE_BYTES = 1 MiB`（`ipc.rs:711`）叠加）随 K1 的 `ExecuteCommand` 请求一起落地。

### 4.8 T8 — handler registry 骨架与 owner 分工

```rust
// commands.rs
pub enum BrokerHandlerId { OpenTerminalHere, SystemLock }   // K1 起使用

const BROKER_HANDLERS: &[(&str, BrokerHandlerId)] = &[];    // K0 空表

pub fn broker_handler(id: &str) -> Option<BrokerHandlerId> {
    BROKER_HANDLERS.iter().find(|(key, _)| *key == id).map(|(_, h)| *h)
}
```

用**静态表 + `find`** 而不是 `match id { _ => None }`：后者会触发 clippy `match_single_binding`（D1 的基线对比会因此变红）。

**owner 分工必须写死并遵守**（最容易做错的一处）：

- broker 只校验 **broker-owned** 命令有对应 handler；无对应者不进 `catalog()`；
- **UI-owned 命令的 handler 存在性由 WPF 判定**，broker 不知道 WPF 注册了什么，因此 `catalog()` 必须原样下发 UI-owned 描述；
- WPF 侧丢弃自己不认识的 UI-owned id（设计 §10.1「仅已注册 UI handler；未知 id 拒绝」）。

搞反的两种典型后果：broker 顺手过滤 UI-owned → 内置的 `prism.settings.open` 被静默吞掉，兼容矩阵测不出问题只是「目录是空的」；WPF 不过滤 → 新 broker 下发的未知 UI 命令在旧前端上被当成可执行项。

---

### 4.9 T9 — 前端 DTO、`PipeClient` 能力捕获、目录服务

#### 4.9.1 新文件 `src/Prism/Models/CommandDescriptor.cs`

`record` + **容忍解析**：未知 `owner` / `trust` / `danger` / `input.kind` / binding 的 `input` 取值一律**不抛异常**，保留原始字符串并置 `IsUsable = false`。

**为什么容忍而不严格**：新 broker + 旧 WPF 是常态组合（安装包分别升级）。严格解析会让一个新增的 binding 类型把整份目录解析打死，等于让 broker 的新功能反向击穿旧前端。类型安全在这里的正确形态是「不认识 → 隐藏」，不是「不认识 → 抛」。

#### 4.9.2 `PipeClient` 增量

| 改动 | 落点 | 要点 |
|---|---|---|
| hello 发 capabilities | `PipeClient.cs:1307` | `new { type = "hello", protocol = ProtocolVersion, capabilities = new[] { "commands_v1" } }`，受 `internal static bool AdvertiseCommandCapability { get; set; } = true;` 控制 |
| hello 解析 features | `PipeClient.cs:1339-1348` | 在既有 type/protocol 校验之后读 `features` 数组、`command_catalog_generation`、**`build_id`**（目前被忽略），存到 `PipeChannel` 的属性上 |
| 按通道暴露能力 | `PipeChannel` | `public bool HasCommands { get; private set; }`、`public string? BuildId`、`public ulong? CommandCatalogGeneration`；断开时清空（与 `ServerProcessId` 同处理，`PipeClient.cs:1166`） |
| 客户端总开关 | `PipeClient` | `public bool CommandsAvailable => _query.HasCommands;` |
| `CommandListAsync()` | 查询通道 | `QueryReadTimeout`；返回 `(generation, descriptors)`；收到 `type=error` 时**不抛致命异常**，返回「不可用」 |

**为什么用 `internal static` 开关而不是 `Settings` 字段**：设计 P7 明确 K0 不往 `settings.json` 加命令字段（旧 WPF 全量保存会丢弃未知字段，`config.rs:23-54` 是共享 schema）。`internal static` 既是测试注入点也是紧急关闭手段，`ActionReadTimeout`（`PipeClient.cs:890`）已是同款先例。

**必须写进代码注释的 K1 陷阱**：`SendActionAsync`（`PipeClient.cs:900-914`）在动作通道未连接时会**回退到查询通道**。K1 的 `ExecuteCommand` 因此可能落在任一通道上，能力/PID/build_id/generation 的一致性检查（设计 §9.2 末段、§14 R13）必须针对**实际承载的通道**做。K0 的交付是让两个通道都能各自回答 `HasCommands`，把这个检查在 K1 变成可行的。

#### 4.9.3 新文件 `src/Prism/Services/CommandCatalog.cs`

- 快照缓存 + `RefreshAsync()`；K0 的调用时机**只有一处**：握手成功后（`ConnectionChanged` → true）；
- 过滤 `owner=="ui"` 且不在 UI handler 注册表内的项（T8）；
- 过滤 `IsUsable == false` 的项；
- 任何 `command_*` 请求收到 error / 抛异常 → 标记整体不可用、快照清空、**不抛给调用方**；
- 记录 `Generation`。

**为什么 K0 不做轮询/防抖，且绝不复用 `_generationDebounce`**：`SearchViewModel.cs:21,143` 的 `_generationDebounce` 与 `IndexerGenerationClient` 服务的是**索引**代际（直连 indexer，`ipc.rs` 之外的通道）。复用会把命令目录刷新绑到索引轮询节奏上，并让两套 generation 语义在同一段代码里混淆。设计 §2.1 明确要求独立命名与独立状态。命令目录的唯一写者是本进程（单实例强制，`SingleInstanceTests`），K0 又没有写者，握手后拉一次足够。

#### 4.9.4 UI handler 注册表骨架

`src/Prism/Services/CommandHandlers.cs`：K0 只需 `private static readonly HashSet<string> Known = new(StringComparer.Ordinal) { "prism.settings.open" };` + `public static bool IsKnown(string id)`。

**为什么不先摆好委托字典**：K0 不执行任何命令，委托字典里只会有 null 或抛异常的占位项——那是脚手架，不是地基（P1/P4）。K1 加执行时再把 `HashSet` 换成 `Dictionary<string, Func<...>>`，改动量比现在预留一个空壳更小。

### 4.10 T10 — 前端命令行安全化与 `cacheable` 接入

K0 没有命令行生产者，但这些改动**必须与类型同批落地**：K1 一旦打开生产者，缺任何一条就是「命令 id 被当路径执行」。这是「修根因而非症状」——在每个入口各一道守卫，而不是只堵 K1 恰好走的那条路。

| # | 改动 | 落点 | 为什么 |
|---|---|---|---|
| 1 | `SearchResultKind` 加 `Command`；`Kind switch` 加 `"command" => SearchResultKind.Command` | `SearchResult.cs:3-13, 67-76` | 否则命令行落进 `Unknown` |
| 2 | **`ActionTarget.FromLegacy` 加 `"command" => "command"`** | `SearchResult.cs:19-31` | **K0 最高价值的一行**。现状 `_ => "file"`：typed target 缺失的命令行会退化成**以命令 id 为路径的 file target**，然后走 `_pipe.ExecuteAsync` → broker Shell 链路，只剩「绝对路径」检查兜底。映射成 `command` 后，broker 每条路径都显式拒绝（T2） |
| 3 | `ParseResult` 为命令行设 `RowKey = "command:" + target.Value` | `PipeClient.cs:916-957` | 默认 `ContainerKey`（`SearchResult.cs:61`）含 Title；描述变更会让容器删旧插新，重演网页行的图标闪烁缺陷（`WebRowIdentityTests` 的成因）。命令 id 是唯一稳定身份 |
| 4 | `ExecuteSelectedCoreAsync` 在通用 `_pipe.ExecuteAsync` 之前截获命令行 | `SearchViewModel.cs:489-524`（插在 `item.Kind == "window"` 分支之后、`try` 之前） | 设计 §6.5 首条。K0 分支体是 `_state.StatusMessage = "命令不可用"; return;`，K1 替换为按 owner 分派 |
| 5 | `RevealSelectedAsync` / Ctrl+Enter 对命令行直接禁用 | `SearchViewModel` 对应方法 | 命令没有「所在文件夹」；不禁用则命令 id 被送进 reveal 的 `explorer /select` 路径 |
| 6 | 进入动作面板的入口拒绝命令行 | `SearchViewModel.cs:605-624`（`_state.Mode = PanelMode.Actions` 处） | `Actions` 请求携带命令 target 会拿到 broker 的 Unsupported 错误，UI 上是一次无意义的失败提示 |
| 7 | `SearchResponse` 追加 `bool Cacheable = true` 与 `ulong? CommandCatalogGeneration = null`（末位默认值） | `PipeClient.cs:1560-1571` | 末位带默认值 → 全部现有构造点（含测试）零改动 |
| 8 | `ParseSearchResponse` 读 `cacheable`（**字段缺失 → true**）与 `command_catalog_generation` | `PipeClient.cs:535-567` | 与 broker 的反向省略约定配对（T4） |
| 9 | 缓存准入门加 `&& resp.Cacheable` | `SearchViewModel.cs:1169-1196` 的 `if` 条件内 | 先补消费者、再上生产者。K1 让含命令行的响应回 `false`，那时消费者已就位 |
| 10 | `TryFilterCompleteCache` 上补 D7 注释 | `SearchViewModel.cs:1497-1523` | 登记 K1 必做项：`IsEquivalentTo` 必须纳入 `CommandContext`。顺带注意 `alias.rs` 那条别名守卫（`SearchViewModel.cs:1508-1516`）是 K1 命令关键字守卫的现成范式 |

---

## 5. 测试清单

原则：每条都必须在契约被破坏时**真的失败**。凡是「需要真实 Shell / 真实索引 / 真实窗口」才能跑的断言，都换成等价的纯函数或 IPC 层断言（§8 陷阱 7）。

### 5.1 Rust（`commands.rs` 内 `mod tests`；协议部分进 `ipc.rs` 的 `protocol_tests`，`ipc.rs:3620`）

| # | 测试 | 锚定的契约 |
|---|---|---|
| R1 | `set` → `persist` → `load` 往返相等 | 存储正确性 |
| R2 | `{"schema_version":999,...}` → `into_compatible()` 为 `Err` | 未来版本不被当前代码误读 |
| R3 | 损坏文件 → `load` 产出 `commands-v1.corrupt-*.json` 留档，且盘上回到合法 JSON | 照抄 `alias.rs:347` 的 `corrupt_store_isolated_not_silently_overwritten`。缺这条 = 下一次 mutation 用空内存态覆盖用户数据 |
| R4 | 边界：id 语法/128 字节、title 64 字符（用中文串）、subtitle 128 字符、keywords 4 个/16 字符/含空白拒绝、entries 512 上限 | 全部上限「保存前校验」 |
| R5 | generation：`load` 后 ≥1；连续 mutation 严格递增 | 0 保留给「未知」；K1 的代际比对依赖单调 |
| R6 | 并发 mutation 不撕裂：`std::thread::scope` 起 N 线程混合 `set`/`delete`，结束后文件可解析且条数符合预期 | 设计 K0 验收项。`alias.rs` 有 `persist_lock` 但**没有**对应测试，这里补上 |
| R7 | `ActionTarget{kind:"command"}.validate()`：合法 id 通过；129 字节、大写、含 `/`、含空白、`../` 拒绝 | T2 校验 |
| R8 | 命令 target 被逐一拒绝：`actions::list_actions` → `Err(Unsupported)`；`file_ops::validate_file_target`；`zip::validate_zip_request`；`history::is_recordable(&cmd) == false` | 五处拒绝点。**`is_recordable` 那条是 `history-v2.json` 不被污染的唯一防线** |
| R9 | IPC 守卫：对 `Execute`/`Reveal`/`Actions`/`RunAction` 四种请求携带命令 target，各回 `Error{category:Unsupported}` | T2.3 的纵深守卫，且这是可进 CI 的等价断言 |
| R10 | hello：带 `capabilities:["commands_v1"]` → 回包 `features` 含之、`command_catalog_generation` 为 `Some`；**不带** → 回包 JSON **不存在** `features` 与 `command_catalog_generation` 两个 key（`serde_json::Value` 断言 key 缺失） | P2 的字节级锚点 |
| R11 | hello protocol 不匹配 → 回 Error 且**不**写入 caps（随后 `CommandList` 仍被拒） | 降级不得半开 |
| R12 | `CommandList`：未协商 → `Error`；已协商 → `CommandCatalog{generation, items}` 且 items 含 `prism.settings.open` | T6 门控与 D5 数据 |
| R13 | `Results` 序列化：`cacheable=true` 时 JSON **无** `cacheable` key；`false` 时有且为 `false` | T4 反向省略约定 |
| R14 | **命令无生产者**：对一组查询（含内置命令标题「设置」「打开设置」）调 `search_service`，断言 items 中无 `kind=="command"` | K0 零行为变化的实质断言 |
| R15 | `SearchCommandContext::sanitize`：未协商连接整体丢弃；相对路径 / UNC / 超长 / 含引号的 current_folder 置 None；未知 host_kind 置 None；host_capabilities 超 8 项截断 | T5 校验 |
| R16 | `CommandInvocationContext::validate`：staged 129 项拒绝；单路径 UNC 拒绝；text 8 KiB+1 拒绝；`CommandArguments` 含未知字段拒绝；聚合估算 >512 KiB 拒绝 | T7 上限 |
| R17 | `ClearHistory` 同时清 usage：预置 usage 文件 → 请求后两份都清空；usage 清理失败 → 回 `Error` 而非 `Status` | §4.1.3 隐私联动 |

### 5.2 C#（新增 `CommandCatalogTests.cs` / `CommandTargetSafetyTests.cs`，并扩 `PipeClientHandshakeTests.cs`）

| # | 测试 | 锚定的契约 |
|---|---|---|
| C1 | 旧 broker 形状的 hello（`{"type":"hello","protocol":1}`）仍握手成功 → `CommandsAvailable == false` | 回归锚：`PipeClientHandshakeTests` 现有断言必须继续通过 |
| C2 | 含 `features:["commands_v1"]` + `command_catalog_generation:3` 的 hello → `CommandsAvailable == true`，generation/build_id 被捕获 | T9 解析 |
| C3 | `SearchPayload` 未协商时的 JSON **字符串**等于旧格式；协商且有 current_folder 时含 `command_context` | P2 字节级锚点（C# 侧） |
| C4 | `ParseSearchResponse`：无 `cacheable` → `true`；`false` → `false`；`command_catalog_generation` 解析正确 | T10.8 |
| C5 | **`ActionTarget.FromLegacy("command", "prism.settings.open").Kind == "command"`**（断言不是 `"file"`） | T10.2，K0 最关键的单行断言 |
| C6 | `ParseResult` 命令行 → `RowKey == "command:prism.settings.open"` | T10.3 |
| C7 | `CommandDescriptor` 容忍解析：未知 owner/danger/binding input → `IsUsable == false` 且不抛 | T9.1 |
| C8 | `CommandCatalog` 降级：fake client 对 `command_list` 返回 error → 服务标记不可用、快照空、不抛 | T9.3 |
| C9 | UI-owned 且不在 `CommandHandlers.Known` 的 id 被过滤出快照 | T8 owner 分工的 WPF 半边 |
| C10 | 伪造 `kind=command` 行 + Enter → fake client 的 `ExecuteAsync` **调用次数为 0**；Ctrl+Enter → `RevealAsync` 调用次数为 0 | T10.4/10.5。用 `SearchViewModelTests` 现有 fake client 的计数手法 |
| C11 | `Cacheable=false` 的响应不进 `_completeCache`：随后同前缀输入触发真实请求（沿用 `SearchViewModelTests` 里既有的「Fake client 调用计数」断言手法） | T10.9 |

---

## 6. 验收门

### 6.1 命令（全部必须绿）

```bash
cargo fmt   --manifest-path src/prism-core/Cargo.toml -- --check
cargo test  --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets
dotnet test  src/Prism.Tests/Prism.Tests.csproj
dotnet build src/Prism/Prism.csproj -c Release
```

clippy 判定按 **D1**：与基线对比不新增 warning。基线 = 2 条（`src/ntfs.rs:130`、`src/zip.rs:160`，均为 `chunks_exact_to_as_chunks`），2026-08-26 实测 exit 0。

`cargo fmt --check` 是唯一有 hook 强制的门（`scripts/hooks/pre-commit`），提交前必过，否则被 hook 拦下。

### 6.2 兼容矩阵（三组，逐组给出可执行的验证方式）

| 组合 | 验证方式 | 通过标准 |
|---|---|---|
| **旧 WPF + 新 broker** | 扩 `scripts/pipe-roundtrip-test.ps1`：发**不带** `capabilities` 的 hello，再发一次 search | 回包无 `features`/`command_catalog_generation` key；search 回包无 `cacheable`/`command_catalog_generation` key、无 `kind:"command"`；`command_list` 回 `type:"error"` |
| **新 WPF + 旧 broker** | 把 `AdvertiseCommandCapability` 保持 true，用 fake channel（回旧形状 hello）跑 C1；条件允许时用 `git stash` 前的 broker 二进制实跑一次 | `CommandsAvailable == false`；不发出任何 `command_list`；搜索/动作/网页/窗口/别名/暂存区行为不变 |
| **新 + 新** | 扩同一 ps1：发带 `capabilities` 的 hello + `command_list` | 回包含 `features:["commands_v1"]`；`command_catalog` 含 1 条内置命令、`generation >= 1` |

外加一次**人工回归**（K0 无自动化 UI 测试覆盖这些路径）：文件搜索、`ext:`/`path:` 过滤、`>` 窗口模式、网页关键字 + 联想、裸 URL、别名精确置顶、动作面板（含重命名/目录选择/永久删除）、暂存区与工作集召回。K0 全是加法，任何一项异常都说明触碰了不该碰的地方。

### 6.3 性能门

K0 不新增每击键路径上的工作（目录只在握手后拉一次），P95 搜索延迟与三进程内存应与基线**无可测差异**。若出现回归，最可能的原因是把 `CommandList` 接到了搜索路径或复用了 `_generationDebounce`（§8 陷阱 10）。

---

## 7. 回滚合同

K0 全部为加法且落在新文件/新字段上，回滚 = `git revert` 对应提交。数据面零损失，逐项说明：

| 文件 | K0 是否改动 | 回滚后的表现 |
|---|---|---|
| `commands-v1.json` / `command-usage-v1.json` | 新增 | 旧 broker 不读这两个文件 → 被忽略；再升级回来数据仍在 |
| `history-v2.json` | **不动** | 无影响。这是刻意的：kind 校验封闭（`persistence.rs:98-103`），写入 `"command"` 会让旧 broker 判整个文件损坏并隔离重置，表现为**用户全部使用历史被清空** |
| `settings.json` | **不动** | 无影响。旧 WPF 全量保存会丢弃未知字段（`config.rs:23-54` 是共享 schema），命令配置放进去会在一次旧版本保存后消失 |
| `aliases-v1.json` / `staging.json` | 不动 | 无影响 |
| `dist/prism.iss` | **不动**（D2） | 无影响 |

协议面：新旧 broker/WPF 任意组合都靠 hello 能力协商降级，不需要同步升级，也不需要卸载重装。

**这是 K0 最重要的交付物**：让后续 K1–K3 每一步都能单独回滚，而不必回退搜索/动作基础设施。

---

## 8. 陷阱清单（施工前通读，每条都对应一个已知会踩的坑）

1. **把 `Command` 插进 `SearchResultKind` 中间**。该枚举 `derive(Ord)` 且参与 `ipc.rs:2999`、`3039` 的最终 tiebreak。→ 追加到末尾（`Window` 之后）。
2. **`ActionTarget.FromLegacy` 漏掉 `"command"`**（`SearchResult.cs:19-31`）。`_ => "file"` 会把命令 id 变成文件路径送进 Shell 链路。→ 加映射 + C5 断言。
3. **想在 `dispatch_non_search` 里处理 hello 并保存连接能力**。它是无状态自由函数。→ hello 挪到 `handle_connection` 的请求 match 里（写法照抄 `ipc.rs:1197` 的不可达分支先例），能力以第 8 个参数传给 `dispatch_non_search`。
4. **`Results.cacheable` 无条件序列化**。会破坏「未协商连接逐字节一致」。→ `skip_serializing_if = "is_true"`。
5. **用 `history-v2.json` 存命令使用记录**。→ 见 §7 表；必须用独立 `command-usage-v1.json`。
6. **给 `TargetKind::Command` 沿用 `MAX_PATH_BYTES`（32 KiB）**。→ 128 字节，`shell.rs:503-507` 改成 `match`。
7. **给 `shell_execute` 的 Command 拒绝写单测**。它会真的调 `ShellExecuteExW`，测出来的是环境不是契约。→ 测 IPC 层守卫（R9）与各 `validate_*` 纯函数（R8）。
8. **`current_folder` 取 `HostScopeController.Root`**（`HostScopeController.cs:50`）。用户 Ctrl+G 切回全局后它是 `null`，命令上下文会莫名消失。→ 取 `Host.Root`（条件 `Host.HasUsableRoot`）。
9. **K0 顺手接上 `ExecuteCommand` 或目录 mutation 的 IPC 端点**。管道对同用户任意本地进程开放，没有消费者的执行面是纯攻击面。→ P4/D3。
10. **复用 `_generationDebounce` / `IndexerGenerationClient` 做命令目录 generation**（`SearchViewModel.cs:21,143`）。两套 generation 语义会混淆，且把目录刷新绑到索引轮询节奏上。→ 独立状态，握手后拉一次。
11. **复用 `alias::normalized_words` 与 `ALIAS_*` 常量做 keywords 归一**。上限语义不同（32 字符/8 个 vs 16 字符/4 个），一侧调参会波及另一侧路由。→ 独立常量与独立归一函数。
12. **`#[serde(deny_unknown_fields)]` 加在整个 `CommandInvocationContext` 上**。新 WPF 加字段后旧 broker 全拒。→ 只加在 `CommandArguments`。
13. **`CommandDescriptor` 与 `UserCommandDefinition` 合并成一个类型**。会让「导入 JSON 指定内置 handler」从结构上不可能退化为靠运行时校验拦。→ 两个类型，用户侧无 `owner` 字段。
14. **broker 顺手过滤掉 UI-owned 命令**（因为查不到 handler）。会把唯一的内置命令静默吞掉，且兼容矩阵只表现为「目录是空的」。→ owner 分工见 T8。
15. **`title`/`subtitle` 用字节上限**。中文标题会提前触顶。→ `chars().count()`；路径类字段仍用字节。
16. **`mutation` 时先改内存态再校验**。`alias.rs:105-121` 记录了这个坑：内存改成功、落盘失败，此后每次 mutation 都卡在同一条坏数据上直到重启。→ 校验前置。

---

## 9. K0 明确不做 / 移交 K1 的待办

**K0 不做**（做了就是超范围，会把 K0 的回归面扩大）：

- 任何命令的执行（`ExecuteCommand`、UI/broker handler 实体、`RecordCommandOutcome`）；
- `QueryRouter`、命令关键字路由、`PanelMode.CommandInput` 参数态；
- 根搜索命令 lane 与排序合并；
- `ActionComposer`、`ActionItem.invocation_kind`、命令快捷键；
- 暂存区 surface 与批量 handler；
- 用户命令设置页、`ValidateTriggerNamespace`、网页引擎目录适配；
- `{clipboard}`、模板占位符与展开器；
- `history-v2.json` / `ActionId` / `WebModeDetector` / `settings.json` / `dist/prism.iss` 的任何改动。

**移交 K1 的待办**（K0 应在代码注释里逐条登记，不要只留在本文档）：

| # | 待办 | 登记位置 |
|---|---|---|
| K1-1 | `SearchContext.IsEquivalentTo` 必须纳入 `CommandContext`（D7），否则缓存跨 current_folder 复用 | `SearchAbstractions.cs:24` + `SearchViewModel.cs:1497` |
| K1-2 | `ExecuteCommand` 的能力/PID/build_id/generation 一致性检查必须针对**实际承载通道**（动作通道会回退到查询通道） | `PipeClient.cs:900-914` |
| K1-3 | `CommandInvocationContext` 的 512 KiB **线路级**检查（与 `MAX_REQUEST_LINE_BYTES` 叠加） | `commands.rs` 的 `validate()` 附近 |
| K1-4 | 内置目录填 `root_search`/`keyword` binding；`broker_handler` 静态表填 `OpenTerminalHere`/`SystemLock` | `commands.rs` 的 `BROKER_HANDLERS` |
| K1-5 | `search_service` 消费 `command_context` 的接入点 | `search_service` 内 `let _ = command_context;` 处 |
| K1-6 | 含命令行的响应回 `cacheable=false`；命令关键字守卫照 `SearchViewModel.cs:1508-1516` 的别名守卫范式写 | 两处 |
| K1-7 | `CommandHandlers` 从 `HashSet` 升为委托字典 | `CommandHandlers.cs` |
| K1-8 | UI handler 成功后发配对的 `RecordCommandOutcome`；`prism.exit` 默认 `record_usage=false` | `CommandHandlers.cs` |

---

## 10. 代码锚点速查（2026-08-26 工作树）

**broker（Rust）**

| 锚点 | 位置 |
|---|---|
| 协议版本 / `Request` / `Response` | `ipc.rs:35` / `71-171` / `192-277` |
| `SearchResultKind` + `wire_str` / `SearchResult` / `ActionItem` | `ipc.rs:296-318` / `320-336` / `339-348` |
| `is_false` 辅助（`is_true` 加在旁边） | `ipc.rs:188` |
| `BrokerShared` / `serve` / accept→handle 解构 | `ipc.rs:375-389` / `506-514` / `636-655` |
| `handle_connection` + 请求 match / 保序 writer | `ipc.rs:819-976`（match 在 `879-968`）/ `994-1023` |
| `dispatch_non_search` 签名 / Hello 分支 / 四个动作分支 / 不可达分支先例 | `ipc.rs:1027-1035` / `1037-1045` / `1050-1087` / `1197` |
| `ClearHistory` 分支 | `ipc.rs:1114-1134` |
| `Results` 构造点 | `ipc.rs:2128` / `2151` / `2223` / `2441`（测试 `3607` / `4908`） |
| 排序与 tiebreak | `ipc.rs:2994-3000` / `3019-3049` |
| `list_actions` 包装 / `resolve_target` / `protocol_tests` | `ipc.rs:3054-3062` / `3124-3141` / `3620+` |
| 入站行上限 1 MiB / 握手超时 10 s | `ipc.rs:711` / `792` |
| `ActionTarget` / `TargetKind` / `validate` / `as_str`+`parse` | `shell.rs:12-16` / `18-25` / `474-533` / `536-556` |
| `execute_run_action` 守卫 / `shell_execute` 守卫 / `reveal` allowlist | `shell.rs:343-349` / `599-605` / `681-690` |
| `allowed_actions` / `list_actions` | `actions.rs:190+` / `177-187` |
| `validate_file_target` / `validate_zip_request` | `file_ops.rs:22-32` / `zip.rs:198-206` |
| `is_recordable` / `clear` / `isolate` | `history.rs:734-741` / `275` / `801` |
| `VersionedData` + `VersionedEnvelope` / HistoryData kind 封闭校验 | `persistence.rs:10-43` / `98-103` |
| `AliasStore`（存储家规范本）：结构 / load / persist / 隔离测试 | `alias.rs:39-47` / `53-101` / `213-243` / `347+` |
| `atomic_replace` / `data_dir` 探测 / `serve` 调用点 / 模块表 | `fs_util.rs:19` / `config.rs:98` / `main.rs:89-98` / `lib.rs:3-25` |

**前端（C#）**

| 锚点 | 位置 |
|---|---|
| `ProtocolVersion` / 双通道字段 / 动作通道回退 | `PipeClient.cs:26` / `32-38` / `900-914` |
| `SearchPayload` / `ParseSearchResponse` / `ParseResult` / `TargetPayload` | `PipeClient.cs:509-533` / `535-567` / `916-957` / `846` |
| `PipeChannel`：握手 / 协议校验 / `ServerProcessId` | `PipeClient.cs:1305-1357` / `1341-1348` / `1166` |
| 超时注入点（`internal static` 先例）/ `SearchResponse` | `PipeClient.cs:879, 890` / `1560-1571` |
| `SearchResultKind` / `ActionTarget.FromLegacy` / `ContainerKey` / `ExecutionTarget` | `SearchResult.cs:3-13` / `19-31` / `61` / `65` |
| `SearchContext` + `IsEquivalentTo` / `PanelMode` | `SearchAbstractions.cs:7-29` / `AppState.cs:7` |
| `HostContext` / `HostScopeController.Host` / `.Root` | `HostContext.cs:67-84` / `HostScopeController.cs:39` / `50` |
| `_completeCache` / `SearchCacheEntry` / `ExecuteSelectedCoreAsync` | `SearchViewModel.cs:41` / `106` / `489-524` |
| 动作面板入口 / 缓存准入门 / `TryFilterCompleteCache` + 别名守卫 | `SearchViewModel.cs:605-624` / `1169-1196` / `1497-1523`（守卫 `1508-1516`） |
| 索引代际防抖（**不要复用**） | `SearchViewModel.cs:21, 143`；`IndexerGenerationClient.cs` |

**工程**

| 锚点 | 位置 |
|---|---|
| pre-commit（仅 `cargo fmt`） / 管道往返夹具 / 安装包卸载策略 | `scripts/hooks/pre-commit` / `scripts/pipe-roundtrip-test.ps1` / `dist/prism.iss:124` |
| 测试工程（xunit 2.9.2、net8.0-windows） | `src/Prism.Tests/Prism.Tests.csproj` |







