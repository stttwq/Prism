# Prism 统一命令系统设计（Unified Command Kernel）

> **文档性质**：设计方案，待批准后实施。
> 日期：2026-08-22（v2，重定位为统一内核方案，取代同日 v1"命令层"方案）
> 输入：①《Listary 命令系统与新一代启动器调研报告》（用户提供）；② 联网补充调研（PowerToys Command Palette v2 扩展模型、Raycast Manifest/Arguments 规范）；③ 代码实测（§2，全部以当前 feature 分支工作树为准；历史文档仅作线索，其中 `POST-ROADMAP-REVISED.md` §2"不支持 JSON 命令"的范围锁定被本文取代）。
>
> **四个目标**（产品诉求原文归纳）：
> 1. **全集成**——现有各功能（搜索/应用/窗口/网页/别名/动作/暂存区）收进一个系统；
> 2. **新功能即插**——加功能 = 在系统上新开一个入口，不再各建各的机制；
> 3. **子系统联动**——像"拖拽 × 暂存区"那样，命令与其他子系统数据互通；
> 4. **做到最好**——对标报告全部产品，取各家最强项并形成 Prism 独有优势。

---

## 1. 设计原则

| # | 原则 | 来源 |
|---|---|---|
| P1 | **一个系统**：一切用户可交互项收进统一模型，一种路由、一套排序、一张动作表 | Raycast 7 类来源平级混排；Listary 4 类对象割裂是反面教材 |
| P2 | **注册即全通**：新功能声明一次元数据 + 一个 handler，自动获得全平台能力（§7） | Raycast 扩展写一次自动获得 12 种触发 |
| P3 | **数据可流动**：子系统之间通过类型化上下文通道传数据，不复制粘贴机制 | 拖拽 × 暂存区先例；uTools redirect、Raycast launchCommand 的行业验证 |
| P4 | **薄**：不加进程、不加常驻大头、不加网络；broker 不因命令变重 | Prism"安静且能成长"定位 |
| P5 | **副作用跟进程走**：无副作用前端闭环，碰文件/系统走 broker；indexer 永远只读 | 三进程权限分离现状 |
| P6 | **协议与存储只做加法**：新字段带默认、未知枚举容忍、schema 版本化 | 现有 IPC/持久化家规 |

**明确不做**：DLL/进程内插件与插件市场（M4 外部扩展仅为协议预留）；Shell 字符串执行器（`> ping` 式任意命令行透传）；命令链/可视化 workflow（Raycast launchCommand 级，YAGNI）；自然语言/AI 参数抽取（网络与密钥依赖，违背"薄"）。

---

## 2. 现状盘点（代码实测，2026-08-22）

三进程：`Prism.exe`（WPF）↔ 管道 `\\.\pipe\prism-core` ↔ `prism-core.exe`（broker，独占 Shell/COM/剪贴板/启动）↔ `\\.\pipe\prism-indexer-v1` ↔ `prism-indexer-service.exe`（LocalSystem，只读索引）。

现状是**6 套并行机制**，彼此无数据通道：

| 机制 | 位置 | 触发 | 与其他机制的联动 |
|---|---|---|---|
| 文件/文件夹搜索 | broker `search_service` 合并（`ipc.rs:1697`） | 无前缀 | 无 |
| 应用搜索 | broker `apps.rs` | 无前缀 | 无 |
| 窗口模式 | 前端 `SearchViewModel.cs:200`（`>`）+ broker `SearchMode::Window` | 首字符 `>` | 无（Window 项无动作） |
| 网页引擎 | broker `websearch.rs:93` + 前端 `WebModeDetector.cs` 镜像 | 首 token+空白 | 无（Web 项无动作） |
| 别名 | broker `alias.rs`（`aliases-v1.json`） | 整词精确 | 无 |
| 动作面板 | broker `actions.rs`（`ActionId` 封闭 16 项）+ `shell.rs::execute_run_action` | 选中 `→` 键/`ActionHotkeys` | 仅作用于文件类结果 |
| 暂存区/工作集 | 前端 `StagingArea.cs`/`StagingStore`（纯路径列表+标记） | Ctrl+D、拖拽 | **唯一联动先例**：搜索结果拖入暂存区、`workset` 行召回 |

