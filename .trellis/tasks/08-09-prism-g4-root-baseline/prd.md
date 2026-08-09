# G4 root 作用域性能基准补采

## Goal

补采 G4（`07-28-prism-g4-host-integration`，已归档）步骤 8 的唯一未完成项：
G0 root 作用域性能基准的正式数据。G4 的功能与兼容矩阵已全部完成并签署，
只差这个性能验证数字，故单独立项，不阻塞任何在建功能。

## 要回答的问题

G4 PRD 第 15 行：

> 只有 G0/G1 基准证明父链方案无法满足 P95，才允许另立内存明确的祖先/子树缓存设计。

即：**加了 root 限定之后，父链祖先验证会不会拖垮 P95？** 需要正式数据才能签署。

## 已有的方向性证据（不足以签署）

`artifacts/bench/g4-root-20260807/`，配对法采集，**仅 31% 请求干净、每组
10–14 样本**（目标 40）：

- 配对延迟差中位数 **+2.0ms**，最坏 +28.7ms，无数量级恶化
- `path_constructions` 证明剪枝生效：`deep_system32` 1000 → 441、
  `shallow_small` 624 → 246

即 root 减少祖先验证工作量，代价是每候选一次前缀比较，净效果基本持平。
**当前证据不支持引入祖先/子树缓存**，但样本量不足以正式签署。

## 为什么当时没采到

1. **2026-08-07**：Windows 更新（`SetupHost`，累计 CPU 1380s）持续重写磁盘，
   索引 6 秒推进 2059 个世代。`Invoke-SearchBaseline.ps1` 按设计中止——绝对
   延迟只有在安静机器上才可跨轮比较，这个前提**不得为了拿到数字而放宽**。
2. **随后重装系统**：`C:\ProgramData\Prism` 被抹掉，索引重建后
   `memory_bytes` 只有 66MB，而 8月7日是 171MB。规模差一半 → 搜索更快 →
   与历史基线不可比。

另有一次教训：第一轮实际采集成功过，但聚合层有 bug，我清目录重采而没有对
已有原始 JSONL 重跑聚合，导致唯一一份干净数据丢失。详见
`docs/排查踩坑记录.md` 第四节。

## Requirements

补采前提（全部满足才开始，否则数据不可比）：

- `memory_bytes` ≥ 150MB（索引长回完整规模）
- 无系统更新 / 大批量解压 / 编译等外部负载
- `Get-IndexerStatus` 的世代在 `GenerationStableSeconds` 内不移动
- Prism 前端与 Directory Opus 均已关闭
- `.\scripts\prism-build.ps1 -VerifyOnly` 通过（确认跑的是当前构建）

执行命令：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench\Invoke-SearchBaseline.ps1 `
  -OutputDirectory artifacts\bench\g4-root-YYYYMMDD `
  -ReleaseDirectory dist `
  -SecuritySoftwareNotes '...' -BackgroundIoNotes '...' `
  -EffectiveOptLevel z
```

工具已就绪，无需改动：`queries.json` 含 5 条 root 夹具（深目录高候选、
System32、小子树、无命中、盘根边界），驱动脚本透传 `root` 并记录
`root` / `root_rejection`，聚合含 `workload.path_constructions`。

`Invoke-RootScopeComparison.ps1` 是配对对照工具，容忍世代移动，
**不能替代正式基准**。

## Acceptance Criteria

- [ ] 每个 query/max 组 ≥ 30 个暖样本，且 `root_rejections` 为空
- [ ] 按 PRD 阈值判定：`max=8` P95 ≤100ms、`max=1000` P95 ≤300ms
- [ ] 对比 root 臂与全局臂的 `path_constructions`，给出「是否需要祖先/子树
      缓存」的明确结论，并写进 `.trellis/spec/backend/quality-guidelines.md`
- [ ] 若结论是「需要缓存」，另立设计任务，**不在本任务内改实现**

## Notes

不在范围：

- 不改 root 作用域的搜索语义
- 不改宿主联动行为
- 不动两个 adapter 的默认值（当前默认关闭，是产品决策）
