# Implementation Plan

## Orchestration Rules

- [ ] 不启动父任务；选择拥有下一独立交付物的子任务。
- [ ] 默认从 G0 开始，完成并归档后再启动 G1。
- [ ] 启动子任务前读取父 PRD、该子任务全部规划文档及其依赖阶段结果。
- [ ] 每个子任务实施前运行 `trellis-before-dev`，完成前运行 `trellis-check`。
- [ ] 每个子任务完成时使用 `trellis-update-spec` 更新被改变的 backend/frontend 契约。
- [ ] 每个子任务单独提交和归档；父任务只更新阶段状态与集成风险。

## Default Sequence

1. G0 可复现基线。
2. G1 搜索正确性、协议与前端测试。
3. G2 使用历史与拼音。
4. G3 工程地基与权限边界。
5. G4 当前目录与宿主联动。
6. G5 窗口切换器。
7. G6 完整内置动作。
8. G7 `ext:` / `path:` 过滤。
9. G8 网页图标与在线联想。

## Parent Review Checklist

- [ ] 子任务依赖和实际归档顺序一致。
- [ ] 任何协议变化都完成旧/new reader-writer 兼容测试。
- [ ] 任何持久数据变化都带版本、损坏恢复和回滚说明。
- [ ] 任何性能主张都引用 G0 基线和相同环境的复测。
- [ ] 100MB、100ms、300ms 门禁按统一口径采样。
- [ ] 明确排除项未被子任务以“顺手实现”重新引入。
- [ ] 最终 `docs/PRISM-COMPREHENSIVE-PLAN.md` 与 `.trellis/spec/` 没有行为冲突。

## Validation

父任务不直接运行产品测试；它核对各子任务保存的验证结果。最终至少应包含：

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <Prism C# test project> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

以及适用阶段的 Windows 11 x64 搜索、内存、USN、Explorer、系统对话框、Directory Opus 13.23、7-Zip 和网络失败机器验收。

