# Prism 统一命令系统设计（Unified Command Kernel）

> **文档性质**：基于当前仓库复核后的实施设计，待批准后进入开发。
> 日期：2026-08-26（v3，按提交 `41df5b3` 所在 feature 分支复核；取代 2026-08-22 v2）
> 本次仅修订方案，不改产品代码。工作树中现有 `artifacts/*.ps1` 未纳入本方案，也未被修改。
>
> **结论先行**：统一命令系统总体可行，但 v2 不能直接实施。正确方向不是把搜索、网页、别名、动作、窗口、暂存区强行改造成同一种对象，而是增加一个**可搜索的命令目录 + 类型化调用上下文 + 分层执行器**，再通过适配器接入现有功能。这样能达到“统一入口、注册即接入、跨子系统联动”，同时不破坏已经稳定的搜索排序、网页联想、窗口激活和文件动作链路。

---

## 1. 目标与边界

### 1.1 产品目标

1. **统一发现**：系统能力可以通过根搜索、关键字参数态、动作面板和快捷键被发现与调用。
2. **低成本扩展**：新增能力主要由一份命令描述 + 一个受控 handler 完成，不再重复实现搜索行、排序、参数提示、历史和设置入口。
3. **类型化联动**：命令可显式接收当前选中项、暂存区路径快照和当前宿主目录，不通过字符串猜测数据含义。
4. **保留专长**：文件搜索、网页联想、窗口激活、别名置顶和暂存区工作集继续使用各自已经验证的机制，命令内核只统一其共同契约。
5. **可降级、可回滚**：新旧前端/broker 混用时不得误显示或误执行命令；命令数据不得污染现有搜索历史和设置文件。

### 1.2 明确不做

- DLL、进程内插件和插件市场；
- 任意 Shell 字符串执行器（例如直接把用户文本交给 `cmd /c`）；
- 首期支持 `.bat`、`.cmd`、`.ps1` 等脚本目标；
- 命令链、可视化 workflow、跨命令函数调用；
- 自然语言或 AI 参数抽取；
- 在首期重写现有 16 个 `ActionId`、网页联想、工作集导航或搜索排序内核；
- 在首期实现通用 `list -> Item[]` 页面栈、便签 KV 平台或外部扩展进程。

### 1.3 设计原则

| # | 原则 | 约束 |
|---|---|---|
| P1 | **统一契约，不强行同构** | Provider、Item、Command、Action、Alias 是不同概念，只统一发现、调用和上下文边界 |
| P2 | **注册一次，多入口适配** | 命令描述可声明根搜索、关键字、动作面板、窗口内快捷键等 surface；每个 surface 仍可按产品需要关闭 |
| P3 | **副作用跟进程和 OS 权限走** | 通用文件/用户命令副作用归普通用户 broker；窗口激活、原宿主实例操作等与前台/UI 生命周期绑定的适配器留在 WPF；indexer 永远只读 |
| P4 | **显式调用来源** | 参数来源由 `InvocationSource` 决定，不使用“键入 > 选中 > 暂存 > 剪贴板”的隐式猜测链 |
| P5 | **协议请求/响应严格配对** | 不发送未经请求的 `CommandChanged` 推送；目录刷新通过 generation + 显式 `CommandList` 完成 |
| P6 | **功能协商后再出新行为** | `kind=command` 只有声明支持能力的新前端才会收到；不能依赖“旧前端会忽略未知 kind” |
| P7 | **现有稳定路径只做加法** | 首期不迁移 `ActionId`，不改 `history-v2.json`，不把命令字段塞进 `settings.json` |
| P8 | **用户定义命令能力小于内置命令** | 导入文件不能选择关机、重建索引、设置页等特权 handler |

---

## 2. 当前仓库事实（2026-08-26）

### 2.1 进程和 IPC

```text
Prism.exe（WPF，前台/UI/宿主上下文/窗口激活/暂存区）
    ├─ 查询管道连接 ─┐
    ├─ 动作管道连接 ─┴─> prism-core.exe（普通用户 broker）
    │                             └─ 搜索请求 -> prism-indexer-service.exe
    └─ generation/status 直连 ------> prism-indexer-service.exe（LocalSystem，只读索引）
```

- broker 管道协议当前为 `ProtocolVersion = 1`，前端有查询、动作两条独立连接；每条连接都先 `hello`。
- WPF 还通过 `IndexerGenerationClient` 直连 indexer 获取 status/generation；命令目录 generation 必须使用独立命名和状态，不得复用 `_generationDebounce`。
- broker 连接是严格的一请求一响应。读取一半后取消会导致协议错位；因此不能加入无请求的服务端推送。
- indexer 协议为 v2，与命令系统无关，且不得增加 Shell、命令、剪贴板或网络能力。

### 2.2 已有机制

| 能力 | 当前真实落点 | 当前关键约束 | 命令系统处理方式 |
|---|---|---|---|
| 文件/文件夹/应用搜索 | broker `ipc.rs::search_service` 合并并排序 | 现有全局 Top-K、拼音、history/pick、过滤和范围契约稳定 | 保持原样，仅在末端合并命令候选 |
| 窗口模式 | broker 枚举/匹配/历史；WPF 复核后激活 | `>` 独占模式；窗口 token 易失；`SetForegroundWindow` 必须在前台 WPF | 作为专用 QueryIntent 保留，不改成 broker Shell 命令 |
| 网页模式 | WPF `WebModeDetector` + `WebSearchCoordinator` 独占编排；broker 仍有 AllMode 兜底行 | 直接行同步出现、联想异步、800ms 超时、隐私开关、favicon 授权 | 先保留专用模式，后续只通过目录适配器统一关键字和展示元数据 |
| 裸 URL | WPF 在管道搜索前识别 | 有 TLD 白名单和文件名误判防护 | 保留 Router 内建意图，不伪装成用户命令 |
| 别名 | broker 精确词通道 + WPF 设置 UI | 精确整词、显式置顶；2026-08-26 起应用别名可解析 `.lnk` 绑定真实 exe | 保持“查询加速器”身份，不迁为 CommandProvider |
| 文件动作 | broker `ActionId` 封闭 16 项，按 target allowlist；WPF 编排重命名/目录选择/删除确认 | `ActionId` 有 16 个枚举值，但当前可见目录是 15 个稳定动作；窗口/Web 不进文件动作面板 | K2 组合命令动作，不在 K0 重写现有动作 |
| 动作快捷键 | WPF `ActionHotkeyCatalog` 静态镜像 + `ActionHotkeys` | 是搜索窗口内、依赖当前选中 target 的动作快捷键，不是全局命令热键 | 命令快捷键单独建模，不复用该字典 |
| 暂存区/工作集 | WPF `StagingArea` + `StagingStore`，持久化为路径列表 | `StagingItem` 只有 `Path`/`Workset`，不是 `ActionTarget`；工作集和临时暂存语义已分离 | 作为调用上下文来源；在执行边界解析/复核路径类型 |
| 工作集召回行 | WPF 在响应后注入 `kind=workset` 合成行 | 不经 broker，Enter 后载入暂存区 | 保留前端适配器，不作为通用 list handler 的先例强推平台化 |

