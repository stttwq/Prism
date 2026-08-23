# Prism 统一命令系统实施方案（Implementation Plan）

> **文档性质**：实施方案，承接并细化《PRISM-COMMAND-SYSTEM-DESIGN-2026-08-22.md》（v2 设计）。设计文档定"做什么/为什么"，本文定"怎么做/按什么顺序/怎么保证不伤现有功能"。
> 日期：2026-08-23。
> 输入：① 设计文档 v2；② 2026-08-23 全量代码实测（broker 27 个 Rust 模块 + 前端 WPF 全链路，含测试布局，行号以当日工作树为准）；③ 联网复核（PowerToys Command Palette 扩展模型、Raycast 排序/参数/占位符规范，见 §12）。
> **与设计文档冲突处，以本文为准**——勘误清单见 §1。

---

## 1. 设计文档勘误与关键决策修正（代码实测结果）

设计文档 v2 写于对代码的快速实测，本次全量复核发现 6 处需要修正的事实假设，每处都直接改变实施方案：

| # | 设计文档假设 | 代码事实（实测） | 对方案的影响与决策 |
|---|---|---|---|
| E1 | "目录快照：握手后下发，**CommandChanged 推送失效**"（§7.2） | 协议是**严格行配对的请求/响应**，无请求 id、无推送通道；`ordered_writer` 按序缓冲正因为不能乱序插入推送（ipc.rs:941-972） | 推送不可行。改为**拉取式快照**：连接建立后、每次 command_set/delete 后、搜索窗口每次显示时拉取 `command_list`。单实例已强制（SingleInstanceTests），目录的唯一写者就是本进程，拉取式失效足够 |
| E2 | history "v2→按需 v3，kind 增加 command"（§7.1） | history-v2.json 的 `kind` 是**封闭校验**（仅 file/directory/application/window，persistence.rs:98-103）；校验失败 → 整个文件被判 corrupt → **隔离重置**（history.rs:763-796）。若把 `"command"` 写进 v2 文件，回滚到旧 broker 会**丢全部历史** | **history-v2.json 完全不动**。命令的 frecency（execute_count/last_used/frecency_milli，复用同款 14 天半衰期公式）存在 commands-v1.json 的条目内。设计文档 Q2 风险就此消解，回滚零损失 |
| E3 | 引擎是"broker 目录（收编后）"，暗示独立目录文件 | 引擎是 **settings.json 的 `WebEngines` 字段**（config.rs:44-48），前端持本地快照并经 `ReloadEngines` 推给 broker——数据源本来就是单点，重复的只是**匹配规则**（WebModeDetector.cs ↔ websearch::try_match） | K2 引擎收编降级为"命名空间统一 + 漂移锚测试"，完整退役移入 K4 候选（见 AD7，§6.4） |
| E4 | `{clipboard}` "broker 即读即弃"（暗示已有读能力） | broker 剪贴板**只写不读**（actions.rs 只有 clipboard_set_text/set_files，全仓无 GetClipboardData） | `{clipboard}` 需在 broker STA 线程新增读函数（K2，默认关） |
| E5 | shell 层面可直接支撑 open_terminal / launch / system | shell.rs 现只有 Open/Reveal/Properties/OpenWith/RunAction 五种操作，**无带参启动、无终端、无电源操作**；RunAsAdmin 走 `ShellExecuteExW verb="runas"` 但不带参数 | 新增 `ShellOperation::LaunchProcess`（path/args/workdir/admin）与 `SystemOp`（lock/sleep/shutdown/reboot/empty_bin），复用 STA 队列与超时预算 |
| E6 | 验收门含 "clippy -D warnings" | 仓库无 clippy 门禁；pre-commit 只有 `cargo fmt -- -l --check`（scripts/hooks/pre-commit:43-76），ipc.rs 已有两处 `too_many_arguments` 豁免 | 验收门 = `cargo fmt --check` + `cargo test` + `dotnet test`；clippy 作为建议不作为门。commands 子系统**沿用 AliasStore 同款六跳穿参**（lib.rs → main.rs → serve → BrokerShared → handle_connection → dispatch），不做顺带重构——重构 ipc.rs 扩大冲突面，违背本方案首要目标 |

另有两处小勘误：`search_service` 实际在 ipc.rs:1711-1997（设计文档写 1697）；前端路由中**裸 URL 检测在引擎关键字之前**（RunSearchAsync 先 884-893 后 898-906），设计文档 §4.2 顺序有误，命令关键字分支应插在引擎关键字检测**之后**（保证现有行为逐字节不变）。

---

## 2. 架构决策记录（AD）

