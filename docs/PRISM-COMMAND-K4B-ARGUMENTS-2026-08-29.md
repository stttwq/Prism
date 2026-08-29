# K4b 第二批实施记录：命令声明式参数 + {arg.name} 占位符

> 2026-08-29。Raycast 差距分析第二批（参数系统最小版）。延续 K4a 的最小影响原则：零新增 IPC 请求、零新增依赖、无 schema bump。

## 功能语义

1. **声明**：`UserCommandDefinition.arguments: Vec<CommandArgumentSpec{name, required, default}>`。上限 8 个；名 `[a-z0-9_]` ≤32 字符、不重复；default ≤256 字符；required 不得带 default；**必填必须排在可选之前**（位置切分的前提）。结构校验在 `CommandData::validate`（导入/保存同路径）。
2. **切分**（`commands::resolve_arguments`）：`arguments.text` 按 `split_whitespace` 切分，按声明顺序位置传入；多余 token 忽略；可选缺位填 default；必填缺位 → `缺少必填参数：a、b`（preview 与 execute 同一错误文案，UI 状态栏展示）。v1 不支持引号——含空白取值用 `{query}`。
3. **占位符**：`{arg.name}` 动态命名空间，先于静态白名单解析；未声明的名字 → `UnknownPlaceholder`（预览/保存即暴露拼写错误）；修饰符链照常（`{arg.q|percent-encode}`）。`{query}` 语义不变 = 整段文本。
4. **保存校验**：`handle_command_set` 的空上下文以「声明 default 填充」（required 无默认 → 空串）——已声明的 `{arg.x}` 保存时可展开，未声明的照样拒绝。实测：模板引用 `{arg.bogus}` 与 required 带默认均在保存时拒绝。

## 关键落点

- `ExpansionContext.args: BTreeMap<String,String>`；`expansion_context_from(context, args)` 签名加参（4 个调用点全量更新）。
- preview（ipc.rs handle_command_preview）与 execute 用户分支在取到 `user_cmd` 后先 `resolve_arguments`，Err 直接转 Error/PreviewResult-false。
- `CommandDescriptor.arguments` 随 catalog 下发（`skip_serializing_if = Vec::is_empty`），编辑器回显用。
- WPF：`CommandEditItem.Arguments`（`ObservableCollection<CommandArgumentEditItem>`）+ 设置页表格（ItemsControl 行编辑：名称/必填/默认值/删除 + 「+ 添加参数」，code-behind 纯视图操作，主题 token）；ToDefinition 过滤空名行后映射上传。结构校验只在 broker 侧，UI 不重复规则。

## 兼容性

- 旧 broker + 新 UI：上传的 `arguments` 字段被忽略（无 deny 层），参数静默不生效。
- 新 broker + 旧 UI：descriptor 新字段被容忍解析忽略。
- 未声明参数的存量命令：resolve_arguments 空表 → 执行路径行为逐字节不变。

## 测试

- Rust +6：resolve_arguments 位置/默认/多余/必填缺失/全可选空文本、`{arg.x}` 展开+未声明报错；persistence 结构校验 5 组拒绝 + 往返/旧文件缺字段。cargo test --lib 558 绿。
- C# +4：descriptor 容忍解析（缺字段/非布尔 required）、编辑行往返、空名行过滤。dotnet test 429 通过（2 失败为存量 favicon 偶发，stash 已验证与本批无关）。
- **管道 E2E**（artifacts\pipe-k4b-test.ps1，9/9 PASS）：保存带参数命令、保存时拒绝未声明 `{arg.bogus}` 与 required 带默认、preview `"80 60"`→`w=80&h=60`、`"80"`→`h=10`、`""`→必填缺失报错、catalog 下发、落盘。
- **GUI E2E**（artifacts\gui-k4b-test.ps1，PASS）：真实设置页——搜索「打开设置」进命令页、选中带参命令、表格渲染 width/height/10 三值、添加行 +2 编辑框、删除行复原、**预览按钮返回 `URL: https://example.test/?w=test&h=10`**（参数解析+默认值在真实 UI 路径全通）。

## 测试环境备忘

- PS 5.1 读无 BOM 的 .ps1 按 ANSI——脚本含中文必须 UTF-8 **with BOM**（gui-k4b-test.ps1 首跑「打开设置」变乱码即此坑）。
- WPF ListBoxItem 的 UIA Name 是类型名（无 ToString 重写时），按标题选中要查后代 Text 元素。

## 明确不做（后续批次候选）

- 引号切分、类型校验（number/enum）、dropdown 补全、参数提示 UI（输入时展示用法行）。
- `{arg.name}` 与 {query} 的混用指引文档化进预设库。
