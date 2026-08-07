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
   - [x] 不支持/提权宿主拒绝的端到端机器验收（矩阵 E12 / E14 / E15 / O10 实测通过）
8. [~] 运行 Windows 11 x64 机器矩阵、G0 基准和全套质量门，更新 spec。
   - [x] 兼容矩阵全部通过并签署（`compat-matrix.md`，E1-E15 / O1-O11 / S1-S5，
         OS 22631，Opus 13.23.0.0，2026-08-06）
   - [x] 全套质量门：Rust 145 / C# 84 通过，clippy `-D warnings` 无告警，
         `cargo fmt --check` 干净，`tools/bench/Test-Bench.ps1` 通过
   - [x] 基准工具支持 root 维度：`queries.json` 新增 5 条 root 夹具；
         `Invoke-SearchBaseline.ps1` 透传 root 并记录 `root` / `root_rejection`；
         聚合新增 `workload`（含 `path_constructions`）——耗时本身无法回答
         PRD 第 15 行的问题，必须看祖先验证次数
   - [ ] **G0 root 基准正式数据未采集**：机器不满足前提，见下方「步骤 8 采集受阻」

### 步骤 8 采集受阻（2026-08-07）

`Invoke-SearchBaseline.ps1` 按设计在索引世代移动时中止——绝对延迟只有在安静
机器上才可跨轮比较。当天 Windows 更新（`SetupHost`，累计 CPU 1380s）持续重写
磁盘，索引 6 秒推进 2059 个世代，正式基准无法采集。

期间暴露两个独立缺陷，已单列 `08-07-prism-ipc-resilience`（P1）：
indexer 管道在 USN 洪峰下大量返回 `ERROR_PIPE_BUSY`（800 请求 69% 失败），
以及 broker 静默崩溃且无日志。二者都不属于 G4 范围。

**已有的方向性证据**（`artifacts/bench/g4-root-20260807/`，配对法，
仅 31% 请求干净、每组 10-14 样本，**不足以签署**）：

- 配对延迟差中位数 +2.0ms，最坏 +28.7ms，无数量级恶化
- `path_constructions` 证明剪枝生效：`deep_system32` 1000 → 441，
  `shallow_small` 624 → 246

即 root 减少祖先验证工作量，代价是每候选一次前缀比较，净效果基本持平。
按 PRD 第 15 行，当前证据**不支持**引入祖先/子树缓存，但该结论需干净数据才能签署。

**补采条件**：`SetupHost` 结束、`Get-IndexerStatus` 世代在 `GenerationStableSeconds`
内不动、Prism 前端与 Opus 关闭。命令见 `tools/bench/README.md`；
`Invoke-RootScopeComparison.ps1` 是配对对照工具，**不能**替代正式基准。

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
