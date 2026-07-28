# Implementation Plan

1. [ ] 定义可切换窗口过滤规则、snapshot/opaque id、激活结果和安全上限。
2. [ ] 实现 broker 窗口枚举与 execute 前身份复核，覆盖句柄复用和关闭竞态。
3. [ ] 将标题/应用名接入 G2 字面、拼音、高亮和历史评分。
4. [ ] 扩展稳定 `window` kind、typed target 和兼容 reader。
5. [ ] 在 WPF 增加显式 Window mode、`>` 解析、空输入与成功/失败状态。
6. [ ] 实现成功后历史写入和隐藏，失败不写历史并保留 UI。
7. [ ] 运行多窗口、最小化、标题变化、关闭竞态和长时间内存机器测试。
8. [ ] 执行全套质量门并更新窗口枚举/状态机 spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

## Review Gates And Rollback

- 前台激活原型必须在 Windows 11 x64 验收机通过，不允许用注入规避系统限制。
- 协议、provider、WPF mode 分开提交；任一层可关闭窗口模式而不影响文件搜索。
- 无持续内存增长并经历史过滤测试后方可归档。