### 2.3 2026-08-22 之后影响本方案的变化

1. `history-v2.json` 已经上线，包含 frecency 和 query-pick；其校验只允许 `file/directory/application/window`。
2. 网页模式已从“前后端镜像一个简单检测器”发展为前端独占的异步联想流程，不能简单退役 `WebModeDetector.cs`。
3. 别名精确行会绕过普通排序质量键置顶；前端前缀缓存还专门维护别名词集以避免漏行。
4. 应用动作已区分 `.lnk` 执行语义和真实 exe 语义：例如 `run_as_admin` 保留 `.lnk` 参数，而复制路径/属性按动作解析真实 exe。命令上下文不能全局把应用 target 改写成 exe。
5. 动作执行有双击在飞守卫、mutation 超时“结果未知”、目录选择失活守卫、永久删除前台协同等成熟约束。通用命令执行必须复用这些纪律，而不是绕开它们。
6. 前缀缓存已因窗口 token、网页、`ext:/path:`、绝对路径和别名增加多重资格判断。命令结果必须声明缓存语义，不能默认进入本地子串过滤。

---

## 3. 对 v2 的可行性评估与修正

### 3.1 总体判断

| 维度 | 判断 | 说明 |
|---|---|---|
| 产品价值 | 高 | 统一发现、上下文动作和暂存区批处理与 Prism 现有能力互补 |
| 架构可行性 | **有条件可行** | 需要能力协商、分层执行器和新 InvocationContext；不能只扩一个 `CommandSpec` |
| 对搜索性能影响 | 可控 | 根搜索只取少量强匹配命令，命令目录内存化；需显式处理前缀缓存 |
| 对 IPC 风险 | 中 | 新请求多，但可保持请求/响应配对；严禁目录变更推送 |
| 对安全风险 | 中高 | 用户 launch/import 是新执行面，必须把用户能力限制在安全子集 |
| 实施复杂度 | 中高 | MVP 约 2–3 周；完整动作/暂存/用户命令闭环约 4–7 周，需分阶段验收 |

### 3.2 v2 中不能照做的内容

| v2 假设 | 当前问题 | v3 修正 |
|---|---|---|
| Router 落在 `OnQueryChanged` | 真正异步路由在 `RunSearchAsync`；`OnQueryChanged` 主要维护 UI 状态和防抖 | 新建纯函数 `QueryRouter`，由 `RunSearchAsync` 调用；`OnQueryChanged` 只做即时状态投影 |
| 窗口由“前端枚举 + broker 校验” | 当前是 broker 枚举/排序，WPF 只负责激活 | 明确 broker→WPF 双阶段窗口调用，禁止路由进 ShellExecutor |
| 网页引擎收编后退役 `WebModeDetector` | 会丢失同步直达行、异步联想、请求取消和隐私控制 | 网页保持专用 handler；目录只统一关键字、展示和冲突校验 |
| `ActionTarget` 可零新增类型承载 `{staged}` | 它只能表示一个 target；暂存区只存路径且可能失效 | 新增 `CommandInvocationContext`，显式包含 selection、staged path snapshot、current folder |
| broker ActionRegistry 权威、前端镜像即可 | 窗口激活和部分 UI 命令只能在前端执行；现有动作还有 WPF 子流程 | broker 权威管理“描述和策略”，执行按 owner 分层；内置 `ActionId` 暂不迁移 |
| 旧前端会忽略 `kind=command` | 当前未知行仍可能显示，Enter 还会走通用 Execute；不是安全忽略 | hello 能力协商；未声明 `commands_v1` 的连接绝不返回命令行 |
| `CommandChanged` 服务端推送失效 | 会破坏 PipeClient 的请求/响应配对 | mutation 回包带新 generation；搜索响应提示 generation，前端再显式 `CommandList` |
| 直接给 `history-v2` 加 command kind | 旧 broker 会因不支持 kind 拒绝整个文件，回滚可能表现为历史重置 | 命令使用记录单独存 `command-usage-v1.json` |
| K0 把 16 动作全部迁入 Registry | 大改稳定路径，且 `LocateApp`/`OpenFolder`、`.lnk` 解析已有细节 | K0 不动；K2 用 ActionComposer 在现有动作后追加命令动作 |
| `ActionHotkeys` 扩展绑定命令 | 其语义是“对当前结果执行动作”，与独立命令调用不同 | 新建命令快捷键绑定；首期只做搜索窗口内快捷键，全局 RegisterHotKey 后置 |
| K1 同时上 note KV 和 list 回流 | 与命令内核无关，显著扩大存储和导航状态机 | 从 MVP 删除；待真实查询型功能出现后单独设计 |
| 暂存区批量可直接复用现有 IFileOperation | 当前 `file_ops`/`zip` 都以单个 `ActionTarget` 为主，批量输出和部分失败尚无合同 | K2 新增真正的批量 API；首发优先无损动作，不把 `move_to` 当作零成本能力 |

---

## 4. 修订后的总体架构

