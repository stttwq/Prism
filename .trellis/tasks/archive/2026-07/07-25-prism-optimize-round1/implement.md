# Implement — Everything 式实时索引

## 实施顺序

每一步都必须保持可编译并独立验证；不把服务、模型和安装器一次性堆到最后。

1. **提取共享 Rust library，保持行为不变**
   - 新建 `lib.rs`，把现有模块从 binary root 移入 library；`prism-core` 仍按原方式启动、加载 v3、通过现有管道搜索。
   - 验证现有测试、clippy、pipe roundtrip 全绿，作为结构重构回滚点。

2. **实现层级索引模型（平台无关）**
   - 新增 `NodeSlot/VolumeIndex/IndexState`、FRN 拆分、slot 容量守卫、名称池、父链路径构造和排除逻辑。
   - 单测覆盖：文件 create/delete、FRN sequence 复用、文件/目录 rename 与 move、100 后代目录改名无需遍历子树、断链/循环、根目录、Unicode、排除边界。
   - 用当前 616,223 条样本做内存估算和搜索基准；不满足 70MB 后端预算则停止后续接线。

3. **实现 NTFS 构建、v5 缓存与重放**
   - 把 Win32 卷访问/MFT 解析移到独立 `ntfs` 模块；查询/按需创建 Journal，枚举前取位点。
   - 实现 USN buffer 安全解析和对本地 `IndexState` 的构建窗口重放。
   - v5 使用 ProgramData、临时文件 + Windows `ReplaceFileW`/等价原子替换；单测覆盖 roundtrip、v3/v4 拒绝、损坏恢复、索引/位点一致性。

4. **新增 Windows 索引服务和只读 IPC**
   - 新增 `prism-indexer-service` binary、SCM 生命周期、console 模式、服务状态/停止检查点。
   - 实现本机版本化管道与 DACL，协议仅 `hello/status/search/wait_generation`。
   - 协议测试覆盖版本拒绝、远程拒绝、未知/写操作拒绝、并发搜索、服务停止。

5. **接入 USN 长监听和重建协调器**
   - 每卷长轮询，50ms/256 批量提交；同锁推进节点、位点和 generation。
   - 实现 journal wrap/id 变化/断链的单一重建协调器，健康状态不启动周期扫描。
   - 自动化脚本创建随机文件和目录，轮询服务 search 记录至少 30 组延迟分布。

6. **把 broker 从索引拥有者改为服务客户端**
   - `prism-core` 删除本地常驻 FileIndex，文件搜索转发服务，保留 apps/web/actions 和现有排序/`max` 语义。
   - 服务断开时返回可选错误且 apps/web 可用；重连后自动恢复。
   - 兼容测试覆盖现有前端对新增可选字段的忽略。

7. **接入前端 generation 事件刷新**
   - WPF 通过独立服务连接长轮询 generation；窗口可见、查询非空时 100ms 去抖后走现有 broker 搜索路径。
   - 不复用当前串行请求连接，避免长轮询破坏一写一读纪律。
   - 手测固定查询下创建/删除结果自动出现/消失，窗口隐藏后不产生持续搜索。

8. **安装器接入服务**
   - 发布第三个 exe；Inno 改管理员安装，完成服务停止/升级/创建/恢复策略/启动和卸载删除。
   - 验证 Program Files、中文路径、覆盖升级、卸载重装；UI 启动保持无 UAC。

9. **完整验收与记录**
   - AC1–AC10 全部执行；记录延迟 P50/P95/max、缓存大小、三进程私有工作集、10,000 事件后池增长、30 分钟磁盘行为。
   - 运行 Trellis check；任何 correctness 或内存门禁失败回到对应步骤，不以降级定时全盘扫描掩盖。

## 验证命令

```powershell
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml -- -D warnings
cargo build --release --manifest-path src/prism-core/Cargo.toml --bins
dotnet build src/Prism -c Release
powershell -ExecutionPolicy Bypass -File scripts/pipe-roundtrip-test.ps1
& 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe' dist\prism.iss
```

新增脚本在实施时确定名称，至少包含 service protocol roundtrip、USN latency、停机重放和内存采样。

## 风险与回滚点

- 步骤 1 只做 crate 边界重构；若现有行为变化，可单独回滚，不影响后续设计。
- 步骤 2 是内存与正确性闸门；未证明目录改名 O(1) 且样本内存达标前，不进入服务开发。
- 步骤 4 的服务只读协议是安全闸门；不得把现有 actions/run_action 搬入服务。
- 步骤 5 的索引、位点、generation 原子性是数据正确性闸门；禁止用日志掩盖漏事件。
- 安装器变更可独立回滚；v5 缓存使用新路径/版本，旧版本仍能忽略并加载自己的 v3。
- 多用户文件名隔离、硬链接、非 NTFS 明确延期，不在施工中临时扩展。

## 启动实施前检查

- PRD、design、backend-spec、implement 已对齐 Everything 式架构。
- `implement.jsonl` 与 `check.jsonl` 各有真实 spec/research 条目。
- 用户已在看到最新最终规划摘要后的下一条消息中明确批准实施。
