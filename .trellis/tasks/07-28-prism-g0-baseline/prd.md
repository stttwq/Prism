# G0 可复现基线

## Goal

建立后续 G1-G8 共用的三进程 Release 内存、搜索延迟与工作量基线，停止把旧两进程 38MB 或其他未复测数字当作当前事实。本阶段只增加测量资产和基线记录，不改变产品行为。

## Background

- 当前架构是 WPF `Prism.exe`、普通权限 `prism-core.exe` broker、LocalSystem `prism-indexer-service.exe` 三进程和两条命名管道。
- 现行硬门槛是三进程总内存不超过 100MB；40-45MB 仅是复测后的优化期望。
- 首屏请求 `max=8`，展开请求 `max=1000`；暖查询目标分别是 P95 不超过 100ms 和 300ms。
- 基线必须绑定 commit、Release 二进制、Windows build、CPU/内存、卷与索引规模，不能只记录单次最快结果。

## Requirements

- 固定一套可版本控制的查询集，至少覆盖 ASCII、中文、精确/前缀/中间、罕见词、无命中、跨卷、`max=8` 和 `max=1000`。
- 区分冷启动、索引就绪后的首次查询和稳定暖查询；暖查询至少重复 30 次，并报告 P50、P95、最大值和样本数。
- 搜索采样记录端到端延迟、扫描节点数、名称候选数、进入 Top-K 数、路径构造次数、结果数、截断状态、generation 与响应字节数；当前协议无法提供的字段标记为“G1 待补”，不得伪造。
- 内存采样分别记录三个进程的 Private Working Set 与 Working Set，并记录 indexer 自报内存、节点数、名字池容量、卷数和缓存版本；总量按同一时间点三个进程相加。
- 环境记录包含 commit、构建命令、Windows build、CPU、物理内存、杀软/后台 I/O 说明、卷规模、索引就绪判断和采样时间。
- 基准脚本不得依赖人工秒表或 GUI 点击；需要管理员权限的服务步骤与普通用户查询步骤分离，并写明前置条件。
- 原始样本与聚合结果分离；聚合不得删除异常值，只能附带解释。
- 不调整排序、协议、缓存、索引结构或 UI 行为。

## Acceptance Criteria

- [ ] 在 Windows 11 x64 的当前验收机上，从干净 Release 构建到生成基线报告有完整可复跑命令。
- [ ] 同一查询集可分别运行 `max=8` 与 `max=1000`，输出样本级原始数据和 P50/P95/max 聚合。
- [ ] 三个目标进程均被单独采样，报告给出同一时刻总内存并明确 Private Working Set 与 Working Set 口径。
- [ ] 报告包含 commit、Windows build、硬件、卷/节点/名字池/缓存信息和索引就绪状态。
- [ ] 旧 38MB、5-10 倍、41-45MB 等数字未被描述为当前实测事实。
- [ ] 基线资产不会触发索引重建、写入产品配置或改变用户文件。
- [ ] `cargo test --manifest-path src/prism-core/Cargo.toml`、Clippy `-D warnings` 和 WPF Release 构建通过，或已有失败被逐条记录且证明与本阶段无关。

## Out Of Scope

- 搜索排序或协议修复；
- 压测结果驱动的性能优化；
- 新增产品遥测、后台上报或联网；
- 收紧 100MB 硬门槛。

## Dependencies

无。G0 完成并归档后，G1 才能启动。
