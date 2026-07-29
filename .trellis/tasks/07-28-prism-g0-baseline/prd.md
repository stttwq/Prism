# G0 可复现基线

## Goal

建立后续 G1-G8 共用的三进程 Release 内存、搜索延迟与工作量基线，停止把旧两进程 38MB 或其他未复测数字当作当前事实。本阶段只增加测量资产和基线记录，不改变产品行为。

## Background

- 当前架构是 WPF `Prism.exe`、普通权限 `prism-core.exe` broker、LocalSystem `prism-indexer-service.exe` 三进程和两条命名管道。
- **100MB 门槛的现行口径只覆盖两个进程。** `.trellis/spec/backend/quality-guidelines.md` 的 “Memory Acceptance (≤100MB hard gate)” 把门槛定义为 `Prism.exe` + `prism-core.exe` 的 Private Working Set 之和，写于三进程拆分之前，全文未提及 `prism-indexer-service.exe`。综合计划所说的“三进程 ≤100MB”目前是目标而非已提交门槛；把 spec 改写为三进程口径是本阶段的交付物之一。40-45MB 仅是复测后的优化期望。
- `[profile.release]` 使用 `opt-level = "z"`（体积优先），与 P95 ≤100ms 目标存在张力，必须作为显式基线变量记录。
- 首屏请求 `max=8`，展开请求 `max=1000`；暖查询目标分别是 P95 不超过 100ms 和 300ms。
- 基线必须绑定 commit、Release 二进制、Windows build、CPU/内存、卷与索引规模，不能只记录单次最快结果。

## Requirements

- 固定一套可版本控制的查询集，至少覆盖 ASCII、中文、精确/前缀/中间、罕见词、无命中、跨卷、`max=8` 和 `max=1000`。
- 区分冷启动、索引就绪后的首次查询和稳定暖查询；暖查询至少重复 30 次，并报告 P50、P95、最大值和样本数。
- 搜索采样记录端到端延迟、扫描节点数、名称候选数、进入 Top-K 数、路径构造次数、结果数、截断状态、generation 与响应字节数；当前协议无法提供的字段标记为“G1 待补”，不得伪造。
- 内存采样分别记录三个进程的 Private Working Set 与 Working Set，并记录 indexer 自报内存、节点数、名字池容量、卷数和缓存版本；总量按同一时间点三个进程相加。
- 环境记录包含 commit、构建命令、`[profile.release]` 设置、Windows build、CPU、物理内存、杀软/后台 I/O 说明、卷规模、索引就绪判断和采样时间。
- 单独测量“扫完所有卷全部节点”的裸耗时（不做提前终止），作为 G1 取消提前终止后的地板成本，也是判断是否必须引入并行搜索的依据。
- 采集 `opt-level = "z"`（现状）与 `opt-level = 3` 两组延迟对照，供 G1 做体积/延迟取舍；本阶段只提供数据，不改 `Cargo.toml` 的提交值。
- 把 `.trellis/spec/backend/quality-guidelines.md` 的 “Memory Acceptance” 小节从两进程口径改写为三进程口径，并附本阶段实测值。这是本阶段唯一允许的 spec 修改。
- 基准脚本不得依赖人工秒表或 GUI 点击；需要管理员权限的服务步骤与普通用户查询步骤分离，并写明前置条件。
- 原始样本与聚合结果分离；聚合不得删除异常值，只能附带解释。
- 不调整排序、协议、缓存、索引结构或 UI 行为。

## Acceptance Criteria

- [x] 在 Windows 11 x64 的当前验收机上，从干净 Release 构建到生成基线报告有完整可复跑命令。
- [x] 同一查询集可分别运行 `max=8` 与 `max=1000`，输出样本级原始数据和 P50/P95/max 聚合。
- [x] 三个目标进程均被单独采样，报告给出同一时刻总内存并明确 Private Working Set 与 Working Set 口径。
- [x] `.trellis/spec/backend/quality-guidelines.md` 的 “Memory Acceptance” 已改写为三进程口径，并引用本阶段实测值；改写前后的口径差异在任务记录中写明。
- [x] 报告包含 commit、Windows build、硬件、`[profile.release]` 设置、卷/节点/名字池/缓存信息和索引就绪状态。
- [x] 报告含全量裸扫描耗时，以及 `opt-level` `"z"` 与 `3` 的延迟对照；两者都标注为 G1 的决策输入而非结论。
- [x] 旧 38MB、5-10 倍、41-45MB 等数字未被描述为当前实测事实。
- [x] 基线资产不会触发索引重建、写入产品配置或改变用户文件。
- [x] `cargo test --manifest-path src/prism-core/Cargo.toml`、Clippy `-D warnings` 和 WPF Release 构建通过，或已有失败被逐条记录且证明与本阶段无关。

## Out Of Scope

- 搜索排序或协议修复；
- 压测结果驱动的性能优化；
- 新增产品遥测、后台上报或联网；
- 把 100MB 的**数值**改小。按三进程重新定义门槛的**统计口径**属于本阶段交付物，不属于收紧数值；
- 修改 `Cargo.toml` 中提交的 `opt-level`；本阶段只测对照数据，取舍由 G1 决定。

## Dependencies

无。G0 完成并归档后，G1 才能启动。
