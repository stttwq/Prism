# Implement — Prism 第一轮优化

## 执行顺序（每步可独立验证/提交）

1. **实测 load_cache 耗时（AC4）**
   - 运行 release 后端，抓日志「加载耗时 {}ms」，记入 `research/load-cache-ms.md`；>2s 则 R2 文案覆盖加载期。
2. **R5 more 行实装（最小、先热身）**
   - `SearchViewModel.ApplySearchResponse`：`resp.Items.Count >= max` 才加 More 行。
   - `ResultList`：单击 more 行触发 ItemInvoked。
   - 验证：dotnet build；手测 AC7。
3. **R4 差量刷新**
   - `ResultList.Items` setter 改内部 ObservableCollection + 键控 diff；ItemsSource 只赋一次；行未变不清 icon.Source；条数不变跳过高度重算。
   - 验证：手测 AC6（abc→ab 退格观察）；选中保持逻辑回归（上下键+退格）。
4. **R3 右键菜单**
   - ResultList `MouseRightButtonUp` → 选中 + `ContextMenuRequested` 事件。
   - SearchWindow：GetActionsAsync → ContextMenu（主题化样式入 Themes/Tokens）；点击复用 RunActionAsync 路径（抽公共方法，ActionPanel 同步改用）。
   - 验证：AC5 手测（深浅色各一遍，五动作全过）。
5. **R2 后端进度字段**
   - `index.rs`：`scanned_count` AtomicU64 + `total_estimate`；`build_full_index` 循环递增。
   - `ipc.rs`：results 可选 `index_progress`；cargo test 补 1 例序列化。
   - 前端：StatusMessage 用进度文案。
   - 验证：删 data 目录冷启动，手测 AC3。
6. **R1 USN 实时监听（核心，最大块）**
   - a. `index.rs`：核实 full_path 拼接方式（parent 链 vs 预拼）；实现 `upsert_entry`/`remove_entry`（含噪声黑名单复用、目录 rename 语义）。**risk 点：若预拼字符串，目录 rename 需子树更新——先确认再动**。单测覆盖 upsert/remove/rename。
   - b. 新建 `usn_watch.rs`：逐卷记录 NextUsn → 长轮询任务 → reason 过滤 → 500ms 批量应用；journal 溢出触发全量重建；非管理员降级标记。
   - c. `main.rs`/`config.rs`：接线 + `usn_watch` 开关（缺省 true）+ `index_refresh_secs` 缺省 300→60，全卷 watch 成功时定时重建改 3600。
   - 验证：管理员运行手测 AC1（新建/改名/删除 各 5s 内）；普通权限手测 AC2；`cargo test`、`cargo clippy -- -D warnings`。
7. **收尾验收**
   - AC8：内存复测（对照 spec 记录的验收方法，后端 ≤70 / 前端 ≤30）。
   - 全套门禁：`cargo test` / `cargo clippy` / `dotnet build -c Release`。

## 验证命令

```bash
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml -- -D warnings
dotnet build src/Prism -c Release
```

## 风险与回滚点

- 步骤 6a 的 full_path 结构核实是 R1 的前置闸门；若结构不利，先只做文件级增量、目录 rename 触发局部重建，范围写回 design.md。
- USN watch 出问题：config `usn_watch:false` 一键回退 1.0.0 行为。
- 每步单独 commit，可按步回滚。
- 步骤 2/3/4/5 纯前端或小改，先行合入不依赖步骤 6。

## task.py start 前检查

- prd/design/implement 三件套齐（本文件）；inline 工作流，JSONL 门禁跳过。
- 用户已批准最终规划摘要。