| # | 决策 | 理由 |
|---|---|---|
| AD1 | **命令行混排默认关**（`CommandSuggestionsEnabled` 默认 false，设置页可开） | 一石三鸟：Q1 排序观感风险灰度放量；旧前端二进制 + 新 broker 默认看不到 command 行（未知 kind 虽被容忍映射为 Unknown，但默认关则根本不产生行）；新功能静默上线不扰动存量用户 |
| AD2 | **命令 frecency 存 commands-v1.json，history-v2.json 不动** | E2；回滚安全优先于存储纯粹性 |
| AD3 | **命令热键走 `CommandHotkeys` 独立设置键**（StagingAddHotkey 模式），不进 `ActionHotkeys` | ActionHotkeys 是 broker ActionId 封闭枚举的前端静态镜像（15 项 + 防漂移锚测试）；命令是动态集合，塞进去会破坏锚测试语义。独立字典 + 双侧校验是既有先例（Settings.cs:100-104 注释明示此分层） |
| AD4 | **命令执行走新请求 `execute_command`**，不复用 `run_action` | run_action 的 action 字段经 ActionId 封闭解析（actions.rs:247-250），命令 id 会落 Unsupported；独立请求让 actions.rs 在 K0/K1 零改动 |
| AD5 | **命令出现在动作面板 = Actions 响应追加段，ActionId 枚举不动** | 面板项 id 用 `cmd:<命令id>` 前缀，前端面板 Enter 按前缀路由到 execute_command。ActionHotkeyCatalog/锚测试零改动，面板渲染零改动（ActionItem 五字段原样复用） |
| AD6 | **TargetKind 增加 `Command` 变体**（value=命令 id） | shell.rs validate() 当前拒绝未知 kind；command 行的 target 需要合法通过校验路径。加法枚举 + match 遍历强制补全；前端 unknown-kind 容忍已有测试先例（SearchViewModelTests.cs:409） |
| AD7 | **引擎收编降级**：K2 只做关键字命名空间查重 + 双侧规则锚测试；`WebModeDetector`/broker `try_match` 双实现保留 | 收编的收益是"消除规则双实现漂移"，但完整迁移要动 WebModeDetector/RunWebSearch/建议通道/broker web 行四处现有稳定路径，回归面大。漂移用"同一组用例向量同时写进 C# 与 Rust 测试"锁住，成本 1 小时 vs 迁移 3 天 + 全量网页搜索回归。真实收益出现（如引擎需要参数声明）再迁，K4 候选 |
| AD8 | **ui 命令前端直跑、broker 命令走管道**（设计文档既定），但 ui 命令的**数据落盘经 broker KV**（command_data_get/set） | P5 副作用跟进程走；note 等数据不属于设置也不属于暂存，独立 KV 文件 `command-data-v1.json`（LRU 上限 1 MB） |
| AD9 | **占位符修饰符命名对齐 Raycast**：`uppercase / lowercase / trim / percent-encode / raw`（链式，`\|` 分隔） | 行业先例命名零学习成本（联网复核确认 Raycast 修饰符全集，见 §12）；设计文档原提的 `url` 改为 `percent-encode`，`open_url` 的 `{query}` 未显式 `\|raw` 时强制 percent-encode 的规则不变 |

---

## 3. 数据模型与协议（K0 落地物，全部加法）

### 3.1 CommandSpec（存储与线上同形）

存储 `commands-v1.json`（数据目录，与 aliases-v1.json 同目录），VersionedEnvelope 家规：

```jsonc
{
  "schema_version": 1,
  "data": {
    "commands": [
      {
        "id": "user.3f2a…",              // "prism.*" 内置（不落盘）| "user.<uuid4>" 用户
        "title": "用 VS Code 打开",        // ≤64 字符
        "subtitle": "",                   // ≤128
        "icon": "",                       // Fluent 字形码（如 "\uE756"）；空 = 默认命令图标
        "keywords": ["vs", "code"],       // ≤4 个，各 ≤16 字符，无空白
        "argument": {                     // 可空 = 无参数命令
          "name": "路径", "required": true, "default": "", "hint": "要打开的文件"
        },
        "executor": "broker",             // "ui" | "broker"
        "handler_kind": "launch",         // 封闭枚举，双侧 match 遍历
        "handler_params": {               // String map；按 kind 校验必选/可选键
          "path": "C:\\Apps\\Code.exe",
          "args": "\"{query}\"",
          "working_dir": "", "admin": "false"
        },
        "applies_to": ["file", "directory"],  // TargetKind 字符串子集；空 = 不进动作面板
        "contexts": [],                   // "explorer" 等；空 = 任意上下文
        "enabled": true,
        "show_in_root_search": true,      // false = 仅关键字/热键/面板可达
        // 仅存储层，线上目录不含：
        "execute_count": 0, "last_used_utc": 0, "frecency_milli": 0
      }
    ]
  }
}
```

**handler_kind 封闭清单（v1）**：

| executor | kind | params | 阶段 |
|---|---|---|---|
| broker | `open_url` | `url_template`（含 `{query}`） | K3（用户命令）；K1 内置引擎兜底行仅前端 |
| broker | `launch` | `path`*, `args`, `working_dir`, `admin` | K3 |
| broker | `open_terminal` | `profile`（空 = wt 检测回退 cmd） | K1 |
| broker | `system` | `op`: lock/sleep/shutdown/reboot/empty_bin | K1 |
| broker | `file_op` | `op`: zip_all/copy_paths/move_to（move_to 另需 args.destination） | K2 |
| ui | `calc` / `note_add` / `note_list` / `settings_page`（params.page）/ `exit_prism` | — | K1 |

