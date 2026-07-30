# Implementation Plan

1. [ ] 读取 G0 基线，增加搜索工作量计数器但保持协议字段可选。
2. [ ] 用表驱动测试固定字面匹配等级、score 和跨类型稳定 tie-break。
3. [ ] 将 indexer 文件搜索改为跨卷全局、按请求 `max` 的有界堆，并延迟最终候选路径构造。
4. [ ] 为 broker 协议增加显式握手版本，新增 `is_truncated`、稳定 kind、可选匹配元数据与可选 `filters` 占位字段，补齐超限校验；`index_generation` 已存在，只补“就绪时必返回”的契约与测试。
5. [ ] 更新 Rust broker 合并排序，确保应用、文件夹和文件遵循同一等级契约。
6. [ ] 新建 C# 测试项目和可替换网关/调度器，重构 timer 与 SearchViewModel 状态边界。
7. [ ] 实现完整性缓存键、迟到响应抑制和 generation 失效规则。
8. [ ] 覆盖跨卷、8/1000、协议矩阵、Unknown、More、失焦、Pin、Actions 和取消竞态。
9. [ ] 使用 G0 脚本复测延迟、内存、扫描/候选/路径次数；任何超门槛先回到设计，不启动下游任务。
10. [ ] 更新 backend/frontend spec 中实际改变的搜索、协议和状态机契约。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 新增的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

另运行 G0 的 `max=8/1000` 基准和三进程内存脚本。

## Risky Areas And Rollback

- 高风险文件：`hierarchy.rs`、`indexer_ipc.rs`、`indexer_runtime.rs`、`ipc.rs`、`PipeClient.cs`、`SearchViewModel.cs`。注意 `src/prism-core/src/search.rs` 是 3 行注释占位、无任何引用，本阶段不要往里写实现，它由 G3 删除。
- 协议按“可选字段 + reader 安全忽略未知值”设计即可，**不需要 reader 提交先于 writer 提交**：三个二进制由同一 Inno 安装包一次替换（`dist/prism.iss` 的 `PrepareToInstall` 先停服务再覆盖），不存在混版运行窗口。要守的是磁盘持久数据跨版本可读。
- 搜索管线、协议、WPF 状态机分三次可审查提交；不得用回滚恢复按 MFT 顺序提前终止的错误行为。
