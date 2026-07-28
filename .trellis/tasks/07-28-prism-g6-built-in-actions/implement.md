# Implementation Plan

1. [ ] 定义 target-kind/action allowlist、参数 schema、outcome/error 类别和协议上限。
2. [ ] 扩展 G3 STA worker adapter，先实现无 mutation 的定位、属性、打开方式、复制路径与应用目标动作。
3. [ ] 实现复制/剪切和 `IFileOperation` 的复制到、移动到、回收站、永久删除，强制系统确认。
4. [ ] 实现 DestinationPicker，按历史目标优先、近期目录补足到 8 条，并支持仅文件夹搜索。
5. [ ] 实现 RenameEditor、leaf 验证和默认选择主体行为。
6. [ ] 实现 7-Zip/Windows 11 ZIP adapter、结构化参数、冲突与退出验证。
7. [ ] 将成功、取消、失败映射到隐藏/保留、历史写入和 generation 等待状态。
8. [ ] 覆盖全部动作矩阵、危险边界与 Windows 11 机器测试；审计 indexer 无动作入口。
9. [ ] 运行全套质量门、内存复测并更新 Shell/action/frontend state spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

另执行动作验收矩阵，分别覆盖 7-Zip 已安装/未安装和 generation 正常/超时。

## Review Gates And Rollback

- 永久删除实现必须单独安全评审，确认无跳过系统确认路径。
- 无 mutation 与 mutation 动作分开提交；复制/移动/删除/ZIP 各有独立 allowlist。
- 任何动作出现 target 类型混淆或 LocalSystem 执行路径时停止阶段，不允许带风险降级发布。
