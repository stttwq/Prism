# Implementation Plan

1. [ ] 先用表驱动测试固定 token、引号、无效回退、OR/AND 和规范化语义。
2. [ ] 实现 broker parser 与有界 FilterSet，不在 WPF 复制解析逻辑。
3. [ ] 扩展可选 filters 协议、版本兼容和超限错误。
4. [ ] 在 indexer 候选进入 Top-K 前接入 ext/path/root 组合过滤。
5. [ ] 把规范化 filters 加入前端请求身份与缓存失效键。
6. [ ] 覆盖足额 Top-K、跨卷、8/1000、应用/窗口/网页排除和迟到响应。
7. [ ] 用 G0 脚本测 path 构造/父链次数、P95 和三进程内存，运行全套质量门。
8. [ ] 更新 query grammar、IPC 和缓存 spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

## Review Gates And Rollback

- parser 与执行器必须共享结构化 FilterSet，不允许字符串二次解析产生偏差。
- filters 复用 G1 预留的可选字段，旧 reader 缺失时等同空集合；不要求 reader 与 writer 分两次发布。任何情况下后端都不得收到已剥离名称但未应用过滤的半过滤请求。
- parser、协议、indexer 执行、WPF 缓存分别提交，功能开关可恢复普通文本行为。
