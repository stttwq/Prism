# Implementation Plan

1. [ ] 绘制当前生产/测试模块引用图，列出旧链测试与现行链的一一迁移表。
2. [ ] 迁移测试到 `hierarchy/indexer_client/indexer_runtime`，确认覆盖后移除旧实现和孤立占位文件。
3. [ ] 定义 typed result/action target、稳定错误类别和新旧协议兼容测试。
4. [ ] 抽取公共 Shell adapter，并实现有界队列、专用 STA 线程、COM 生命周期和安全关闭。
5. [ ] 将现有 reveal/属性/打开方式/启动路径迁入公共层，做行为对照。
6. [ ] 分离机器硬排除与用户过滤快照，在 Top-K 前执行并限制条数/长度。
7. [ ] 为 broker/indexer 建立独立结构化 rolling log，注入目录失败、磁盘只读和轮转失败。
8. [ ] 审计 LocalSystem IPC 命令集合，加入拒绝越权请求的回归测试。
9. [ ] 运行全套质量门和 Windows 11 Shell/COM 机器测试，更新 backend/frontend spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

## Review Gates

- 删除旧模块前必须先提交测试迁移和引用证据。
- STA worker 的取消/退出/崩溃边界通过评审后，G6 才能复用。
- 日志轮转配置必须与实际 crate/writer 能力一致。

## Rollback Points

测试迁移、Shell adapter、STA executor、typed protocol、排除、日志分别形成提交。协议回滚保留新字段 reader；权限边界不可回滚到 indexer 执行 Shell。
