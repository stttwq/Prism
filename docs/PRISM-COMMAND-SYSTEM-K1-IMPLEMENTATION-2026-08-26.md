# PRISM 统一命令系统 K1 施工方案

**文档版本**: K1-IMPL-2026-08-26  
**K0 基线**: commit a1b72d2 (2026-08-26)  
**目标状态**: K1 - 基础命令路由与执行能力

---

## 施工概述

K1 在 K0 地基之上建设最小可用的命令系统：两条内置命令（`prism.settings.open` 与 `prism.terminal.open`）在根搜索中可见、可执行。

**核心目标**：
- 命令行在搜索结果中显示（`SearchResultKind::Command`）
- Enter 键能执行命令并隐藏窗口
- 设置页能呼出，在终端中能打开当前目录

**交付边界**：
- **IN**：两条内置命令的完整路径（搜索→显示→执行→隐藏）
- **OUT**：用户命令导入、关键词路由、动作面板、暂存区集成均留待 K2+

---

## 架构设计

### 命令产出通道设计

K1 采用**并行产出 + 最终合并**的架构：

```
搜索请求 → ┌─ 文件索引器 ────┐
           ├─ 应用目录   ────┤ → 统一排序 → 截断 → 前端
           ├─ 网页匹配   ────┤
           └─ 命令目录   ────┘   (新增)
```

命令目录独立产出**不参与跨通道竞争**：
- 命令匹配逻辑与文件名匹配解耦
- 命令排序依据标题字面匹配，使用既有 `literal_match_lowered`
- 最终合并时命令行按 `MatchMetadata` 参与统一排序

### 命令 Lane 合并点

**关键设计决定**：命令结果在 `search_service` 的 **alias 合并之后** 注入：

```rust
// 现有流程
ranked.extend(history_candidates);
ranked.extend(app_results);
// 索引器结果合并...
alias_indices = merge_alias_rows(&mut ranked, alias_rows);

// K1 新增：命令结果在此处注入
let command_results = command_search(query, command_context, &commands);
ranked.extend(command_results);

sort_search_results_with_picks(&mut ranked, pick_flags);
```

**理由**：
1. 命令不需要与别名去重（命令ID vs 文件路径，键空间不重叠）
2. 简化 `query_pick_key` 和 `pick_flags` 逻辑（无需感知命令）
3. 命令行享受统一的 picks 置顶机制

### 前端命令目录管理

K0 已建立 `CommandCatalog` 类但**未接线**。K1 需要：

```csharp
// App.xaml.cs 中装配
_catalog = new CommandCatalog(_pipe);
_pipe.ConnectionChanged += connected => {
    if (connected) _ = _catalog.RefreshAsync();
    else _catalog.Clear();
};

// SearchViewModel 中引用目录代际
var context = _searchContext with { 
    CommandCatalogGeneration = _catalog.Generation 
};
```

---

## 详细实施

### 1. Broker 侧命令搜索 (Rust)

**1.1 命令搜索函数签名**

```rust
// src/prism-core/src/ipc.rs
fn command_search(
    query: &str,
    max_results: usize,
    commands: &Arc<CommandStore>,
) -> Vec<SearchResult>
```

**1.2 核心搜索逻辑**

复用既有 `literal_match_lowered` 进行标题匹配：

```rust
fn command_search(query: &str, max_results: usize, commands: &Arc<CommandStore>) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    
    let terms = NameTerms::parse(query);
    let catalog = commands.catalog();
    let mut results = Vec::new();
    
    for desc in catalog {
        // 只匹配 enabled 且有 root_search binding 的命令
        if !desc.enabled || desc.bindings.root_search.is_none() {
            continue;
        }
        
        if let Some(metadata) = literal_match_lowered(&desc.title, &terms) {
            results.push(SearchResult {
                kind: SearchResultKind::Command,
                title: Arc::from(desc.title.as_str()),
                subtitle: Arc::from(desc.subtitle.as_str()),
                execute_id: Arc::from(desc.id.as_str()),
                target: ActionTarget::new(TargetKind::Command, desc.id.clone()),
                match_spans: build_match_spans(&desc.title, &terms, &metadata),
                match_metadata: Some(metadata),
            });
        }
    }
    
    // 按 metadata 排序并截断
    results.sort_by(|a, b| {
        a.match_metadata.unwrap().cmp(&b.match_metadata.unwrap())
    });
    results.truncate(max_results);
    
    results
}
```

