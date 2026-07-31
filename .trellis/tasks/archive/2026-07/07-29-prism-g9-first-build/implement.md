# Implementation Plan

按"先正确性、后可观测、最后调优"排序。第 1–5 步是核心交付，第 6 步可为空。

1. [x] 读取 G0 基线的首建时间与卷规模数据，记录改动前的"首建到任何文件可搜"作为对照基准。
2. [x] 在 `ServiceState` 增加 `merge_and_publish(volume)` 与 `first_build_complete: AtomicBool`；保留现有 `publish()` 用于缓存命中与整表 rebuild。补单元测试：空索引首次 merge、后续 merge 累加、generation 每次递增、`wait_generation` 被唤醒。
3. [x] 在 `run()` 中对 `discover_volumes()` 结果做系统卷优先的稳定重排（R3），加确定性测试：构造乱序 descriptor 列表，断言系统卷在首位且其余相对顺序不变。
4. [x] 把首建改为逐卷循环：`build_volume` → `merge_and_publish` → 启动该卷 watcher → 更新进度计数。抽出单卷版 `start_watcher`，`start_watchers` 复用它。验证 `ready && building` 组合可正确表达。
5. [x] 加落盘门（R2）：`checkpoint()` 在 `first_build_complete == false` 时跳过并记日志；`run()` 退出前的 checkpoint 同样受门控。测试：首建中途请求 Stop → 断言未写缓存文件；重启后走完整重建。
6. [x] 加 `IndexerStatus.build_progress` 可选结构（R4），计数按枚举批次累加而非逐条；broker 透传；WPF 显示可解释进度。测试可选字段缺失时旧 reader 不变。
7. [x] 补 R5 的交互测试：部分索引下拿到结果 → 追加字符 → 断言重新请求后端而非本地过滤；并在 `is_truncated` 的文档/测试里写明它不含"索引未建完"。
8. [x] 补 R7 测试：首建期间应用与网页结果仍返回（锁定 `prefix_results` 在 indexer 调用之前的顺序）。
9. [x] R6 逐项评估并只保留有实测收益者：缓冲区上调、pending 重试实际遍数、系统卷优先 + 其余卷并行。每项单独提交，无收益项写明"已评估、放弃"及数据。
10. [x] 在验收机跑机器验收：多卷首建、逐卷可用时间、卷 A 发布后在卷 A 增删改文件的可见性、首建中途停服务、重启重建。
11. [x] 执行全套质量门；用 G0 脚本复测首建时间、稳态延迟与三进程内存，确认无回归。
12. [x] 更新 backend spec：逐卷发布契约、`ready && building` 组合语义、进度字段、落盘门。

## Completion Evidence (2026-07-31)

- Final machine run: `artifacts/bench/g9-20260731-final4/summary.json` (`pass=true`).
  Three fixed NTFS volumes; C: searchable at 1,400.585 ms while D:/E: still
  built; all volumes ready at 4,540.404 ms.
- Published-volume watcher: create 18.801 ms, rename 17.583 ms, delete 17.412 ms
  while only one of three volumes was published.
- Persistence: interrupted build wrote no cache; restart rebuilt all three volumes
  and reached ready at 4,381.377 ms. Completion is now published only after the
  complete cache save succeeds.
- Pre-G9 comparison: G0 did not record first-build timing. The backed-up installed
  pre-G9 service was therefore rerun on the same three volumes; first `ready` was
  1,657.039 ms (`legacy-first-build-summary.json`). G9 exposes C: at 1,400.585 ms
  instead of waiting for the legacy whole-index publication.
- Memory: `memory-summary.json` passed the 100 MiB three-process hard gate;
  maximum synchronized Private Working Set was 55,554,048 bytes.
- Steady search: `search-baseline/search-summary.json`; worst P95 was 90.478 ms
  for `max=8` and 92.785 ms for `max=1000`, within 100/300 ms limits.
- Automated gates: 114 Rust tests, Clippy `-D warnings`, 10 C# tests, WPF Release
  build, bench self-tests, PowerShell parse, and `git diff --check` passed.

## R6 Decision Record

- MFT output buffer: retained at 256 KiB. G0 contains no 1/4 MiB enumeration A/B
  result, and G9 produced no same-environment candidate comparison; therefore no
  measured benefit exists to justify changing memory/IO behavior.
- Pending hierarchy loop: retained with the 64-pass safety bound. Records remain
  FRN-sorted and no acceptance run produced an unreachable/depth failure; no
  measured hot spot or alternative result justifies extra machinery.
- Multi-volume parallelism: rejected for G9. The measured sequential timeline was
  C: 1.401 s searchable and all volumes 4.540 s; no controlled physical-disk A/B
  proves a benefit, while concurrency would add IO contention and cancellation
  complexity. System-volume-first serial order remains deterministic.

All three candidates were evaluated under the PRD's data gate and deliberately
left unchanged; G9's measured user benefit comes from per-volume publication.

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

机器验收另需：`scripts/indexer-service-install-test.ps1`、`scripts/indexer-usn-latency-test.ps1`，以及 G0 的首建计时与三进程内存脚本。首建计时须在删除 `%ProgramData%\Prism\index-v5.bin` 后进行，且每次只删缓存、不动服务注册。

## Review Gates

- 第 4 步（逐卷发布 + 逐卷 watcher）合并前必须先有第 2、3 步的测试通过，且证明"卷 A 发布后在卷 A 的变更可见"。这是本阶段唯一可能静默丢事件的地方。
- 第 5 步的落盘门必须实测"首建中途停止后磁盘无缓存文件"，不接受仅靠 `volumes.len()` 兜底。
- 第 9 步任何调优项若无法给出同环境前后对照，不得合并。
- `ready && building` 组合进入协议前，确认前端在该状态下不会把部分结果当完整集缓存（第 7 步）。

## Rollback Points

1. `merge_and_publish` + 系统卷排序（结构准备，行为不变）
2. 逐卷发布 + 逐卷 watcher（核心行为变更，独立回滚点）
3. 落盘门（不建议回滚，它修真实风险）
4. 进度字段 + 前端展示
5. R6 各调优项，每项一个提交

回滚第 2 项即可完全恢复"全部卷建完才发布"的旧行为，其余项互不依赖。