```text
用户输入 / 当前选中项 / 暂存区工具栏 / 快捷键
                    │
                    ▼
            QueryRouter（WPF 纯函数）
     window │ direct-url │ web │ command-keyword │ normal
                    │
       ┌────────────┴─────────────┐
       │                          │
专用 UI 编排                    broker 查询
窗口激活/网页联想        files/apps/alias + command candidates
       │                          │
       └────────────┬─────────────┘
                    ▼
              SearchResult[]
                    │
          Enter / 动作面板 / 快捷键
                    ▼
          CommandInvocationContext
                    │
       ┌────────────┴────────────┐
       │                         │
 UI Handler Registry       Broker CommandDispatcher
设置/退出/窗口前台能力      Shell/COM/文件/系统安全边界
       │                         │
       └────────────┬────────────┘
                    ▼
             类型化结果与错误
```

### 4.1 五个概念，不再混为“四原语”

| 概念 | 回答的问题 | Prism 中的例子 |
|---|---|---|
| Provider | 可搜索对象从哪来 | 文件、应用、窗口、命令目录 |
| Item | 结果列表显示什么 | `SearchResult`；包括 file/app/window/web/command |
| Command | 系统能执行什么能力 | 打开设置、锁屏、在此打开终端、用户 URL 命令 |
| Action | 对当前 Item 做什么 | 复制路径、属性、用某工具打开；可由内置 `ActionId` 或 Command 提供 |
| Query accelerator | 如何更快找到既有 Item | 别名、`ext:/path:`；它们不是 Provider，也不是 Command |

“结果即命令”只在**结果可以触发调用**这个层面成立，不表示每个结果都应转换为 `CommandSpec`。文件是资源、命令是能力，两者通过 Action 和 InvocationContext 连接。

### 4.2 权威归属

| 数据/行为 | 权威方 | 原因 |
|---|---|---|
| 命令描述、关键字冲突、用户命令存储、命令使用记录 | broker | 跨 surface 一致，且用户命令执行必须在安全边界内复核 |
| 根搜索命令候选和命令 lane 排序 | broker | 与文件/app/alias 的最终合并排序同地完成 |
| 本地关键字快车道快照 | WPF 缓存，broker 提供 generation | 输入时立即进入参数态，不能每键先发目录请求 |
| UI handler 实现 | WPF 编译期注册表 | 设置页、退出、窗口前台能力只能在 UI 进程 |
| broker handler 实现 | broker 编译期注册表 | Shell、COM、文件和系统副作用归 broker |
| 用户命令 handler | broker 受限解释器 | 只能使用白名单执行种类，不能访问内置特权 handler |
| 网页联想、favicon、直接 URL | WPF 现有服务 | 已有取消、超时、隐私和 UI 状态合同 |
| 暂存区和工作集 | WPF | 生命周期跨呼出但属于用户 UI 状态；执行时只发送路径快照 |

---

## 5. 命令目录与调用合同

### 5.1 目录描述与执行定义分离

目录下发给 WPF 的是不可执行的 `CommandDescriptor`；真正 handler 不序列化为任意 JSON 对象。

```jsonc
{
  "id": "prism.terminal.here",
  "title": "在此打开终端",
  "subtitle": "在当前目录打开 Windows Terminal",
  "icon_glyph": "",
  "owner": "broker",                 // ui | broker
  "trust": "builtin",                // builtin | user
  "keywords": ["term", "终端"],
  "input": {
    "kind": "none",                  // none | text（v1）
    "required": false,
    "prompt": ""
  },
  "bindings": {
    "root_search": { "input": "current_folder", "requires_host_root": true },
    "keyword": { "input": "current_folder_or_prompt" },
    "action_panel": {
      "input": "selection",
      "cardinality": "one",
      "target_kinds": ["directory"]
    },
    "staging": null,
    "window_shortcut": { "input": "current_folder_or_prompt" }
  },
  "danger": "normal",                // normal | confirm | destructive
  "enabled": true
}
```

内置实现采用编译期表：

```text
UI handlers:     command id -> exhaustive WPF handler
Broker handlers: command id -> exhaustive Rust handler
```

用户命令另存受限定义，不能指定内置 handler id：

```jsonc
{
  "id": "user.<uuid>",
  "descriptor": { "...": "受校验的展示和触发字段" },
  "execution": {
    "kind": "open_url",              // v1 仅 open_url | launch_program
    "template": "https://example.com/?q={query|url}"
  }
}
```

`system`、`file_op`、`settings_page`、`reindex`、`exit` 等只能由内置命令 id 访问，导入 JSON 无法获得这些能力。

`bindings` 必须按 surface 分别声明输入来源，而不是用一组全局 `accepts`。例如“在此打开终端”可在根搜索使用 `current_folder`，在动作面板使用选中的 directory；没有宿主目录时，后者仍然可用。

当前 Search 只携带 query/root/mode/filters，而 `root` 是文件搜索范围，不等于宿主当前目录：用户切回全局范围后 root 会清空，但本次 summon 的 `HostContext` 仍可能有效。K0 因此新增能力门控的 `SearchCommandContext { current_folder, host_kind, host_capabilities }`，由 WPF 从**本次 summon** 的 HostContext 构造；它只用于命令适用性，绝不改变文件搜索 root。broker 根搜索只评估 `none` 或该 context 能满足的 binding；依赖 selection/staging 的命令不会进入根搜索 lane。

### 5.2 命令结果身份

当前 Rust `SearchResult.target` 是必填 `ActionTarget`，不能把 command id 填进 file target，也不能依赖 C# 的未知 kind legacy fallback。K0 明确新增：

```jsonc
{
  "kind": "command",
  "target": { "kind": "command", "value": "prism.settings.open" }
}
```

