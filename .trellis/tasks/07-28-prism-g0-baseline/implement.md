# Implementation Plan

1. [ ] 记录验收机与 `main` commit，确认三进程 Release 构建和索引 ready 判断方式。
2. [ ] 在 `tools/bench/` 建立查询夹具、普通用户搜索驱动器和进程采样脚本，所有输出路径均显式传入。
3. [ ] 实现稳定预热、30 次以上暖查询、`max=8/1000` 两组运行及失败即停止语义。
4. [ ] 输出原始 JSONL、环境清单和聚合摘要；对当前拿不到的扫描/候选/路径计数器明确标注 G1 待补。
5. [ ] 在验收机执行冷/暖搜索和三进程内存采样，保存不含用户敏感数据的基线记录。
6. [ ] 复跑一次并核对样本数、百分位算法、进程身份、generation 和 Release 产物一致。
7. [ ] 执行质量门并在任务记录中保存命令、日期、结果和已知干扰。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet build src/Prism/Prism.csproj -c Release
```

基准脚本的最终命令由实现时确定，但必须提供一个搜索入口和一个三进程内存入口，并支持显式输出目录。

## Review Gates

- G0 报告经所有者确认测量口径后方可归档。
- G1 只能引用同一环境、同一查询集和同一统计方法的对照数据。
- 基准脚本若需要提升权限，必须只覆盖服务状态读取/控制，不扩大到任意命令执行。

## Rollback Point

基准资产首次提交为独立回滚点；本阶段不得与任何搜索行为修改同一提交。
