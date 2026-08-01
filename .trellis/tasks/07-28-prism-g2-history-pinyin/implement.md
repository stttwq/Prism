# Implementation Plan

1. [x] 固定拼音字典/词组来源、许可证、版本和简繁测试语料。
2. [x] 实现 broker 历史 schema、原子持久化、500/90 天淘汰、开关/清除和脱敏错误恢复。
3. [x] 用表驱动测试先锁定字面/全拼/首字母、音节边界、混合后缀、多音词和高亮契约。
4. [x] 原型比较 sidecar 表示，记录构建时间、ASCII 与拼音 P95，按任务内既定门禁选型。三进程增量内存保留在步骤 9 的安装态门禁。
5. [x] 实现版本化主 sidecar、USN delta/tombstone、校验、内部重建和关闭释放。
6. [x] 在 indexer 文件/文件夹与 broker 应用评分中接入相同匹配等级，历史只在同级加权；窗口沿用同一共享 matcher，待 G5 枚举源接入。
7. [x] 扩展可选匹配 spans/status 协议字段并保持旧 reader 兼容。
8. [x] 增加 WPF 设置、清除反馈和汉字高亮；错误状态不阻塞字面结果。
9. [x] 运行 G0 对照、故障注入、内存释放和全套质量门，更新实际改变的 spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

另运行 G0 基准、sidecar 损坏/版本错配夹具、功能关闭后的三进程内存采样。

## Review Gates

- 字典许可证和版本记录通过评审后才能提交生成资产。
- 原型报告必须证明选型满足 ≤10MB 或明确停止本阶段，不能以估算替代。
- 历史/拼音协议新增字段一律可选，旧 reader 安全忽略；不要求 reader 与 writer 分两次发布（同一安装包原子替换三个二进制）。

## Rollback Points

历史存储、sidecar 生成器、搜索接入、WPF 设置分别提交。删除 sidecar 或关闭功能即可退回字面搜索；回滚不得删除用户历史，除非用户执行清除。