**已有的结构红利**（统一的地基，大部分已存在）：
- `SearchResult`（`ipc.rs:296`）已是统一结果项：`kind + title + subtitle + execute_id + target: ActionTarget + match_spans`——Item 模型事实上已统一，缺的只是成员和通道；
- `ActionTarget{kind, value}` 是类型化执行契约，全链路通用——联动数据载体现成；
- 排序框架（kind → match class → history → query-pick，`ipc.rs::sort_search_results_with_picks`）可推广到全 kind；
- frecency 历史引擎、`VersionedEnvelope` 持久化、serde 加法演进协议、暂存区纯策略层——全是可复用件。

---

## 3. 统一模型：四原语

整个系统由四个原语构成，现有 6 套机制全部映射为四原语的组合：

```
                 ┌────────────────────────────────────────────┐
                 │              QueryRouter（前端）             │
                 │  输入解析：前缀 / 关键字 / 过滤 token / 裸词     │
                 └──────────────┬─────────────────────────────┘
                                │ 分发
      ┌──────────┬──────────┬──┴───────┬──────────┬──────────┐
      │ 文件      │ 应用      │ 窗口      │ 网页      │ 命令      │  …Provider（数据来源，§3.1）
      │(broker)  │(broker)  │(前端+broker)│(收编)    │(目录快照) │   暂存区/别名同为成员
      └──────────┴──────────┴──────────┴──────────┴──────────┘
                                │ 产出
                        Item[]（统一结果项）
                                │ 统一排序（现有框架推广）
                        结果列表（一个框接住一切）
                                │ 用户选择
                 ┌──────────────┴────────────────────────────┐
                 │  Action 分发：Enter=默认动作 │ →=动作面板     │
                 │            │ 命令=注册为动作的 Command       │
                 └──────────────┬─────────────────────────────┘
                                │ 执行（ui handler / broker handler）
                 ┌──────────────┴────────────────────────────┐
                 │  Context 通道（§5）：selection / staged /    │
                 │  clipboard / current_folder                │
                 └───────────────────────────────────────────┘
```

### 3.1 Provider（来源）——回答"东西从哪来"

Provider 不是新插件接口，是**静态分发表 + 现有模块的规范化**：每个来源声明自己的路由条件（无前缀全局 / 专属前缀 / 关键字）与产出 Item 的方式（本地计算 or broker 请求）。迁移后各来源仍在原进程原模块，只是产出统一 Item、走统一路由、进统一排序：

| Provider | 数据落点 | 路由条件 | 迁移动作 |
|---|---|---|---|
| files | broker（indexer 搜索） | 无前缀 | 无（已是 Item） |
| apps | broker（Start Menu） | 无前缀 | 无 |
| folders | broker | 无前缀 | 无 |
| windows | 前端枚举 + broker 校验（现状） | `>` 前缀 | K2：Item 增配动作（§5.4） |
| web（引擎） | broker 目录（收编后） | 关键字+空白 | K2：收编为 `open_url` 命令（§8） |
| alias | broker | 整词精确快车道 | 不动（已是 Item；设置页归入"词与命令"分组） |
| commands | 目录快照（前端缓存，broker 权威） | 标题/关键字模糊 + 关键字+空白 | K0/K1 新建 |
| staging | 前端 `StagingStore` | `workset` 行 + Context 通道 | K2：接入通道 |

`ext:`/`path:` 不是 Provider，是 Router 的查询修饰符（保持现状语义）。

### 3.2 Item（统一结果项）——"一切皆条目"

`SearchResult` 扩展为完整 Item：

- `SearchResultKind` 增加 `Command` 变体（加法，旧前端按未知 kind 忽略——type-safety 规范已要求）；
- 窗口项、网页项从"无动作"升级为"按 kind 声明动作"（`allowed_actions` 从硬编码函数改为 ActionRegistry 查询，broker 仍逐 target 校验）。