- Rust `TargetKind` 增加 `Command`，只接受规范化 command id（`prism.*` 或 `user.<uuid>`，总长 ≤128 字节）；它表示调用身份，不是文件系统资源。
- `Execute`、`Reveal`、`Actions`、`RunAction` 和 `ShellExecutor` 必须显式拒绝 `TargetKind::Command`；命令只能进入 `ExecuteCommand`。
- WPF 在 `ExecuteSelectedCoreAsync` 的通用 `_pipe.ExecuteAsync` 之前截获 `kind=command`，按目录 owner 分派。typed target 缺失或 catalog generation 不一致时拒绝执行并刷新目录。
- capability gating 仍是第一道保护；严格路由是第二道保护。即使 UI 分支遗漏，broker 也不能把 command id 当路径或 URL 执行。

### 5.3 CommandInvocationContext

`ActionTarget` 继续表示**单个可操作目标或命令身份**，但数组和宿主状态不塞进其 `value`。新增调用合同：

```jsonc
{
  "command_id": "prism.terminal.here",
  "source": "action_panel",           // root | keyword | action_panel | staging | shortcut
  "arguments": {
    "text": null,
    "destination": null,
    "output_path": null
  },
  "selection": {
    "target": { "kind": "directory", "value": "D:\\work" },
    "title": "work",
    "subtitle": "D:\\work"
  },
  "staged_paths": [],
  "current_folder": "D:\\work",
  "host_kind": "explorer"
}
```

合同规则：

1. `arguments` 是有界结构化参数，不把 ZIP 输出路径或 move 目标目录塞进自由文本。每个 handler 只读取 descriptor 声明的字段；未知/多余字段拒绝。
2. `selection.target` 仍由 broker 按现有 `ActionTarget::validate` 和 handler 所需类型复核；`title/subtitle` 只是有界 UI 快照，不可作为路径或权限依据。
3. `staged_paths` 是执行瞬间从 `StagingArea.Items` 复制的数组。运行时可能因“未标记项 + 128 项工作集”超过 128，超限必须整体拒绝并要求用户缩减/拆批，禁止静默截断。
4. broker 入站单行硬上限是 1 MiB。Invocation 除字段上限外再设 **512 KiB UTF-8 JSON 聚合上限**；命令 id ≤128 字节、text ≤8 KiB、staged ≤128 项、单路径沿用现有上限。客户端在发送前检查，broker 解码后复核。
5. 本地路径校验放在 `spawn_blocking`；但它不能取消卡死的 UNC/离线路径访问。K2 首版 `copy_paths` 可复制原始字符串而不探测存在性，ZIP/mutation 只接受本地路径；UNC/网络路径明确禁用并提示，待有独立的可取消探测策略后再开放。
6. `current_folder` 来自本次 summon 的 `HostContext`。检测失败必须为空，绝不沿用上次目录；broker 再校验绝对路径。
7. 应用 target 保留原始 `.lnk`/exe 值。handler 按语义选择 `raw_target` 或 `resolved_executable`，不做全局改写。
8. v1 不自动读取剪贴板。若以后加入 `{clipboard}`，由 broker 按命令权限即读即弃；默认关闭且不写日志/历史。

### 5.4 不使用隐式参数优先级

v2 的“键入 > selection/staged > clipboard > default”会让同一命令因现场状态不同而悄悄改变含义。v3 按调用来源确定参数：

| 来源 | 主要输入 | 缺失时行为 |
|---|---|---|
| keyword/root | `arguments.text` 或 surface 绑定的 `current_folder` | required 时进入参数提示，不执行 |
| action_panel | `selection` | 类型不匹配则不展示；执行前复核失败则保留 UI 并报错 |
| staging | `staged_paths` | 空暂存区时禁用；部分无效先汇总提示，不静默跳过 |
| shortcut | 描述声明的输入 | 缺 target/text 时呼出 Prism 进入参数态，不猜剪贴板 |

同一命令可以声明多个 surface，但每个 surface 都有明确 adapter；“注册即全通”不是无条件把每个命令暴露到所有入口。

---

## 6. 路由、搜索合并和缓存

### 6.1 QueryRouter

新建 WPF 纯函数 `QueryRouter.Route(rawQuery, catalogSnapshot, webEngines, hostContext)`，由 `SearchViewModel.RunSearchAsync` 在真正发请求前调用。`OnQueryChanged` 只负责重命名/动作过滤、即时 mode 投影和防抖。

路由顺序：

```text
0. Rename/Actions UI state 捕获（现有，Router 外）
1. 首字符 `>`                         -> WindowIntent
2. 明显 URL / localhost / www         -> DirectUrlIntent
3. 网页引擎 keyword + 空白             -> WebIntent
4. 命令 keyword + 空白                 -> CommandIntent
5. 空输入 + 有 host root               -> ScopedRecentIntent
6. 空输入 + 无 host root               -> Idle
7. 其他                               -> NormalSearchIntent
```

规则：

- 网页引擎关键字与命令关键字共享一个**独占路由命名空间**，保存时由 broker 查重；同名拒绝，不依赖列表顺序碰运气。
- 关键字必须是首个 token 且后跟空白；单独输入 `g`、`term` 仍可搜本地文件。
- 别名保持整词精确语义，可与“关键字+空白”语法技术上共存；设置 UI 应提示潜在认知冲突。
- `ext:/path:` 仍只由 broker 解析。带过滤 token 的普通搜索不混入命令行；显式命令关键字已在进 broker 前进入 CommandIntent。

### 6.2 根搜索命令 lane

普通搜索中 broker 从内存目录生成少量命令候选，再与现有 `ranked` 结果合并。不要伪造 `MatchMetadata` 后直接加入现有 comparator；现有 comparator 已有“query pick 超过匹配质量、class 再比较拼音 kind”的稳定合同。

v1 合并规则：

1. 文件/app/alias 按现有算法完整排序；
2. 命令仅接受标题或关键字的 exact/prefix 强匹配，暂不做宽松拼音/编辑距离；
3. alias 显式意图仍保持置顶；
4. 普通命令候选最多 2 条，插在核心结果的 whole-name exact 命中之后、弱 contains 命中之前；
5. 命令 frecency 只在命令 lane 内排序，不跨越文件 query-pick；
6. root binding 关闭、依赖 selection/staging、`SearchCommandContext` 不具备所需 current folder/capability，或 danger=`destructive` 的命令不进根搜索。文件搜索 `root` 不能替代 command context。