**1.3 集成到搜索主流程**

在 `search_service` 的既有合并点注入命令结果：

```rust
// 在 alias 合并之后
if !alias_indices.is_empty() {
    alias_indices = merge_alias_rows(&mut ranked, alias_rows);
}

// K1 新增：命令搜索
if root.is_none() && !has_filters && !is_empty_query {
    let command_results = command_search(&name_query, result_slots, commands);
    ranked.extend(command_results);
}
```

### 2. 命令执行路径 (Rust)

**2.1 broker handler 注册表**

扩展 K0 的空注册表：

```rust
// src/prism-core/src/commands.rs
const BROKER_HANDLERS: &[(&str, BrokerHandlerId)] = &[
    ("prism.settings.open", BrokerHandlerId::OpenSettings),
    ("prism.terminal.open", BrokerHandlerId::OpenTerminalHere),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerHandlerId {
    OpenSettings,
    OpenTerminalHere,
}
```

**2.2 ExecuteCommand IPC 端点**

新增专用的命令执行请求：

```rust
// Request 枚举新增
ExecuteCommand {
    context: CommandInvocationContext,
},
```

**2.3 命令分派逻辑**

```rust
fn execute_command(
    context: CommandInvocationContext, 
    commands: &Arc<CommandStore>
) -> Response {
    context.validate()?;
    
    let catalog = commands.catalog();
    let desc = catalog.iter()
        .find(|d| d.id == context.command_id && d.enabled)
        .ok_or("command not found or disabled")?;
        
    match desc.owner {
        "broker" => {
            let handler = broker_handler(&desc.id)
                .ok_or("no handler for broker command")?;
            execute_broker_command(handler, context)
        }
        "ui" => {
            Response::UiCommand { 
                id: desc.id.clone(),
                context 
            }
        }
        _ => Err("unsupported command owner")
    }
}

fn execute_broker_command(
    handler: BrokerHandlerId,
    context: CommandInvocationContext
) -> Response {
    match handler {
        BrokerHandlerId::OpenSettings => {
            // 通过现有 Shell 路径触发设置页
            Response::Status { is_indexing: false, cancelled: false }
        }
        BrokerHandlerId::OpenTerminalHere => {
            let folder = context.current_folder
                .or(context.selection.map(|s| s.value))
                .unwrap_or_else(|| env::current_dir().to_string());
            // 调用 shell_execute 在指定目录打开终端
            open_terminal_at(&folder)?;
            Response::Status { is_indexing: false, cancelled: false }
        }
    }
}
```

### 3. 前端集成 (C#)

**3.1 SearchViewModel 命令执行**

修改现有的执行分支：

```csharp
// SearchViewModel.ExecuteSelectedCoreAsync()
if (item.Kind == "command")
{
    try 
    {
        var context = BuildCommandInvocationContext(item);
        await _pipe.ExecuteCommandAsync(context).ConfigureAwait(true);
        HideRequested?.Invoke();
    }
    catch (Exception ex)
    {
        _state.StatusMessage = "命令执行失败：" + ShortMsg(ex);
    }
    return;
}

private CommandInvocationContext BuildCommandInvocationContext(SearchResult item)
{
    return new CommandInvocationContext
    {
        CommandId = item.ExecuteId,
        Source = InvocationSource.Root,
        Arguments = new CommandArguments(), // K1 暂不支持参数
        CurrentFolder = _hostContext?.Root,
        HostKind = _hostContext?.Kind.ToString().ToLowerInvariant(),
        HostCapabilities = _hostContext?.Capabilities ?? Array.Empty<string>()
    };
}
```

**3.2 PipeClient 新增 ExecuteCommandAsync**