**结果即命令，命令即结果**（Flow Launcher 的核心洞察）：一个 Item 无论来自哪个 Provider，都能进动作面板、都能作为命令参数、都能写同一条 frecency 历史。这是"全集成"在数据层的含义。

### 3.3 Action（统一动作表）——"对条目做什么"

现状：`ActionId` 封闭 16 项，只有文件类结果有动作。统一后：

```
ActionRegistry（broker 权威 + 前端镜像缓存）
  内置动作：现有 16 项 ActionId 原样迁入（open_folder/copy/copy_path/zip/runas…）
  命令动作：任何声明了 applies_to 的命令自动注册为动作
    例：用户命令"用 VS Code 打开"（launch handler, applies_to: [file, directory]）
        → 自动出现在文件/文件夹结果的动作面板里
```

**一份定义，三种身份**：同一个命令 = 搜索结果行（根搜索/关键字触发）+ 动作面板项（对选中/暂存目标执行）+ 热键目标（`ActionHotkeys` 扩展可绑命令 id）。这是对 Listary"Command 与 Action 两张表单两套字段"割裂的直接解法，也是"最好方案"的核心体验卖点。

### 3.4 Command（命令目录）——"系统能做什么新事"

```jsonc
{
  "id": "prism.terminal.here",        // "prism.*" 内置 | "user.<uuid4>" 用户
  "title": "在此打开终端",
  "subtitle": "Windows Terminal 于当前目录",
  "icon": "",                          // Fluent 字形码或图片路径
  "keywords": ["term", "终端"],         // 多关键字（治 Listary 痛点 1）；与引擎关键字同一命名空间，写入查重
  "argument": { "name": "路径", "required": true, "default": "", "hint": "要打开的目录" },
  "executor": "broker",                // "ui" | "broker"（M4 预留 "extension"）
  "handler": { "kind": "open_terminal", "profile": "" },
                                       // broker: open_url|launch|open_terminal|system|file_op
                                       // ui:     calc|note|settings_page|list(回流型,§5.5)
  "applies_to": ["file", "directory"], // 可选：声明后自动注册为这些 kind 的动作面板项（§3.3）
  "contexts": ["explorer"],            // 可用上下文；依赖 {current_folder} 的命令自动绑 explorer
  "enabled": true,
  "show_in_root_search": true          // false = 仅关键字/热键/动作面板可达（降噪）
}
```

handler kind 两侧封闭枚举、`match` 遍历——加 kind 编译器强制补全，这是"易维护升级"的编译期保障。占位符与修饰符管线在 broker 单点实现：`{query}`、`{current_folder}`、`{clipboard}`（默认关、即刻消费不落盘）、`{selection}`、`{staged}`（§5）；修饰符 `url/upper/lower/trim/raw`，`open_url` 的 `{query}` 未显式 `|raw` 时强制 URL 编码——Listary 十年 `#`/`&` 破坏 URL 的 bug 结构上不存在。

---

## 4. 路由与触发

### 4.1 触发方式（渐进式复杂度）

| 层 | 方式 | 说明 |
|---|---|---|
| 零学习 | 根搜索混排 | 打"关机"/"终端"，命令按标题/关键字模糊混入（配额 §4.3） |
| 进阶 | 关键字快车道 | `vs E:\proj`：首 token 精确命中 + 空白 → 参数输入态（网页模式同款心智） |
| 高频 | 热键 | `ActionHotkeys` 扩展：热键直接绑命令 id |
| 目标向 | 动作面板 | 选中/暂存目标 → `→` 键 → 面板含命令动作（§3.3） |
| 兜底 | Fallback | 零结果（非索引中）追加兜底命令行，默认=引擎搜原词，可配置（CmdPal/Raycast 模式） |

**不做模式切换前缀**（Raycast 决策）：`>` 维持窗口模式专属；拒绝 PowerToys Run 式 18 个单字符前缀军团。**每命令多入口并存**是行业共识，也是本系统"注册即全通"的直接体现。

### 4.2 路由总顺序（单一事实源，落在 `SearchViewModel.OnQueryChanged`）