上线后再依据真实数据调整，不在 K0 重写统一评分公式。

### 6.3 缓存合同

新增 `Results.cacheable`（默认 true）和 `command_catalog_generation`（可选）：

- K1 中只要响应含命令行，broker 返回 `cacheable=false`，WPF 不把它作为 `_completeCache` 来源；
- 不含命令行的响应保持现有缓存资格；
- 目录变更后 generation 增加。前端在下一次响应发现变化时显式拉 `CommandList`，不得接收异步推送；
- 后续若实现命令感知的本地过滤，必须同时把 catalog generation 纳入 `SearchCacheEntry` 身份，并补别名、filter、窗口、网页回归测试。

### 6.4 参数态与 staging picker 状态机

现有 `PanelMode` 只有 Idle/Results/Actions，K1 必须显式增加 `CommandInput`，K2 再增加 `CommandPicker`（也可复用同一 command overlay，但状态必须可区分）：

```text
Results --Enter(required command)--> CommandInput --Enter--> Execute
   ^                                   | Esc/Back
   └───────────────────────────────────┘

Staging toolbar --> CommandPicker --> CommandInput(若需参数) / Execute
                         | Esc
                         └-----------> Results/原状态
```

状态至少保存 `ActiveCommandId`、`InvocationSource`、`ReturnMode`、原查询、结构化 arguments 和 selection/staged 快照。进入参数态后普通搜索、防抖、网页联想和 action filter 全部暂停；退出时恢复原查询和选中行。快捷键缺参数时先呼出窗口，再进入同一状态机，不另写一套流程。

### 6.5 WPF 接入清单（防止命令 id 被当路径）

- `ExecuteSelectedCoreAsync`：在通用 Execute 前截获 command；
- `RevealSelectedAsync`/Ctrl+Enter：command 直接禁用；
- `ResultList` 图标：command 使用 descriptor glyph/image，不把 `ExecuteId` 交给 Shell 图标 API；
- `EnterActionsAsync`：K2 从 file/folder 放宽到实际有动作的 application，但不默认开放 window/web；
- 右键菜单和 `RunActionOnCoreAsync`：按 `invocation_kind` 分派，command 不进入 rename/picker/delete 的 `ActionId` 硬编码分支；
- 行去重/选中保持：command 使用 `command:<id>` 稳定 RowKey，并把 catalog generation 纳入过期判断；
- 所有入口复用 `_actionInFlight` 或独立同等级 command guard，不能双按执行两次。

---

## 7. 动作面板与快捷键

### 7.1 不迁移现有 ActionId

K0/K1 保留：

```text
Actions(target) -> actions.rs::allowed_actions -> ActionItem[]
RunAction(target, ActionId, ActionArgs) -> Shell STA / IFileOperation
```

K2 新增 `ActionComposer`：

```text
现有内置 ActionItem[]
    + 对 target kind/context 适用的 CommandDescriptor[]
    -> 分段后的动作面板
```

`ActionItem` 以加法字段区分执行类型：

```jsonc
{
  "id": "command:prism.open.with_vscode",
  "label": "用 VS Code 打开",
  "invocation_kind": "command",       // builtin_action | command
  "command_id": "prism.open.with_vscode",
  "is_enabled": true,
  "disabled_reason": null
}
```

- `builtin_action` 继续发 `RunAction`；`command` 发 `ExecuteCommand`。
- broker 在列出和执行两处都校验 applies-to、cardinality 和 target。
- WPF 继续负责目录选择、重命名编辑和必须前台展示的交互；CommandDescriptor 不能绕过这些子流程。
- 首批命令动作只接 file/directory/application。窗口/Web 动作等 `SelectedItemContext` 和 UI handler 回归充分后再开。

### 7.2 快捷键分层

| 类型 | 当前/计划 | 说明 |
|---|---|---|
| `ActionHotkeys` | 保留 | 搜索窗可见且有选中结果时，对 target 执行现有内置动作 |
| Command window shortcuts | K2 新增 | 搜索窗内直接调用命令；缺参数时进入参数态 |
| Global command hotkeys | K3 评估 | 需要 RegisterHotKey 生命周期、冲突提示、重连目录刷新和设置 UI，不与 K1 捆绑 |

命令绑定存 broker 管理的命令文件并通过目录快照下发，避免旧 WPF 保存 `settings.json` 时丢弃未知字段。

---

## 8. 暂存区联动

### 8.1 数据边界

暂存区继续保存路径，不迁移持久化 schema。执行时：

```text
StagingArea.Items
  -> UI 线程快照 staged_paths
  -> ExecuteCommand
  -> broker spawn_blocking 校验/分类
  -> handler 接收 Vec<ActionTarget>
```

这既保留工作集的路径归档语义，也避免在 UI 热路径做 `File.Exists/Directory.Exists`。

### 8.2 首批批量命令

| 命令 | 首批 | 原因 |
|---|---|---|
| 复制全部路径 | 是 | 无损、可一次生成文本、容易定义成功语义 |
| 压缩暂存区为 ZIP | 是，但需新批量 API和输出选择 | 当前 `zip` 是单 target；必须定义输出文件名、冲突和部分失效行为 |
| 移动全部到… | 延后到 K2 后半 | mutation 超时可能“结果未知”，还要处理部分成功、同名冲突和 DestinationPicker |
| 永久删除/回收站 | 首批不做 | 危险操作、确认窗口和批量失败语义复杂 |
| 对每项启动外部程序 | 用户命令阶段再做 | 需要并发上限、窗口数量保护和逐项失败汇总 |

### 8.3 批量错误合同

- 校验阶段发现无效/不存在路径：不静默过滤，先显示“有效 N、失效 M”，由用户确认继续或取消；
- 聚合系统 API 返回逐项结果时，响应包含成功、失败、取消数量和有界错误摘要；
- mutation 超时继续使用“结果未知”文案，禁止自动重试；
- 模板逐项执行设置并发/数量上限，首版串行，结束后汇总；
- 暂存区内容不因命令执行成功自动清空，除非未来命令明确声明并二次确认。

