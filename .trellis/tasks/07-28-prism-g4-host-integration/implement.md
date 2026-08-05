# Implementation Plan

1. [x] 在 indexer 定义 root 结构、规范化/映射错误和有界祖先验证测试。
2. [x] 将 root 验证接入 G1 候选到 Top-K 之前（G0 深目录/高候选 P95 基准仍待步骤 8 机器采集）。
3. [x] 在 WPF 建立 HostContext/scope 状态、范围标签、`Ctrl+G` 和失败清空规则；broker 可选 root 透传与 `RootUnavailable` 结构化降级已接通。
4. [~] 分别原型化 Explorer Shell COM、系统对话框 UIA、Opus 13.23 官方接口，产出兼容矩阵。
   - [x] Explorer Shell COM adapter（默认关）+ 单测
   - [x] Directory Opus 13.23 官方 `dopusrt` adapter（默认关）+ 单测
   - [x] `compat-matrix.md` 机器验收清单
   - [ ] 系统对话框 UIA adapter
5. [~] 只产品化通过识别、取目录、导航/回填、关闭竞态和降级门禁的 adapter。
   - [x] 独立开关 + `HostAdapterCatalog`；未过门禁默认关闭
   - [ ] 实机矩阵通过后才允许默认开启对应开关
6. [~] 实现上下文空输入最近项与非上下文最近窗口规则，复用 G2 历史。
   - [x] A：空 query + 有效 root + history on → 仅扫描有界历史，返回 root 下仍存在的 file/directory（前缀边界、大小写不敏感、按 history score 截断到 max）；不走 apps/web/indexer 全扫
   - [x] WPF：空输入且 Root 非空时走现有 debounce 搜索；Root 为空保持 Idle；`SetScopeRoot` 在空框下也能进入/退出该路径
   - [~] B：非宿主空输入 → 最近窗口 **defer 到 G5**（窗口枚举/切换 API 尚未产品化；history 已支持 `kind=window` 记录，本步不伪造可切换列表）
7. [~] 覆盖 Enter/`Ctrl+Enter`、标准对话框用户确认和不支持/提权宿主拒绝。
   - [x] Ctrl+Enter → 原宿主 `NavigateOrFill`（`HostScopeController.TryRevealInHost` + SearchWindow 分支）；Enter 仍 broker 打开；无宿主 fallback broker reveal；ActionFailed 不清 root，HostGone/Elevated 才 Invalidate；C# 单测
   - [ ] 标准对话框用户确认语义（SystemFileDialog 未产品化，本轮不做 Fill）
   - [ ] 不支持/提权宿主拒绝的端到端机器验收
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
- root 为可选协议字段，旧 reader 缺失时等同无 root；不要求 reader 与 writer 分两次发布。回滚后旧客户端仍能全局搜索。