```
1. 重命名编辑捕获 / 动作面板过滤          （现有，不动）
2. `>` 窗口模式                          （现有，不动）
3. 空查询                               （现有，不动）
4. ui 命令关键字检测（前端本地表）          新增
5. 引擎/命令关键字检测                     K2 收编后统一为命令关键字表（前端持快照）
6. 裸 URL                              （现有，不动）
7. 普通搜索 → broker：
   files + apps + windows + alias + web行 + 命令行 全局合并排序   命令行由 broker 并入（web row 同款模式）
```

排序合并双落点保留现状（broker `search_service` 大合并 + 前端本地 ui 行并入），两侧共用同一排序数值表（写进协议文档，消除 `WebModeDetector.cs` 式双实现漂移——现有教训）。

### 4.3 排序与降噪（文件搜索仍是第一公民）

- 命令行复用现有排序框架：Literal（关键字/标题精确）→ 前缀 → 模糊，类内 frecency（history kind `command`，目标=命令 id）；
- **配额**：普通查询命令行最多 2 条且排在文件/应用精确命中之后；关键字/标题强匹配才置顶；
- `show_in_root_search` 关闭的命令只走关键字/热键/面板；设置总开关 `CommandSuggestionsEnabled`。

---

## 5. 联动设计（核心增量：Context 通道）

联动的本质：**命令执行时，参数除用户键入外，可来自四个类型化上下文源**。全部以现有 `ActionTarget` 为载体，零新序列化类型。

### 5.1 四个上下文源

| 源 | 值 | 何时可用 | 对标 |
|---|---|---|---|
| `{selection}` | 当前选中 Item 的 target（单个） | 结果列表有选中行 | Raycast `{selection}`/Show in Raycast |
| `{staged}` | 暂存区全部条目的 targets（数组，`StagingStore` 现有数据） | 暂存区非空 | **Prism 独有**（无对标产品有暂存区） |
| `{clipboard}` | 剪贴板文本（broker 即读即弃） | 总是 | Raycast `{clipboard}` |
| `{current_folder}` | 宿主当前目录（G4 识别链） | explorer/对话框上下文 | Listary `{current_folder}`（它只在 Explorer 生效且静默传空——Prism 缺失时命令不可用并提示，治痛点 3-②） |

### 5.2 参数解析顺序（声明在 CommandSpec，执行时按序取首个非空）

```
显式键入（参数态输入） > {selection}/{staged}（从面板/暂存进入） > {clipboard} > default
```

required 仍无值 → 显示参数提示，不执行（治 Listary 痛点 12 可选参数）。

### 5.3 暂存区 × 命令 = 批量动作（治 Listary 痛点 5）

暂存区工具栏新增"对暂存区执行"入口 → 列出 `applies_to` 匹配的命令 → 执行：

- **聚合型 handler**（`file_op`/`zip`/`copy_path` 类）：一次接收 targets 数组，单次系统调用（IFileOperation 天然批量）；
- **模板型 handler**（`launch`/`open_url` 类）：`{staged}` 展开为逐目标执行（"对 5 个文件各开一个 VS Code 窗口"）。

内置首发：`prism.staging.zip_all`（压缩暂存区为 ZIP）、`prism.staging.copy_paths`（复制全部路径）、`prism.staging.move_to`（全部移动到…，复用 DestinationPicker）。暂存区从"路径收纳盒"升级为"批处理工作台"——这是报告全部对标产品都没有的能力。

### 5.4 窗口/网页项获得动作（Item 统一的自然结果）

- 窗口项：切换（现有默认）+ 动作"复制窗口标题"、"最小化后切换"等轻动作（K2 起按需加，均前端可完成或经 broker）；
- 网页项：复制 URL、复制 Markdown 链接、二维码（远期）。动作注册表按 kind 查询，加动作=注册表加一行。

### 5.5 回流：命令产出 Items（list 型命令）

handler `mode: "instant" | "list"`（Raycast view/no-view 的对应物）：