**边界（VersionedData::validate，镜像 alias 限制风格）**：用户命令 ≤64 条；关键字全局唯一且不得与引擎关键字/别名/保留字（`>`、`workset` 等）冲突（冲突校验在 CommandSet 时执行，存储层只查自身重复）；params map ≤8 键、值 ≤1024；文件总量 ≤256 KB；未知 handler_kind/executor/applies_to 值 → 该条目禁用并计数上报（容忍不拒绝，P6）。

**内置命令表（commands.rs 静态数组，不落盘）**——K1 首发：

| id | executor:kind | 说明 |
|---|---|---|
| `prism.calc` | ui:calc | 表达式自然检测出结果行（零前缀） |
| `prism.terminal.here` | broker:open_terminal | `wt -d {current_folder}`，wt 缺失回退 `cmd /K`；contexts: ["explorer"]；applies_to: [directory] |
| `prism.system.lock` / `sleep` / `shutdown` / `reboot` / `empty_bin` | broker:system | Listary 内置命令对齐 |
| `prism.settings.open` | ui:settings_page | Listary `opt` 对齐 |
| `prism.reindex` | ui:settings_page(page= indexing) | |
| `prism.exit` | ui:exit_prism | |
| `prism.note.add` | ui:note_add | KV 通道验证 |
| `prism.note.list` | ui:note_list | 回流最小版：本地行再生成（workset 行同款机制） |
| `prism.staging.zip_all` / `copy_paths` / `move_to` | broker:file_op | K2 启用（K1 先注册占位，applies_to 空） |

### 3.2 IPC 新增（Request 6 个 / Response 4 个，全部加法变体）

```jsonc
// Request（加进 Request 枚举，snake_case tag）
{"type":"command_list"}
{"type":"command_set","spec":{…}}                       // 全量 upsert；id 冲突 = 覆盖
{"type":"command_delete","id":"user.x"}
{"type":"execute_command","id":"prism.system.lock",
 "argument":null,                                        // 参数态键入值
 "query":"lock",                                         // 原始查询（frecency 键控）
 "context":{"current_folder":null,"selection":null,"staged":[]}}   // K1 仅 current_folder；K2 补 selection/staged
{"type":"command_data_get","key":"note:1"}
{"type":"command_data_set","key":"note:1","value":"…"}
{"type":"command_preview","id":"user.x","argument":"E:\\a.txt","context":{…}}   // K3，dry-run

// Response
{"type":"command_catalog","items":[…],"version":7}      // 内置+用户合并；version 单调递增
{"type":"command_applied","message":"已执行"}
{"type":"command_data","value":null}                    // value 可 null
{"type":"command_preview","lines":["C:\\Apps\\Code.exe \"E:\\a.txt\""]}          // K3
{"type":"clipboard_set"}                                // K1，ui:calc 复制结果用（复用 actions::clipboard_set_text）
```

配套枚举扩展：

- `SearchResultKind` 增加 `Command`（serde `"command"`；声明位放 Folder 之后：App, File, Folder, **Command**, Web, Window——Ord 仅作最终 tiebreak，命令排在 Web 前）；
- `TargetKind`（shell.rs）增加 `Command`，validate：value 非空、≤128 字节、无控制字符（复用 TargetInvalid 规则子集）；
- 未知 Request type 在**旧 broker** 上 → `Response::Error`（连接不断，ipc.rs:915-923 实测行为）——前端必须捕获并降级（§8）。

### 3.3 占位符与修饰符（broker 单点实现，commands.rs）

- 占位符 v1：`{query}`（=argument）、`{current_folder}`（context）、`{clipboard}`（K2，默认关）、`{staged}`（K2，模板型展开为逐目标执行）；
- 修饰符：`uppercase / lowercase / trim / percent-encode / raw`，链式 `{query | trim | percent-encode}`；
- `open_url` 的 `{query}` 未显式 `|raw` → 强制 percent-encode（websearch.rs:153-169 现成函数复用）；
- 展开函数 `expand_template(template, ctx) -> Result<String>` **同时服务执行与 K3 预览**——预览与实际执行逐字节一致由构造保证（同一函数）。

---

## 4. K0：内核地基（3-4 天）

**目标**：类型、存储、协议就位，零行为变化（没有任何用户可见新东西）。

### 4.1 broker（Rust）

