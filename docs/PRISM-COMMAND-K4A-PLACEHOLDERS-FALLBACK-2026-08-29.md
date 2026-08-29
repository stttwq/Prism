# K4a 第一批实施记录：{clipboard}/{date}/{uuid} 占位符 + 根搜索无结果回退命令

> 2026-08-29。来源：命令系统调研报告（Raycast 对标）差距分析第一批。设计原则：对仓库影响最小——无 schema bump、无 deny 层触碰、零新增 IPC 请求、零新增依赖（仅 windows crate 加一个 feature）。

## 交付内容

### 1. 新占位符（commands.rs）

`PLACEHOLDERS` 从 3 个扩到 6 个：`query` / `current_folder` / `selection.target` / `clipboard` / `date` / `uuid`。

- **`{clipboard}`**：剪贴板文本。**UI 进程读取**（`PipeClient.ReadClipboardForCommand`，在 `ExecuteCommandAsync` / `CommandPreviewAsync` 两个咽喉点即读即传），经 `CommandInvocationContext.clipboard`（新可选字段，serde default）传入 broker。与设计文档 K4 预案（broker STA 读、默认关）的偏差：UI 永远在调用环路中（broker 无推送通道），UI 读取 diff 更小、暴露面更窄（仅命令调用瞬间读）。异常→null→展开为空串；UI 侧截 8 KiB 字符，broker 侧 `expansion_context_from` 再按 `TEXT_MAX_BYTES` 字符边界安全截断（静默，不走失败路径）。
- **`{date}`**：默认 `yyyy-MM-dd`。新**参数化修饰符** `fmt:`（`{date|fmt:yyyyMMdd HH:mm:ss}`），token 集 `yyyy yy MM M dd d HH H mm m ss s`，其余字符字面输出；`fmt:` 用在非 date 占位符上或空模式 → `BadModifier`。时间源复用 `filters::local_now()`（GetLocalTime，本地时区，零新依赖）；`ExpansionContext.now` 显式传入（同模板内多次 `{date}` 一致、测试可固定）。`{time}` 不单列，用 `{date|fmt:HH:mm:ss}`。
- **`{uuid}`**：UUID v4。`BCryptGenRandom`（windows crate 新 feature `Win32_Security_Cryptography`）16 字节熵 + v4 位；失败（实际不会发生）回退墙钟纳秒+原子计数器。每次出现独立生成。
- 编码规则不变：仅 `{query}` 默认 percent-encode，其余默认 raw（URL 里用 `{clipboard|percent-encode}`）。
- 预览一致性：`CommandPreview` 与执行走同一 `expand_template`（§4.6 不变），预览时 `{clipboard}`/`{date}`/`{uuid}` 显示当时真实值。

### 2. 无结果回退命令（persistence.rs + SearchViewModel.cs）

- **持久化**：`UserCommandDefinition.fallback: bool`（`#[serde(default, skip_serializing_if = "is_false")]`，新增 `is_false`；存量 commands-v1.json 逐字节不变）。`CommandDescriptor` 同名字段随 catalog 下发（skip when false，旧 UI 容忍忽略）。
- **合成行**：`SearchViewModel.ApplySearchResponse` 的 K3 §4.9 空结果块内，从 `_commandCatalog.Snapshot` 取 `Fallback==true` 的启用命令，合成 `SearchResult.FallbackCommand`（Kind="command"，RowKey=`fbcmd:{id}:{query}` 防撞），**排在默认 web 行之前**（产品取向已确认），web 行保留最后。broker 全程无感知（零协议改动，工作集召回行同款先例）。
- **传参**：`SearchResult.FallbackQuery`（UI-only 字段）→ `BuildCommandInvocationContext` 把查询词作为 `arguments.text`（`{query}` 展开源），Source=root，复用现有 ExecuteCommand 路径，无新 InvocationSource。
- **设置页**：命令编辑器新增「无结果时作为回退命令显示」CheckBox（SettingsCheckBoxStyle）；导入路径 `ParseImportCommand` 解析 fallback；模板字段下新增占位符速查帮助（7 占位符 + 修饰符 + 隐私提示）。

## 兼容性

- 新 UI + 旧 broker：`clipboard`/`fallback` 字段被 serde 忽略（无 deny 层），功能不生效但不报错。
- 旧 UI + 新 broker：新字段被 C# 容忍解析忽略，无影响。
- 不触碰唯一 deny 层 `CommandArguments`；不 bump `COMMANDS_SCHEMA_VERSION`（仅追加可选字段）。

## 测试

- Rust（commands.rs）：clipboard 有值/None 空串/percent-encode、截断+字符边界、date 默认/fmt 全 token/中文/空模式/错占位符、uuid 形状+v4 位+唯一性。
- Rust（persistence.rs）：fallback 缺字段默认 false + true 往返 + false 省略（旧文件逐字节）。
- C#（CommandFallbackTests.cs）：合成行形状/RowKey/target、Descriptor 容忍解析缺字段、CommandEditItem 往返。
- 质量门：`cargo test --lib` 552 通过（15 live #[ignore]）；clippy 我方代码零新增告警（filters.rs map_or×3 与 ipc.rs field-assignment 为存量）；`dotnet build` 0 警告；`dotnet test` 422 通过 + 2 失败（`WebIconNegativeCacheTests`/`FaviconGrantTests`，stash 验证为存量测试隔离问题，与本批无关）。

## 明确不做（留给后续批次）

- 参数系统（typed arguments、可选/默认值）、`{argument}`、命令链、命令 UI 视图——第二批起。
- 回退总开关设置项（`_fallbackEnabled` 保持硬编码 true）。
- 模板预设增补、内置命令的回退编辑（编辑器只管用户命令）。
- broker 侧读剪贴板（如需 broker 独立调用 {clipboard} 再按设计文档 STA 方案补）。

## 部署注意

- 运行时安装目录实际为 `D:\LS\Prism`（注册表 InstallLocation；早前记忆 E:\LS\Prism 已过时）。写该目录需提权（Users 组仅 RX）。
- indexer 二进制本批未变，服务无需重启；broker + 前端重启即可。