- `instant`：执行即完成（现有一切命令）；
- `list`：返回 Item[] 推入结果列表继续交互（前端保留面包屑/Back 返回上级）。首发用户：`note list`（便签列表→逐条打开/复制）、`prism.workset.list`（工作集列表→载入）。这使 Prism 能承载"查询型"功能（未来的重复文件查找、磁盘分析都走这条路），而不只是"执行型"。

**命令链不做**（P6）：回流 + 上下文源已覆盖 90% 联动场景；跨命令函数调用等真实需求出现再立项。

---

## 6. 新功能开发模式（"在系统上新开"的实现样板）

以 OCR（未来方向 §D）为例，加一个新功能的全部工作量：

```
1. broker/前端 各加一个 handler kind 枚举值（ocr → ui）
2. 实现 handler：ui:ocr = 截屏选区 + WinRT OCR + 文本回填搜索框（~150 行，纯前端）
3. 注册 CommandSpec：{ id: "prism.ocr", title: "截屏识字", keywords: ["ocr","识字"], ... }
4. 完。
```

**自动获得**（不写一行代码）：根搜索混排与排序、关键字触发、热键绑定、frecency 历史、`{clipboard}`/`{staged}` 上下文、动作面板注册（若声明 applies_to）、设置开关、导入导出（若是用户命令）、文档位（目录快照自描述）。

对照现状：拖拽-暂存区联动是专门写的管道；此后的每个新功能（note/ocr/calc/颜色/翻译/hosts 编辑/重复文件…）都只是"声明 + 一个 handler"。这就是 P2"注册即全通"，也是"可持续发展"在工程上的含义——**功能增长是线性的，平台代码不随功能数增长**。

内置命令 v1 清单（K1 首发）：

| id | executor:handler | 说明 |
|---|---|---|
| `prism.calc` | ui:calc | 表达式自然检测出结果行（零前缀，对标 PTRun `=`） |
| `prism.terminal.here` | broker:open_terminal | `wt -d {current_folder}`，cmd 回退；context: explorer；applies_to: [directory] |
| `prism.system.shutdown` / `reboot` / `lock` / `sleep` / `empty_bin` | broker:system | Listary 内置命令对齐 |
| `prism.settings.open` / `reindex` | ui:settings_page | Listary `opt` 对齐 |
| `prism.exit` | ui | — |
| `prism.note.add` / `prism.note.list` | ui:note / ui:list 回流 | 承接未来方向 §C，验证 KV 通道与回流 |
| `prism.staging.zip_all` / `copy_paths` / `move_to` | broker:file_op | §5.3 批量首发 |

---

## 7. 数据、协议与安全

### 7.1 存储（VersionedEnvelope 家规）

| 文件 | schema | 内容 | 归属 |
|---|---|---|---|
| `commands-v1.json` | v1 | 用户命令（内置不落盘）+ 导入导出即此文件原样+`exported_at` | broker |
| `history-v2.json` | v2→按需 v3 | kind 增加 `"command"`（旧二进制容忍性 K0 实测，不确定则升版+迁移函数） | broker |
| `command-data-v1.json` | v1 | ui 命令数据 KV（note 等；LRU 上限） | broker |

### 7.2 协议新增（全部加法）

```rust
// Request
CommandList / CommandSet { spec } / CommandDelete { id }
ExecuteCommand { id, argument: Option<String>, query }          // ui 命令不经此通道，前端直跑
CommandDataGet { key } / CommandDataSet { key, value }
// Response
CommandCatalog { items } / CommandApplied { message } / CommandData { value }
// SearchResultKind 增加 Command；Results.items 结构不变
// 目录快照：握手后下发，CommandChanged 推送失效；前端保留字校验（用户命令不得占 ui 命令关键字）
```

### 7.3 代码落点

| 层 | 新增/改造 |
|---|---|
| broker | 新模块 `commands.rs`（目录存储/校验/占位符展开/执行分派）；`ipc.rs` 加变体 + `search_service` 并入命令行；`actions.rs` 的 `allowed_actions` 改查 ActionRegistry（内置 16 项原样迁入） |
| 前端 | 新 `Models/CommandSpec.cs`、`Services/CommandCatalog.cs`（快照缓存）、`Services/ActionRegistry.cs`；`SearchViewModel.OnQueryChanged` 插 ui 命令路由；动作面板数据源改 ActionRegistry；暂存区工具栏加"对暂存区执行" |
| indexer | **零改动**（只读边界不动） |