---

## 9. 存储、协议与兼容

### 9.1 存储

| 文件 | owner | 内容 | 兼容策略 |
|---|---|---|---|
| `commands-v1.json` | broker | 用户命令、命令 surface 开关和命令快捷键 | 新文件；原子替换、损坏隔离、写入串行锁 |
| `command-usage-v1.json` | broker | 仅 command id、成功次数、最近成功时间和 frecency；v1 不保存参数/query | 与 `history-v2.json` 分离，旧 broker 回滚不受影响 |
| `history-v2.json` | broker | 现有文件/目录/应用/窗口历史 | **不改 schema，不加 command kind** |
| `settings.json` | WPF/broker 现有共享 | 现有应用设置、网页引擎等 | K0 不加入命令字段，避免旧 WPF 全量保存时丢字段 |
| `staging.json` | WPF | 路径和工作集 | 不改 schema |

新增持久化文件时，同阶段更新 `persistence.rs` 的 schema 常量、数据目录文档和 `dist/prism.iss` 的保留/卸载策略。`command-usage` 必须受现有 `HistoryEnabled` 控制；关闭历史时不落盘，`ClearHistory` 同时清除 `history-v2` 和 `command-usage-v1`。UI 的“使用历史已清除”只有两者都成功才算完成。

### 9.2 能力协商

扩展 hello，不立即提升 broker 协议号：

```jsonc
// 新 WPF -> broker；旧 broker 会忽略未知字段
{ "type": "hello", "protocol": 1, "capabilities": ["commands_v1"] }

// 新 broker -> WPF；旧 WPF 忽略新增字段
{
  "type": "hello",
  "protocol": 1,
  "features": ["commands_v1"],
  "command_catalog_generation": 3
}
```

每条管道连接独立握手并保存连接能力。hello 同时返回 `build_id`，客户端还记录服务端 PID。兼容矩阵：

| 组合 | 行为 |
|---|---|
| 旧 WPF + 新 broker | 未声明能力，broker 不返回 command 行、不返回命令 ActionItem；旧行为不变 |
| 新 WPF + 旧 broker | hello 无 feature，WPF 隐藏命令 UI，不发送新请求；旧行为不变 |
| 新 WPF + 新 broker | 双方能力交集包含 `commands_v1` 后启用 |
| 两边能力声明不一致 | 功能关闭并记录非敏感诊断，不猜测兼容 |

动作通道保持懒连接，但执行 broker-owned 命令前必须确认其 server PID/build_id/features 与产生结果的查询通道一致，且 action catalog generation 不旧于结果 generation；不一致时刷新目录并要求用户重试。UI-owned 命令执行前也要确认当前 catalog generation 仍包含同 id/owner，不能依据旧行直接本地执行。

如果实现中发现 serde/握手状态难以可靠维护，则备选方案是 broker 协议整体升 v2并拒绝混用；不得退回“旧前端会忽略未知行”的假设。

### 9.3 IPC 增量

```rust
// Hello：capabilities/features + command_catalog_generation

// Search additive context（仅 commands_v1 连接发送）
Search { ..., command_context: Option<SearchCommandContext> }

// Query channel
CommandList
CommandSet { definition }
CommandDelete { id }
CommandSetEnabled { id, enabled }
ValidateTriggerNamespace { proposed_web_engines, proposed_command }
RecordCommandOutcome { id, source, outcome } // UI handler 成功/取消/失败回执

// Action channel
ExecuteCommand { invocation: CommandInvocationContext }

// Response
CommandCatalog { generation, items }
CommandApplied { generation, message }
CommandOutcomeRecorded
CommandResult { status, message, batch_summary }

// Search/Action additive fields
SearchResultKind::Command                 // 仅 capability-gated 连接可收到
Results { cacheable, command_catalog_generation, ... }
ActionItem { invocation_kind, command_id, is_enabled, disabled_reason, ... }
```

不增加 unsolicited push。所有 mutation 直接返回新 generation；外部文件变更最多在下一次请求时被发现。UI handler 只有成功后才发送并等待配对的 `RecordCommandOutcome`；失败/取消不增加 frecency。`prism.exit` 默认 `record_usage=false`，避免“退出前预记成功/退出后无法回执”的二义性。

---

## 10. 安全模型

### 10.1 信任等级

| 等级 | 来源 | 能力 |
|---|---|---|
| Builtin UI | 编译进 WPF | 仅已注册 UI handler；未知 id 拒绝 |
| Builtin broker | 编译进 broker | 可调用受控 Shell/系统能力；危险命令带确认策略 |
| User | 用户创建/导入 | v1 仅 `open_url`、`launch_program`，无内置特权 handler |

### 10.2 用户命令执行限制

1. `launch_program` 本质上允许用户以当前普通用户权限启动任意程序，属于**明确授权的代码执行功能**，不是“安全数据解释”。它只承诺不提权、不暴露内置特权 handler、不直接接受 Shell 字符串。
2. 目标必须是本地存在的绝对 `.exe` 或 `.lnk`；v1 拒绝 UNC/网络目标和直接 `.bat/.cmd/.ps1`。不得用“屏蔽 cmd/PowerShell/LOLBins”宣传成安全沙箱——解释器可被重命名，无法可靠枚举。
3. 参数以 token 模板保存，逐 token 展开后由单一 Windows quoting 实现生成参数串；不接收完整原始命令行。`.lnk` 仍可能自带目标、参数和工作目录，导入/启用页必须解析并展示这些真实信息。
4. `open_url` 只允许 `http/https`，`{query}` 默认 URL 编码；仅显式安全字段允许 `raw`，设置页实时预览最终 URL。
5. 工作目录必须是存在的本地绝对目录；缺失上下文时命令禁用并提示，不传空字符串继续执行。
6. 用户命令不能请求 `runas`、关机、重启、文件删除、索引重建、设置写入和任意 COM verb；这些只存在于内置表。
7. 导入命令默认 `enabled=false`，按“运行程序/代码”的风险等级展示标题、解析后目标、内嵌参数、工作目录和网络行为，再由用户启用。
8. 命令 id、关键字、模板、参数、路径和目录总数全部有界；保存前校验，执行时再校验。
9. 查询文本、展开参数、剪贴板和暂存路径不写普通日志；日志仅记 command id、source、结果类别和脱敏错误 id。
10. danger=`confirm/destructive` 的内置命令不能出现在普通根搜索建议中；执行前由对应 owner 显式确认。