```csharp
public async Task ExecuteCommandAsync(
    CommandInvocationContext context, 
    CancellationToken ct = default)
{
    var request = new {
        type = "execute_command",
        context = new {
            command_id = context.CommandId,
            source = context.Source.ToString().ToLowerInvariant(),
            arguments = context.Arguments,
            current_folder = context.CurrentFolder,
            host_kind = context.HostKind,
            host_capabilities = context.HostCapabilities
        }
    };
    
    var response = await SendActionAsync(request, ct).ConfigureAwait(false);
    
    // 处理 UI 命令回传
    if (response.TryGetProperty("type", out var typeEl) 
        && typeEl.GetString() == "ui_command")
    {
        var commandId = response.GetProperty("id").GetString();
        await ExecuteUiCommand(commandId).ConfigureAwait(false);
    }
}

private async Task ExecuteUiCommand(string commandId)
{
    switch (commandId)
    {
        case "prism.settings.open":
            // 触发主线程设置页打开
            Application.Current.Dispatcher.BeginInvoke(new Action(() => {
                var app = (App)Application.Current;
                app.OpenSettings(); // 需要将 App.OpenSettings 改为 public
            }));
            break;
    }
}
```

**3.3 CommandCatalog 接线**

在 App.xaml.cs 中集成目录服务：

```csharp
private CommandCatalog? _catalog;

// OnStartup 中初始化
_catalog = new CommandCatalog(_pipe);
_pipe.ConnectionChanged += connected => {
    if (connected) {
        _ = Task.Run(async () => {
            try { await _catalog.RefreshAsync(); } 
            catch { /* 目录拉取失败不影响其他功能 */ }
        });
    } else {
        _catalog.Clear();
    }
};

// SearchViewModel 构造中传入目录引用
_vm = new SearchViewModel(
    _state, _pipe, 
    activator: new Win32WindowActivator(),
    suggestions: new SuggestionService(),
    staging: _staging,
    aliasList: ListBackendAliasesAsync,
    commandCatalog: _catalog  // 新增参数
);
```

### 4. 内置命令实现

**4.1 设置页命令**

K0 已有 `prism.settings.open` 定义，K1 需要：
- broker 侧：返回 `UiCommand` 响应交给前端执行
- 前端侧：接收后调用 `App.OpenSettings()`

**4.2 终端命令**

新增 `prism.terminal.open`：
- 标题："在此处打开终端"
- 逻辑：使用当前目录上下文（`current_folder`）或选中项目录
- 实现：通过 `ShellExecute` 调用 `wt.exe` 或 `cmd.exe`

### 5. UI 适配

**5.1 ResultList 图标支持**

扩展现有图标分派逻辑：

```csharp
// ResultList.LoadItemIconAsync
if (item.Kind == "command")
{
    return GetCommandIcon(item.ExecuteId);
}

private static BitmapSource GetCommandIcon(string commandId)
{
    return commandId switch
    {
        "prism.settings.open" => LoadIconResource("Settings"),
        "prism.terminal.open" => LoadIconResource("Terminal"), 
        _ => LoadIconResource("Command")  // 默认命令图标
    };
}
```

**5.2 禁用不适用操作**

命令行已在 K0 中禁用 Reveal：

```csharp
// SearchViewModel.RevealSelectedAsync() 已有
if (item.Kind is "command") return;
```

K1 需确认动作面板同样拒绝命令：

```csharp
// SearchViewModel.EnterActionsAsync() 已有
if (item.Kind is not ("file" or "folder")) return;
```

---

## 测试验证

### 核心场景测试

**T1: 命令显示**
```
输入: "设置" → 显示 "prism.settings.open"
输入: "终端" → 显示 "prism.terminal.open" 
```

**T2: 命令执行**
```
选中设置命令 → Enter → 设置窗口打开 → 搜索窗口隐藏
选中终端命令 → Enter → 终端在当前目录打开 → 搜索窗口隐藏
```

**T3: 排序正确性**
```
输入: "设" → 命令行按标题匹配度排序，与文件结果统一竞争
```

### 边界条件测试

**T4: 协商保护**
```
旧版前端连接 → 不发送 command_context → 命令结果为空
```

**T5: 禁用场景**
```
窗口模式: ">设置" → 无命令结果
目录模式: 有 root → 无命令结果  
过滤模式: "ext:txt 设置" → 无命令结果
```

> **K2 修订（commit `5d4576e`）**：T5 第二条已被推翻。命令 lane 注入条件现为
> `has_command_context && !has_filters`（`src/prism-core/src/ipc.rs:2371`），
> root 不再是排除条件——有 root 时仍显示命令。过滤态仍排除命令。

### 单元测试补充