| 任务 | 落点 | 要点 |
|---|---|---|
| 新模块 `commands.rs` | lib.rs 加 `pub mod commands;` | `CommandEntry`/`CommandSpecDto`/`CommandStore`（AliasStore 模式：RwLock + Mutex<()> persist_lock + tmp + atomic_replace）；builtin 静态表；`catalog()` 合并视图；version 计数器（AtomicU64，每次变更 +1）；frecency 字段与衰减公式（从 history.rs:606-610 复制常量与算法，不引用 history store） |
| persistence.rs | 加 `COMMANDS_SCHEMA_VERSION = 1` + `CommandData` + `impl VersionedData` | 校验规则 §3.1；corrupt/future-schema → 空表起步（不隔离重命名——命令可重建，不需要 history 级抢救） |
| ipc.rs 协议 | Request/Response 加变体（§3.2）；`SearchResultKind::Command` | serde 往返测试照 protocol_tests 模式（ipc.rs:3636-3700 先例） |
| ipc.rs 穿参 | BrokerShared 加 `Arc<CommandStore>`（第 8 个句柄）；main.rs 构造 | 沿用现状六跳；**不做** BrokerContext 重构（E6） |
| dispatch_non_search | command_list/set/delete/data_get/data_set 空实现级接通 | command_set 完整校验 + 持久化 + version++；execute_command K0 返回 Error("命令执行未开放")占位 |
| shell.rs | `TargetKind::Command` | validate 分支 + match 遍历补全 |

### 4.2 前端（C#）

| 任务 | 落点 | 要点 |
|---|---|---|
| `Models/CommandSpec.cs` | 新文件 | DTO + 容忍解析（未知 handler_kind/executor → `IsKnown=false` 但不抛）；wire 小写 kind/value（type-safety 规范） |
| `Services/CommandCatalog.cs` | 新文件 | 快照缓存 + `RefreshAsync()`（连接后/变更后/窗口显示时调用）+ `TryDetectKeyword(query)`（规则与 WebModeDetector 完全同构：首 token + 必须尾随空白、长关键字优先、忽略大小）+ **降级**：任何 command_* 请求收到 Error → 标记不可用，命令功能全部静默隐藏 |
| PipeClient | 加 6 个方法 | 放 action 通道（execute_command 5 分钟超时同 run_action；command_list 放 query 通道） |
| Settings | `CommandSuggestionsEnabled`（默认 false）、`CommandHotkeys`（字典，默认空） | SettingsStore.Validate：热键可解析、非保留键、不与 ActionHotkeys/StagingAddHotkey 重复；上限 = 目录快照大小 |

### 4.3 K0 验收门

- `cargo fmt --check` + `cargo test` + `dotnet test` 全绿；
- Rust：新变体 serde 往返；未知 handler_kind 容忍；commands-v1.json 空/缺/损/corrupt/超界各一测；catalog version 单调；
- C#：DTO 解析（含未知枚举值）；CommandCatalog 降级路径（FakeSearchClient 返回 error）；CommandHotkeys 校验用例；
- **旧前端二进制 + 新 broker** 手动回归：搜索/动作/网页/窗口/别名/暂存区全部不变（新变体未被调用，SearchResultKind::Command 未产生任何行）；
- 新前端 + **旧 broker**：命令功能静默隐藏，其余不变。

---

## 5. K1：命令上线（1-1.5 周）

**目标**：内置命令可用（关键字触发 + 根搜索混排 + 热键 + calc + note），默认对混排关闭。

### 5.1 broker

1. **execute_command 完整执行路径**（dispatch_non_search，走 action 通道语义）：
   - 校验：id 存在、enabled、contexts 包含当前上下文（current_folder 缺失但命令声明依赖 `{current_folder}` → Error("此命令需要资源管理器上下文")，治 Listary 痛点 3-② 的"静默传空"反面）；
   - required 参数为空 → Error("缺少参数：<name>")（治痛点 12）；
   - `expand_template` 展开占位符 → 按 executor 分派；
   - 成功/失败 → frecency bump（execute 权重 4.0，同 history 公式）+ `CommandApplied` 或 `Error`。
2. **shell.rs 新操作**（全部走既有 STA 队列与超时预算）：
   - `ShellOperation::LaunchProcess { path, args, working_dir, admin }`：admin=true 复用 ShellExecuteExW runas 路径（扩展 parameters/directory 字段，shell.rs:532-602）；否则 `std::process::Command` + `raw_arg`（reveal 的 explorer /select 同款先例，shell.rs:613-634）。**whitelist**：launch 目标必须存在且扩展名 ∈ {exe, lnk, bat, cmd}（K3 用户命令启用；K1 仅内置 open_terminal 用 LaunchProcess，目标 wt.exe/cmd.exe 经 PATH 解析）；
   - `ShellOperation::SystemOp(op)`：LockWorkStation / SetSuspendState / ExitWindowsEx（先 EnableShutdownPrivilege，失败返回结构化 Error）/ SHEmptyRecycleBinW（STA）。
3. **open_terminal**：wt 检测（PATH 上 `where wt` 一次性缓存）→ `wt -d <folder>`；缺失 → `cmd /K`，working_dir=folder。
4. **根搜索混排**（search_service，alias 块之后 ipc.rs:1949 附近）：
   - 门：`command_suggestions_enabled() && root.is_none() && !has_filters && query 非空`；
   - `commands::match_commands(query, max=2)`：title+keywords 走 `literal_match_lowered`（复用 ipc.rs:2771-2834）得 class 0/1/2，history_score = 命令 frecency；产出 `kind:Command` 行（execute_id=命令 id，target=ActionTarget{command, id}，match_spans 按命中词）；
   - `show_in_root_search=false` 或 enabled=false 的命令不参与；排序零新代码（进 ranked 后由 sort_search_results_with_picks 统一处理——class 分层天然满足"排在文件/应用精确命中之后、强匹配才靠前"）；
   - **配额**：match_commands 内部截断 2 条（模糊 class 2 仅在命中数 <2 时补位）。