---

## 11. 现有功能接入策略

| 功能 | v3 定位 | 实施动作 |
|---|---|---|
| 文件/目录/应用搜索 | 核心 Provider | 不迁移；只在 broker 最终合并处加入命令 lane |
| 16 个 `ActionId` | 稳定内置动作 | 不迁移；K2 由 ActionComposer 追加命令动作 |
| 别名 | Query accelerator | 不迁为命令；纳入关键字冲突提示和缓存回归 |
| `ext:/path:` | broker 查询语法 | 不迁；过滤态禁止根命令混排 |
| 窗口 `>` | 专用 Provider + UI activation | 不迁；未来 UI 命令必须尊重先激活后隐藏合同 |
| 网页引擎 | 专用 Command-family adapter | 保留检测/联想；K3 可由 catalog 统一关键字元数据和冲突校验，不删除专用 handler |
| 裸 URL | Router 内建 intent | 保留现有 TLD/localhost 规则 |
| 暂存区 | Context source | K2 增加 staging surface 和批量 handler，不改工作集存储 |
| 工作集召回 | 前端适配器 | 保留现有合成行；以后可注册“载入工作集”UI 命令，但不先建通用 list 栈 |
| 动作快捷键 | Target-bound shortcut | 保留；命令快捷键另建 |

---

## 12. 分阶段路线图

### K0：兼容地基（3–5 天）

**范围**

- hello capabilities/features；两条连接都记录能力；
- Rust/C# `CommandDescriptor`、`CommandInvocationContext`、`SearchCommandContext`、目录 generation；
- broker 内置目录骨架、`CommandList`；
- `commands-v1.json`、`command-usage-v1.json` 的 envelope、校验、原子持久化和损坏隔离；
- capability-gated `SearchResultKind::Command`；
- `Results.cacheable`；
- UI/Broker handler registry 骨架，但不发布危险命令；
- 不改 `history-v2`、`ActionId`、网页模式和暂存区。

**验收门**

- 旧 WPF/新 broker、新 WPF/旧 broker、新/新三组自动化或管道夹具通过；
- 未协商能力时搜索 JSON 不出现 command；
- 目录读写往返、future schema 拒绝、损坏文件隔离、并发 mutation 不撕裂；
- Rust tests、C# tests、clippy `-D warnings`、Release build 通过。

### K1：最小可用命令（1–2 周）

**范围**

- `QueryRouter` 和 command keyword 参数态；
- broker 根搜索强匹配命令 lane，最多 2 行；
- command target 的严格分派、`ExecuteCommand` 双执行器、在飞守卫和类型化错误；
- `CommandInput` 参数态、UI handler outcome 回执、命令 usage/frecency；
- 首批内置命令：
  - `prism.settings.open`（UI）；
  - `prism.exit`（UI，默认不记录 usage）；
  - `prism.system.lock`（broker，安全系统动作）；
  - `prism.terminal.here`（broker，根搜索用 current folder、动作面板用目录 target）；
- 当前托盘“重建索引”只是占位提示，indexer IPC 也拒绝 rebuild；`prism.index.rebuild` 不进入 K1，待单独设计受控服务管理/提权流程；
- 参数缺失时提示，不执行；危险 shutdown/reboot 暂不上线。

**验收门**

- Router precedence：window、URL、web、command、filter、empty 全覆盖；
- current folder 只来自本次 summon 的 HostContext；切换全局搜索后仍与 search root 独立，检测失败时清空且不复用旧值；
- command 响应不进入不安全前缀缓存；
- 双按 Enter 只执行一次；UI handler 未注册/目录不一致时显式 unavailable；
- P95 搜索和三进程内存门不回归。

### K2：动作与暂存联动（1–2 周）

**范围**

- `ActionComposer`，保留内置段并增加命令段；命令段 frecency 排序、cap 5；
- file/directory/application 命令动作；
- Command window shortcuts（不复用 `ActionHotkeys`）；
- staging surface；`copy_paths` 和真正的多目标 ZIP；
- 批量校验、部分失败汇总、mutation 结果未知合同；
- 根据验证结果再决定是否加入批量 `move_to`。

**验收门**

- `.lnk` raw/resolved 语义测试；
- 动作列出和执行都做 target 重校验；
- 暂存路径失效、运行时超过 128 项、512 KiB 聚合上限、UNC 明确拒绝、空暂存和部分失败覆盖；
- 现有重命名、目录选择、永久删除、右键菜单和动作快捷键回归全绿。

### K3：用户命令与网页目录适配（1–2 周）

**范围**

- 用户命令设置页，`open_url`/`launch_program` 安全表单；
- 关键字查重、模板预览、导入导出、导入默认禁用；
- 网页引擎保存前调用 broker `ValidateTriggerNamespace`，通过后才写 settings 并更新 WPF；启动时若 settings 与 commands 文件仍冲突，确定性采用“网页关键字优先、冲突命令 keyword binding 自动禁用并报诊断”，不依赖保存顺序；
- 网页引擎以 adapter 进入统一目录和关键字冲突校验，保留现有联想/favicons；
- Fallback 行；
- 评估全局 command hotkeys，不作为本阶段强制验收项。

**验收门**

- 预览和实际 URL/参数展开一致；
- 导入导出往返无损，恶意/超界/脚本目标全部拒绝；
- 网页直达行、联想取消、800ms 超时、隐私默认和 broker 兜底回归；
- 旧设置页保存不会删除命令数据。

### K4：独立立项（未承诺）

- 查询型 command view / `list -> Item[]` 导航栈；
- notes、OCR、翻译等具体功能；
- clipboard context；
- 外部扩展进程和 stdio JSON-RPC；
- 类型化多参数、命令链和全局热键完善。

