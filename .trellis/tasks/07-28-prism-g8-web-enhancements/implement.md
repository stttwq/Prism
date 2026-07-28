# Implementation Plan

1. [ ] 将网页关键词解析为显式 Web mode，并保证直接结果同步产生、不混排本地 provider。
2. [ ] 定义可注入 HTTP transport、request identity、取消和 800ms 超时测试地基。
3. [ ] 为 Bing、百度、Google 实现有界 adapter 和固定响应夹具；拒绝自定义联想 endpoint。
4. [ ] 实现设置中的默认关闭、隐私说明、切换即取消和最多 5 条 UI 合并。
5. [ ] 打包内置图标，实现自定义 origin 的独立 favicon 授权流程。
6. [ ] 实现受限下载、格式/尺寸验证、版本化 metadata、原子写和有界缓存淘汰。
7. [ ] 注入断网、超时、取消、迟到、证书/状态码、无效 JSON、超大响应和损坏缓存。
8. [ ] 检查 history/日志零 query，运行 C# tests、WPF Release build 和适用 Rust 质量门。
9. [ ] 更新网络隐私、Web mode、缓存和状态机 spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

网络测试默认使用 fake transport/固定夹具；真实端点只做显式、可关闭的人工兼容检查。

## Review Gates And Rollback

- 隐私文案、默认关闭和日志脱敏先评审，再允许真实联网测试。
- suggestions、favicon downloader/cache 分开提交和开关；任一可独立回滚。
- 任何无法实施响应/解码上限的方案不得发布。