5. **设置开关**：config.rs `CommandSuggestionsEnabled` 进 BrokerPreferences（AtomicBool，UpdatePreferences 扩字段——加法，旧请求缺省 false）。

### 5.2 前端

1. **路由分支**（RunSearchAsync，引擎关键字分支 898-906 之后、正常搜索之前）：
   ```
   Web 引擎关键字（现有，不动）
   → CommandCatalog.TryDetectKeyword(query) 命中：
       bump _searchSeq / CancelSearch / CancelSuggestions / _completeCache = null
       进入"命令参数态"：状态行显示命令标题+图标+参数 hint（RunWebSearch 同款staleness 纪律）
       Enter → ui 命令：UiCommandRouter 本地执行；
               broker 命令：ExecuteCommandAsync(id, argument, context{current_folder ← HostScopeController.Root})
   → 正常搜索（现有，不动）
   ```
   OnQueryChanged ~407 加早设标志 `IsCommandMode`（隐藏 scope 标签，与 IsWebMode 同款）；关键字无尾随空白 = 普通搜索（引擎同款心智，纯词仍是文件搜索，治 Q5）。
2. **本地行再生成**（ApplySearchResponse，workset 行 1032-1034 旁）：
   - calc 行：query 匹配算术形态（数字/括杠开头且含运算符）且 CommandSuggestionsEnabled → 追加"= <结果>"行（Kind:"calc"，RowKey:"calc:direct"）；Enter → `set_clipboard` 请求（broker 写剪贴板，P5）；从 complete-cache 排除（Web 行同款守卫 1060）；
   - note_list：命令参数态下 ui:note_list → 从 command_data 拉笔记列表生成本地行（Enter=复制/再编辑）。
3. **UiCommandRouter**（Services/ 新静态类，纯逻辑可测）：calc 解析器（递归下降 ~60 行，只认 + - * / % ( ) 与数字，溢出/除零 → 无行）；settings_page 导航（复用设置窗口打开逻辑）；exit_prism 走 App 退出路径。
4. **命令热键**（SearchWindow.OnHeaderKeyDownCore，ActionHotkeys 匹配 1281-1290 之后）：`CommandHotkeyTable`（FromSettings/TryMatch/Parse 全套抄 ActionHotkeyTable，保留键守卫复用）；命中 → ExecuteCommand/UiCommandRouter，不受选中行限制（但 contexts 校验仍在 broker）。
5. **图标**：ResultList.DecorateVisibleItems 加 `"command"`/`"calc"` 分支（冻结 DrawingImage，workset 分支 448-452 同款先例）；icon 字段非空 → Fluent 字形渲染。

### 5.3 K1 验收门

- 手测清单：双击 Ctrl → `lock`/`关机`（根搜索混排，开关开）→ Enter；`term `（注意尾空格）进参数态；explorer 内唤起 → terminal.here 打开 wt 于当前目录；非 explorer 上下文 → 明确报错不执行；`=2+2*3`…（无前缀 calc）→ 行出现、Enter 入剪贴板；热键绑定 `prism.calc` 直接触发；frecency：连选同一命令 5 次后其根搜索行名次上升；
- 回归：现有测试套件全绿 + 网页搜索/窗口模式/别名/暂存区/动作面板手测不变；
- 性能：混排开关开时搜索 P95 ≤100ms 不回归（match_commands 是 O(命令数×关键字数)，64 上限下 <0.1ms）；三进程内存 ≤100MB 不回归；
- 兼容：开关默认 false 时全部行为与 K0 前一致（自动化：SearchViewModelTests 加"开关关 = 无 calc 行/无命令路由"用例）。

---

## 6. K2：联动解锁（1-1.5 周）

**目标**：命令注册为动作（applies_to）、暂存区批量、`{selection}/{staged}/{clipboard}` 上下文、Actions 面板收编。

### 6.1 broker

1. **Actions 响应追加命令段**（AD5）：dispatch 的 actions 处理器在 `actions::list_actions` 结果后，若 target kind ∈ 某 enabled 命令的 applies_to → 追加 `{is_section_header:true,label:"命令"}` + 命令项（id=`cmd:<id>`，label=title，icon_glyph）按 frecency 排序 cap 5（Q3）；
2. **context 扩展**：execute_command 的 context 解析 selection/staged（Vec<ActionTarget>，逐个 validate——UI 只传 id+context，路径与存在性 broker 侧重校验，现有信任模型延伸）；
3. **file_op 聚合型**：zip_all（zip.rs 循环 + 汇总）、copy_paths（clipboard_set_files 现成）、move_to（args.destination + IFileOperation 批量，原子语义，Q8）；模板型（launch/open_url 含 `{staged}`）→ 逐目标展开执行 + 结束汇总报失败数；
4. **`{clipboard}`**：actions.rs 新增 `clipboard_get_text`（STA，OpenClipboard-GetClipboardData-CloseClipboard 即读即弃，不缓存不入日志）；门 `AllowClipboardContext`（默认 false，E4）。