K0 → K1 严格依赖；K2 与 K3 在 K1 后可部分并行。每阶段应能通过 feature capability 或目录开关关闭新命令，不回滚搜索/动作基础设施。

---

## 13. 关键端到端数据流

### 13.1 根搜索执行 UI 命令

```text
输入“设置”
-> Router: NormalSearch
-> broker 合并文件/app/alias + prism.settings.open
-> SearchResult(kind=command, target=command id)
-> WPF 查 catalog owner=ui
-> 构造 source=root 的 InvocationContext
-> UI handler registry 执行
-> UI handler 成功后发送配对的 RecordCommandOutcome（exit 除外）
```

### 13.2 对目录执行“在此打开终端”

```text
选中 folder 行 -> 动作面板
-> Actions 返回现有文件动作 + command action
-> WPF 选择 command action
-> selection.target + current_folder 快照
-> ExecuteCommand（action channel）
-> broker 复核 target/current_folder
-> open_terminal handler
-> 返回成功/类型化错误
```

### 13.3 暂存区压缩

```text
点击“对暂存区执行”
-> WPF 快照 staged_paths
-> 选择 ZIP 输出位置（前台 UI 子流程）
-> 写入 arguments.output_path
-> ExecuteCommand
-> broker 后台校验/分类全部路径
-> 批量 ZIP handler
-> 返回 success/failure/cancelled 汇总
-> WPF 保留暂存区，显示结果
```

---

## 14. 主要风险与决策

| # | 风险 | v3 决策 |
|---|---|---|
| R1 | 命令行干扰文件排序 | 强匹配、lane 合并、cap 2、命令 frecency 不跨文件 pick |
| R2 | 新 broker 给旧前端返回未知行 | hello capability gating；不依赖 Unknown 映射 |
| R3 | 目录推送打乱管道配对 | 禁止 push；generation + 显式 list |
| R4 | 命令行破坏前缀缓存 | K1 含命令响应 `cacheable=false`；后续再做 generation-aware cache |
| R5 | `launch_program` 就是普通用户权限代码执行 | 明确风险与授权；无 runas/内置特权/原始 Shell 串，导入默认禁用并展示 `.lnk` 真实目标和参数 |
| R6 | 上下文偷偷选错来源 | InvocationSource 决定输入；缺失就提示，不做隐式 fallback |
| R7 | 应用 `.lnk` 参数被解析 exe 后丢失 | 默认保留 raw target，handler 显式声明是否解析真实 exe |
| R8 | 暂存区批量部分成功后自动重试 | 返回汇总；mutation 超时标“结果未知”；禁止自动重试 |
| R9 | 网页收编造成联想/隐私退化 | 只适配目录元数据，不删除 WebModeDetector/WebSearchCoordinator |
| R10 | settings 全量保存丢新字段 | 命令数据独立 broker 文件，不写入现有 settings schema |
| R11 | “注册即全通”暴露不合适入口 | 每个 surface 单独 binding；danger/context/cardinality 决定可达性 |
| R12 | 内核被 note/list/plugin 提前拖重 | K4 前不建设通用页面栈、KV 平台或外部扩展 |
| R13 | 双管道连接到不同 broker/目录代际 | 执行前比较 PID/build_id/features/catalog generation；不一致则刷新并重试 |
| R14 | 命令 usage 绕过隐私设置 | v1 不存参数/query；遵守 HistoryEnabled，ClearHistory 同时清两份历史 |

---

## 15. 最终评价

修订后方案具备实施条件，并与仓库现状相容：

- **可行的核心**：可搜索命令目录、关键字参数态、分层 handler、命令使用记录、动作适配和暂存区上下文；
- **必须保留的特例**：窗口激活在 WPF、网页联想专用编排、别名精确置顶、工作集前端状态、现有 `ActionId` 和 mutation 子流程；
- **最重要的工程改进**：能力协商、显式 InvocationContext、命令独立持久化、禁止 IPC 推送、用户命令最小权限；
- **最合适的 MVP**：先完成 K0/K1，证明“目录—搜索—参数—双执行器—兼容”闭环，再进入动作面板和暂存区批量。不要把 note、list 回流、用户脚本、插件或命令链放进第一版。

该方案不追求把所有功能改写成同一种机制，而是让各功能在保持正确边界的前提下共享一套发现和调用内核。这比 v2 的“全集成即全部收编”更可维护，也更符合 Prism 当前代码已经形成的进程、排序、缓存和 Windows 权限现实。

---

## 16. 代码依据与参考资料

### 当前仓库依据

- broker 协议/搜索/合并：`src/prism-core/src/ipc.rs`
- 动作 allowlist：`src/prism-core/src/actions.rs`
- Shell/STA/target 校验：`src/prism-core/src/shell.rs`
- 历史与 schema：`src/prism-core/src/history.rs`、`src/prism-core/src/persistence.rs`
- 网页引擎：`src/prism-core/src/websearch.rs`
- 别名：`src/prism-core/src/alias.rs`
- 窗口枚举：`src/prism-core/src/window_list.rs`
- 前端搜索状态机：`src/Prism/ViewModels/SearchViewModel.cs`
- 前端结果/动作模型：`src/Prism/Models/SearchResult.cs`、`src/Prism/Models/ActionItem.cs`
- 网页专用模式：`src/Prism/Services/WebModeDetector.cs`、`src/Prism/Services/SuggestionService.cs`
- IPC 双通道：`src/Prism/Services/PipeClient.cs`
- 动作快捷键：`src/Prism/Models/ActionHotkeyCatalog.cs`、`src/Prism/Services/ActionHotkeyTable.cs`
- 暂存区/工作集：`src/Prism/Models/StagingArea.cs`、`src/Prism/Services/StagingStore.cs`

### 外部参考

- Raycast Manifest / Arguments / Dynamic Placeholders / 搜索排序
- PowerToys Command Palette extensibility overview / samples
- Flow Launcher result-command 与 JSON-RPC 扩展模型
- uTools 上下文与 redirect 思路
- 用户提供的《Listary 命令系统与新一代启动器调研报告》
