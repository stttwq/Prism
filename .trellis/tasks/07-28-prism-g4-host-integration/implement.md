# Implementation Plan

1. [ ] 在 indexer 定义 root 结构、规范化/映射错误和有界祖先验证测试。
2. [ ] 将 root 验证接入 G1 候选到 Top-K 之前，并用 G0 脚本测深目录与高候选查询。
3. [ ] 在 WPF 建立 HostContext/scope 状态、范围标签、`Ctrl+G` 和失败清空规则。
4. [ ] 分别原型化 Explorer Shell COM、系统对话框 UIA、Opus 13.23 官方接口，产出兼容矩阵。
5. [ ] 只产品化通过识别、取目录、导航/回填、关闭竞态和降级门禁的 adapter。
6. [ ] 实现上下文空输入最近项与非上下文最近窗口规则，复用 G2 历史。
7. [ ] 覆盖 Enter/`Ctrl+Enter`、标准对话框用户确认和不支持/提权宿主拒绝。
8. [ ] 运行 Windows 11 x64 机器矩阵、G0 基准和全套质量门，更新 spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

另执行 Explorer、系统打开/保存对话框、Directory Opus 13.23 的版本化机器验收清单。

## Review Gates And Rollback

- 父链方案超门槛时必须提交数据后重新评审，不能直接扩大 NodeSlot。
- 三个 adapter 分别形成提交与功能开关；单个 adapter 失败可独立回滚。
- root 协议 reader 兼容先提交，writer 后启用。回滚后旧客户端仍能全局搜索。