### 6.2 前端

1. **面板路由**：ActionPanel Enter/上下文菜单项 id 以 `cmd:` 开头 → ExecuteCommand(context.selection=当前 target)（ExecuteSelectedAsync 动作分支旁加前缀分支）；
2. **暂存区批量**：StagingStrip 加"执行命令"按钮 → 弹轻量列表（快照中 applies_to 匹配暂存区类型集合的命令）→ 选中：move_to 先弹 DestinationPicker（IFolderPicker 现有 seam）→ ExecuteCommand(context.staged=[targets])；
3. **IsActionableSelection 不动**（命令动作经面板 cmd: 前缀路由，不进 kind 门）。

### 6.3 K2 验收门

- 面板：文件结果 → 面板出现"命令"段且 cap 5；分组顺序内置段在前；对 directory 选"在此打开终端"（applies_to 声明的用户命令）端到端；
- 暂存区：3 文件 → zip_all 出 3 个 zip；copy_paths 入剪贴板可粘贴；move_to 经选择器落盘；
- `{clipboard}`：默认关 → 未展开报占位符缺失；开 → 展开且日志无内容记录；
- 引擎回归：网页搜索全部行为不变（AD7 下引擎路径零改动，跑 G8WebEnhancementsTests 全量）。

### 6.4 引擎收编降级执行（AD7）

1. 命名空间统一：CommandSet 与前端表单的冲突校验集合 = 引擎关键字 ∪ 别名 ∪ 内置命令关键字 ∪ 保留字；
2. 漂移锚：同一组检测用例向量（"g"无空白/"g "/"G x"/"bi b"长词优先/制表符分隔）同时写进 `WebModeDetectorTests` 与 Rust `websearch` 测试，注释互指；
3. `UnifiedCommandRouting` 设置键预留（默认 false），K4 若做完整收编用它灰度。

---

## 7. K3：用户命令闭环（1-2 周）

**目标**：设置页命令管理、launch/open_url 表单、dry-run 预览、导入导出、Fallback 行。

### 7.1 设置页（SettingsWindow 第 5 个 tab"命令"，快速访问之后）

- TabIndex 常量 + IsCommandsTab + 导航按钮 + 面板（现有四 tab 模式，SettingsWindow.xaml:360-379）；
- 列表：标题/关键字/enabled/图标；增删改；
- 表单：kind 二选一（launch / open_url）+ §3.1 全字段（多关键字、可选参数+默认值、占位符修饰符、working_dir、admin、silent、图标、applies_to、show_in_root_search）；bat/cmd 目标显式标注"将以脚本执行"；
- **模板预设 6-8 个**（治 Listary 痛点 2）：VS Code 打开、记事本打开、管理员 PowerShell、GitHub 仓库搜索、百度/Google 搜索、复制文件名、用浏览器打开本地 HTML；
- **实时解析预览**（治痛点 8）：参数框 + 预览区，输入即调 `command_preview` 展示展开后命令行/URL——与执行共用 expand_template，逐字节一致由测试锁；
- **导入导出**（治痛点 6/7）：导出 = command_list dump 为 commands-v1.json 信封 + exported_at；导入 = 选文件 → 解析 → 展示待导入清单（标题/handler/路径）→ 首跑确认（不可信输入，§9）→ 逐条 command_set；
- **Fallback 行配置**（CmdPal/Raycast 模式）：默认"用默认引擎搜索原词"；可换任意命令；总开关。

### 7.2 Fallback 行（前端）

- ApplySearchResponse：`resp.Items 为空 && !resp.IsIndexing && query 非空 && 开关开` → 追加兜底行（本地生成，web 行基建复用）；Enter → 引擎打开或 ExecuteCommand；
- 与"more"/workset/calc 行同款再生成机制，complete-cache 排除。

### 7.3 K3 验收门

- 导入导出往返无损（自动化：导出 → 删 → 导入 → catalog 深比较）；预览与执行逐字节一致（Rust 测试：expand 结果 == preview lines == 实际 spawn argv）；冲突校验用例（引擎/别名/保留字/彼此重复各一）；首跑确认流程手测；
- launch 安全面：相对路径拒绝、不存在拒绝、非白名单扩展拒绝、admin 走 UAC（手动）、bat/cmd 标注呈现。

---

## 8. 兼容与回滚矩阵