### 7.4 安全边界

1. broker 全量重校验：UI 只传 id + argument，路径/上下文/存在性/类型 broker 侧解析（现有信任模型延伸到命令）；
2. `launch` 目标白名单 exe/lnk/bat/cmd 且必须存在；bat/cmd 表单显式标注；
3. admin 走 `runas`，UAC 系统呈现；
4. `{clipboard}` 默认关、即读即弃、不入日志/历史；
5. 导入命令首跑确认（导入文件=不可信输入，展示解析后的标题/handler/路径）；
6. 不执行 Shell 字符串（无 `cmd /c` 面）；ui 命令不直接碰磁盘（数据走 broker KV）。

---

## 8. 现有功能迁移表（全集成落地顺序）

| 功能 | 迁移后形态 | 行为保持 | 阶段 |
|---|---|---|---|
| 动作面板 16 项 | ActionRegistry 内置成员 | 面板顺序/文案/热键不变 | K1 |
| 网页引擎 | CommandProvider 成员（`open_url` handler），`WebModeDetector.cs` 双实现退役，统一走命令路由 | 专用网页模式 UX、联想、`{q}` 编码不变 | K2 |
| 暂存区 | StagingProvider + Context 源 + 批量命令入口 | 拖拽/`workset` 行/Ctrl+D 不变 | K2 |
| 窗口模式 | WindowProvider（`>` 保留） | 触发与切换行为不变 | K2（仅加动作） |
| 别名 | 独立快车道保留（已是 Item） | 精确匹配语义不变 | 不迁 |
| ext:/path: | Router 查询修饰符（非 Provider） | 语法不变 | 不迁 |
| 文件/应用/文件夹搜索 | 事实 Provider，已走统一 Item/排序 | — | 不迁 |

---

## 9. 路线图（内核先行，每阶段独立验收、可回滚）

| 阶段 | 内容 | 验收门 | 估算 |
|---|---|---|---|
| **K0 内核地基** | CommandSpec 双侧类型 + 目录 IPC + `commands-v1.json` + `SearchResultKind::Command` + history kind command + ActionRegistry 骨架（16 内置迁入）+ 协议文档 | Rust/C# tests + clippy `-D warnings`；旧前端二进制对新 broker 回归全绿；Q2 实测 | 3-5 天 |
| **K1 命令上线** | §6 内置命令表 + 路由（ui 检测/参数态）+ 根搜索混排/配额/frecency + 热键绑命令 + note KV 通道 + list 回流最小版 | 手测清单（explorer 上下文、required 参数、frecency、回流导航）；内存 ≤100MB、P95 ≤100ms 不回归 | 1-2 周 |
| **K2 联动解锁** | 命令注册为动作（applies_to）+ 面板收编 + 暂存区批量三命令 + `{selection}/{staged}` + 引擎收编/双实现退役 + 窗口/Web 项轻动作 | 面板分组与 cap（内置段+命令段，命令按 frecency cap 5）；暂存区批量端到端；引擎行为回归 | 1-2 周 |
| **K3 用户命令** | 设置页命令管理（模板预设 6-8 个）+ launch/open_url 完整表单（多关键字/可选参数/占位符修饰符/working_dir/admin/silent/图标）+ 实时解析预览（dry-run，治痛点 8）+ 导入导出 + Fallback 行 | 导入导出往返无损；预览与实际执行逐字节一致；冲突校验用例 | 1-2 周 |
| **K4 探索（未承诺）** | 外部扩展进程（stdio JSON-RPC，`executor: "extension"`）、上下文匹配扩展（uTools 五类的 Pris 子集）、类型化多参数 | 独立 PRD 再立项 | — |

依赖：K0 → K1 → K2 → K3 严格顺序。K1 结束即有可发布价值（内置命令）；K2 结束达成"全集成 + 联动"目标；K3 完成用户侧闭环。