**命令搜索逻辑** (`commands.rs`):
```rust
#[test]
fn command_search_matches_title() {
    let (store, _) = test_store();
    store.set(command_def("user.test", "测试命令")).unwrap();
    
    let results = command_search("测试", 10, &store);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title.as_ref(), "测试命令");
}

#[test] 
fn command_search_respects_binding() {
    // 无 root_search binding → 不出现在结果中
}

#[test]
fn command_search_empty_query_returns_empty() {
    // 空查询不返回命令
}
```

**执行路径测试** (`ipc.rs`):
```rust
#[test]
async fn execute_command_validates_context() {
    // 无效 command_id → 返回错误
}

#[test]
async fn execute_broker_command_routes_correctly() {
    // broker 命令正确路由到 handler
}
```

**前端集成测试** (C#):
```csharp
[Fact]
public async Task Command_Execution_Hides_Window() {
    // 命令执行成功后窗口隐藏
}

[Fact]
public void Command_Catalog_Updates_On_Connection() {
    // 连接状态变化时目录正确更新
}
```

---

## 风险与缓解

### 技术风险

**R1: 命令与文件排序竞争**
- 风险：命令行在统一排序中位置不符预期
- 缓解：使用精确的 `MatchMetadata` 确保命令按匹配质量合理排位
- 监控：通过 T3 测试确认排序行为

**R2: 前端目录代际同步**
- 风险：目录更新延迟导致命令不可见
- 缓解：ConnectionChanged 立即刷新目录，失败不阻塞其他功能
- 监控：检查 `_catalog.Generation` 与后端保持一致

**R3: UI 命令执行上下文**
- 风险：`App.OpenSettings` 访问性或线程安全问题
- 缓解：通过 Dispatcher.BeginInvoke 确保 UI 线程执行
- 备案：如需要将 App.OpenSettings 改为 public/internal

### 兼容性风险

**R4: K0 前端连接 K1 后端**
- 风险：旧前端收到 `SearchResultKind::Command` 无法处理
- 现状：K0 前端已有 `Command` 枚举值，映射为 `"command"` 字符串
- 缓解：K0 的 `ActionTarget.FromLegacy` 正确映射命令类型

**R5: K1 前端连接 K0 后端**
- 风险：新前端期望命令功能但连接到旧后端
- 缓解：通过 `commands_v1` 能力协商，未协商则不显示命令
- 行为：降级到 K0 行为，无功能损失

---

## 交付检查单

### 代码交付

**Rust (prism-core)**
- [x] `commands.rs` - 扩展 handler 注册表
- [x] `ipc.rs` - 新增 `command_search` 函数
- [x] `ipc.rs` - 集成命令结果到 `search_service`  
- [x] `ipc.rs` - 新增 `ExecuteCommand` 请求处理
- [x] `shell.rs` - 实现 `open_terminal_at` 函数

**C# (Prism)**
- [x] `SearchViewModel.cs` - 修改命令执行分支
- [x] `PipeClient.cs` - 新增 `ExecuteCommandAsync`
- [x] `App.xaml.cs` - 接线 `CommandCatalog`
- [x] `ResultList.cs` - 命令图标支持
- [x] `CommandInvocationContext.cs` - 新建上下文类型

### 测试交付

- [x] Rust 单元测试：命令搜索逻辑 (3个)
- [x] Rust 集成测试：执行路径 (2个)  
- [x] C# 单元测试：前端集成 (2个)
- [ ] 手工测试：核心场景 T1-T3
- [ ] 手工测试：边界条件 T4-T5

### 文档交付

- [x] 本施工方案 (当前文档)
- [x] 代码注释：关键函数的设计理由
- [x] 测试说明：验证步骤与预期结果

---

## 后续演进

K1 交付最小可用命令系统后，后续版本规划：

**K2**: 用户命令导入与关键词路由
**K3**: 命令参数输入与文件选择器集成  
**K4**: 暂存区命令与工作集管理集成
**K5**: 命令热键与高级绑定配置

K1 的架构设计已为这些演进预留空间：
- 命令搜索函数接受 `max_results` 参数，支持未来容量优化
- `CommandInvocationContext` 包含完整上下文字段，支持参数化命令
- 前端目录代际同步机制支持动态命令管理

---

*文档完成时间: 2026-08-26*  
*预计实施周期: 5-7 个工作日*  
*依赖项: K0 基线稳定运行*