| 场景 | 结果 | 依据 |
|---|---|---|
| 新 broker + 旧前端（升级 broker 先行） | **零影响**：新 Request 未被调用；command 行只在 CommandSuggestionsEnabled=true 时产生，默认 false | AD1；SearchResultKind 加法序列化，旧端未知 kind → Unknown（容忍已测） |
| 旧 broker + 新前端（回滚 broker） | 命令功能静默隐藏（command_* 请求 → Response::Error → CommandCatalog 降级位），其余全功能 | ipc.rs:915-923 未知 type 行为；K0 验收门含此场景回归 |
| 回滚到旧版后重升 | commands-v1.json 未被旧 broker 读取（未知文件），重升后原样可用；history/alias/settings/staging 全程不动 | E2/AD2 |
| settings.json 携带新字段被旧组件读 | 旧 broker Config 是容忍子集（serde default + alias），旧前端 SettingsStore 同理（前端忽略未知字段为现状行为） | config.rs:23-54 实测 |
| commands-v1.json 损坏 | 空表起步 + Error 上报，不隔离不重命名其他文件 | §4.1 |
| indexer | **零改动、零感知**（K0-K3 不触碰 indexer 管道与协议） | 只读边界不动 |

每阶段独立可回滚：K0/K1 仅新增文件 + 加法分支，回滚 = 还原二进制；K2 面板命令段随目录为空自动消失；K3 用户数据独立成文件。

---

## 9. 安全边界（执行清单）

1. broker 全量重校验：UI 只传 id + argument + context；selection/staged 的每个 target 逐个 validate（存在性/类型/路径边界）；
2. launch 白名单：exe/lnk/bat/cmd 且必须存在；bat/cmd UI 显式标注；**不执行 Shell 字符串**（无 cmd /c 面——LaunchProcess 是 argv 数组 + raw_arg，不走 shell 解析）；
3. admin = ShellExecuteExW verb "runas"，UAC 系统呈现，broker 不绕过；
4. `{clipboard}` 默认关、即读即弃、不入日志/历史/预览缓存；
5. 导入命令 = 不可信输入：解析后展示清单 + 首跑确认；
6. ui 命令不直接碰磁盘：数据经 command_data KV（broker 落盘，LRU 1MB）；
7. frecency 只记命令 id 与查询键，不记参数内容（参数可能含敏感路径）。

---

## 10. 冲突最小化保证（现有功能逐一对照）

| 现有机制 | 本方案触碰点 | 保证不变 |
|---|---|---|
| 文件/应用/文件夹搜索 | search_service 加一个命令行生成块（alias 块后） | 默认关；开时配额 2 + 现有排序框架分层；class 分层保证文件精确命中永远在前 |
| 窗口模式（`>`） | 零改动（K2 可选尾件才考虑窗口轻动作） | 路由顺序原位不动 |
| 网页搜索（引擎/联想/URL 检测） | 零改动（AD7 降级后连收编都不做） | WebModeDetector/RunWebSearch/建议通道原样；仅新增关键字冲突校验 |
| 别名 | 零改动；关键字查重把别名纳入保留集 | 匹配语义/存储/设置页不动 |
| 动作面板 16 项 | Actions 响应追加段（K2）；ActionId/actions.rs/allowed_actions 不动 | 面板顺序/文案/热键/锚测试不动；命令段独立分组 cap 5 |
| ActionHotkeys | 零改动（命令热键独立字典 AD3） | 目录/锚测试/保留键不动 |
| 暂存区/工作集 | StagingStrip 加一个按钮（K2）；StagingArea/StagingStore/StagingPolicy 零改动 | 拖拽/workset 行/Ctrl+D/容量策略不动 |
| 设置页 | 加第 5 tab；现有 4 tab 不动 | settings.json 现有字段不动，新字段全带默认 |
| G4 上下文识别 | 只读 HostScopeController.Root 作 context 来源 | 识别链/适配器零改动 |
| indexer | 零改动 | — |
| 前端路由 | 两个新分支（命令关键字、calc 行）均插在现有分支之后/旁侧 | 现有分支顺序与行为逐字节不变（K0 验收门含旧二进制回归） |

---

## 11. 风险登记册（承接设计文档 Q1-Q8，状态更新）

| # | 风险 | 处置 | 状态 |
|---|---|---|---|
| Q1 | 命令行与文件行排序观感冲突 | 默认关 + 配额 2 + class 分层 + 设置开关灰度 | 已内置（AD1） |
| Q2 | 旧二进制读含 command 的 history | history 不动，命令 frecency 自带存储 | **消解**（E2/AD2） |
| Q3 | 面板收编后变长 | 分组 + frecency cap 5 | 已内置 |
| Q4 | `{clipboard}` 隐私 | 默认关 + 即读即弃 + 不入日志；有顾虑可整体不启用该占位符展开（Error 提示） | 已内置 |
| Q5 | 关键字误触发 | 尾随空白才生效（引擎同款）；纯词=文件搜索 | 已内置 |
| Q6 | 引擎收编 UX 融合 | AD7 降级为零改动方案，风险移除 | **消解** |
| Q7 | list 回流导航深度 | v1 单层（参数态列表，Back/退格回关键字），多层等真实需求 | 已内置 |
| Q8 | 暂存区批量中途失败 | 聚合型交 IFileOperation；模板型逐个 + 汇总失败数 | 已内置 |
| Q9（新） | ipc.rs 继续膨胀（commands 是第 8 个穿参句柄） | K0-K3 不重构（E6）；commands.rs 自身独立模块承接主要增量；BrokerContext 收拢列入 K4 前置项 | 接受监控 |
| Q10（新） | 命令关键字与引擎关键字未来仍可能漂移 | 双侧锚测试向量（§6.4.2） | 已内置 |