---

## 10. 为什么这是"最好的方案"（对标论证）

| 来源 | 吸收的最强项 | Prism 超越点 / 拒绝项 |
|---|---|---|
| Raycast | 声明式清单 + 类型化上下文 + 多触发并存 + Fallback + frecency | 无暂存区概念、无跨进程类型安全模型（单进程 monolith）；React 扩展渲染过重，拒 |
| uTools | 上下文感知方向（Context 四源）、redirect 式联动思想 | Electron 重、闭源、插件一致性差，全拒 |
| PowerToys CmdPal | Fallback 产品化细节（可排序/可禁用） | WinRT 进程外扩展模型需要商店生态支撑，暂拒（K4 参考） |
| Flow Launcher | "Result as command"洞察（本模型 Item 统一的直接来源）；stdio JSON-RPC 作 K4 协议参考 | 纯字符串参数（公认短板）、每键 spawn 进程性能模型，拒 |
| Listary | 内置命令清单对齐；14 痛点逐条治（多关键字①、可选参数⑫、导入导出⑥⑦、模板②、dry-run⑧、上下文提示③、批量⑤、URL 编码④） | 整个"4 类对象 + 9 字段 + 4 占位符"形态是反面基线 |
| Everything | —（其命令层依附搜索框，架构不同） | — |

**Prism 独有组合**（报告范围内无产品同时具备）：
1. **暂存区 = 批量命令参数源**——启动器品类里唯一的批处理工作台；
2. **一份命令定义三种身份**（结果行/面板动作/热键）——Listary 需要三处配置的事，Prism 一处声明；
3. **类型化 ActionTarget 贯穿三进程**——联动数据全程 typed，安全边界由架构保证而非约定；
4. **原生轻量**（三进程 ≤100MB、P95 ≤100ms）承载以上全部——对标产品要么重要么缺。

---

## 11. 风险与开放问题

| # | 问题 | 倾向 |
|---|---|---|
| Q1 | 命令行与文件行排序观感冲突（"搜 pro 文件夹先出命令"） | 配额 2 条+强匹配才置顶；规则集中 broker 一张表可热调；上线按反馈 |
| Q2 | 旧 Prism 读含 `"command"` kind 的 history 文件行为 | K0 实测；不确定则 envelope 升 v3+迁移函数 |
| Q3 | 动作面板收编命令后变长 | 分组（内置段/命令段）+ 命令段 frecency 排序 cap 5 |
| Q4 | `{clipboard}` 隐私 | 默认关+即读即弃；有顾虑降级到 K3 后 |
| Q5 | 关键字与日常搜索词误触发 | 关键字仅"首 token+空白"生效（引擎同款），纯词仍是文件搜索 |
| Q6 | 引擎收编后联想与参数态 UX 融合 | 联想保持独立通道，仅路由统一；K2 PRD 细化 |
| Q7 | list 回流型的导航深度（面包屑 vs 栈） | v1 单层（Back 返回），多层等真实需求 |
| Q8 | 暂存区批量执行的中途失败语义 | 聚合型交 IFileOperation 原子语义；模板型逐个执行+结束汇总行报失败数 |

---

## 12. 参考资料

- 《Listary 命令系统与新一代启动器调研报告》（2026-08，用户提供的全部一手来源链接）
- [Raycast Manifest](https://developers.raycast.com/information/manifest) · [Arguments](https://developers.raycast.com/information/lifecycle/arguments) · [Dynamic Placeholders](https://manual.raycast.com/dynamic-placeholders) · [搜索排序](https://manual.raycast.com/search-bar)
- [PowerToys Command Palette 扩展模型](https://learn.microsoft.com/en-us/windows/powertoys/command-palette/extensibility-overview) · [扩展示例](https://learn.microsoft.com/en-us/windows/powertoys/command-palette/samples)
- 代码实测：`src/prism-core/src/{ipc,actions,websearch,alias,history,persistence,shell}.rs`、`src/Prism/{Models/StagingArea.cs, ViewModels/SearchViewModel.cs, Services/StagingStore.cs}`