---

## 12. 联网复核结论（2026-08-23）

| 来源 | 复核点 | 结论 |
|---|---|---|
| [PowerToys Command Palette 扩展模型](https://learn.microsoft.com/en-us/windows/powertoys/command-palette/extensibility-overview) | 扩展 = MSIX appExtension + 进程外 COM server + WinRT API；命令类型 top-level/fallback/context-menu；页面 List/Detail/Form/Markdown/Grid | 证实 K4 延后正确：该模型依赖打包与商店基建，Prism stdio JSON-RPC 预留足矣；fallback 命令产品化细节（无结果触发）被 §7.2 吸收 |
| [Raycast 搜索排序](https://manual.raycast.com/search-bar) | 精确别名 > 别名前缀 > 标题模糊 > 副标题/关键字 > frecency；Reset Ranking | 与本方案 class 分层（Literal class 0/1/2 + frecency tier）同构，排序设计有先例背书 |
| [Raycast Arguments](https://developers.raycast.com/information/lifecycle/arguments) | 上限 3 参数；类型 text/password/dropdown；required 标志；顺序敏感 | Prism v1 单 text 参数 + required/default 是该模型的收敛子集，扩展路径清晰 |
| [Raycast Dynamic Placeholders](https://manual.raycast.com/dynamic-placeholders) | 占位符全集与修饰符（uppercase/lowercase/trim/percent-encode/json-stringify/raw，链式） | AD9 命名对齐依据；json-stringify 列入 K4 候选（当前无消费场景） |

---

## 13. 执行顺序与工作量

```
K0 内核地基      3-4 天    零行为变化，可单独合入
K1 命令上线      1-1.5 周  内置命令可用（默认混排关）——首个可发布价值点
K2 联动解锁      1-1.5 周  面板命令段 + 暂存区批量 + 上下文源
K3 用户闭环      1-2 周    设置页/预览/导入导出/Fallback
K4 探索（未承诺）          外部扩展进程、引擎完整收编、类型化多参数、BrokerContext 收拢
```

严格顺序 K0→K1→K2→K3；每阶段验收门（§4.3/§5.3/§6.3/§7.3）不过不进下一阶段。每阶段一个合并单元，含双侧测试与协议文档更新（`docs/` 下协议说明随 K0 落盘，新变体写入同一份文档）。

---

## 14. 附：本方案引用的代码锚点速查（2026-08-23 工作树）

| 锚点 | 位置 |
|---|---|
| Request/Response 枚举 | ipc.rs:71-166 / ipc.rs:187-265 |
| SearchResult / SearchResultKind | ipc.rs:284-311 |
| search_service 合并管线 | ipc.rs:1711-1997（web 行 1812-1818、alias 块 1932-1963、终排序 1974） |
| 排序比较器 / literal_match_lowered | ipc.rs:2868-2898 / ipc.rs:2771-2834 |
| MatchMetadata::cmp / usage_tier | hierarchy.rs:186-201 / hierarchy.rs:174-184 |
| ActionId 16 项 / allowed_actions | actions.rs:17-54 / actions.rs:190-228 |
| execute_run_action / STA 队列 | shell.rs:289-386 / shell.rs:94-97, 213-267 |
| TargetKind validate | shell.rs:425-500 |
| try_match / url_encode | websearch.rs:93-169 |
| alias 存储/匹配 | alias.rs:23, 158-171 |
| history 持久化纪律 / 封闭 kind 校验 | history.rs:692-761 / persistence.rs:98-103 |
| VersionedEnvelope | persistence.rs:10-43 |
| Config 容忍子集 | config.rs:23-91 |
| 前端路由两阶段 | SearchViewModel.cs:339-412 / 878-1002（URL 884-893、引擎 898-906） |
| ApplySearchResponse 本地行注入 | SearchViewModel.cs:1018-1133（more 1028、workset 1032-1034、cache 排除守卫 1054-1060） |
| WebModeDetector 规则 | WebModeDetector.cs:21-76 |
| ActionHotkeyCatalog（15 项镜像）/ Table | ActionHotkeyCatalog.cs:30-44 / ActionHotkeyTable.cs:25-118 |
| 暂存区策略/存储 | StagingArea.cs:38-146 / StagingStore.cs |
| 面板数据流 | SearchViewModel.cs:585-605 → PipeClient.GetActionsAsync |
| 图标分支先例 | ResultList.xaml.cs:383-497（workset 448-452） |
| 设置 tab 模式 | SettingsWindow.xaml:360-379 / SettingsViewModel.cs:39-45 |
| G4 上下文链 | HostScopeController.cs:121-201（Root 即 current_folder 来源） |
