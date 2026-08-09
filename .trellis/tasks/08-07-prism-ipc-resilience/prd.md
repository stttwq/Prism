# IPC 韧性：broker 静默崩溃与 indexer 单实例管道饱和

## 来源

G4 步骤 8 采集 root 作用域性能基准时实测暴露。两个缺陷都不属于 G4 范围
（G4 只负责宿主联动），但都会让用户看到「搜索失败」，优先级高于剩余的 G 阶段。

采集环境：Windows 更新（`SetupHost`，累计 CPU 1380s）持续重写磁盘，
索引在 6 秒内推进 2059 个世代。这是真实用户会遇到的场景——系统更新、
大批量解压、Git 检出大仓库都会产生同级别的 USN 洪峰。

## 进展（2026-08-09）

**缺陷 B 已修复并确证。** 缺陷 A 的**代码缺陷已修，但因果链未证明**——A/B 对照
显示未修复的旧二进制在更猛的洪峰下同样零失败，即当前探针**复现不了**原始故障。

| | 状态 | 依据 |
| --- | --- | --- |
| B panic 无日志 | **已修复** | 探针触发 panic，日志得到 `panic at tools/panic-probe.rs:23 message_e395…`；位置明文、路径已哈希 |
| A ERROR_PIPE_BUSY | **未证明** | A/B：旧二进制 5831 世代/秒下 150 轮零失败；新二进制 5513 世代/秒下同样零失败 |

### 为什么复现不了

合成洪峰与 8月7日的现场有两处关键差异，都指向「CPU 与索引规模」而非 USN 速率：

1. **CPU 未饱和**。8月7日是 Windows 更新（`SetupHost`，累计 CPU 1380s）在解压安装，
   CPU 与磁盘同时打满。合成洪峰只产生 USN 记录，CPU 有大量余量——而工作线程饥饿
   恰恰需要 CPU 竞争。
2. **索引规模差一半**。8月7日 `memory_bytes` 是 171MB；重装抹掉 `C:\ProgramData\Prism`
   后重建只有 66MB。索引更小 → 搜索更快 → 持锁更短 → 竞争更弱。

USN 速率反而不是瓶颈：合成洪峰 5800/秒远超当时的 340/秒，仍不触发。

### 代码缺陷本身是真实的

与能否复现无关，以下三处在 async 运行时里持锁阻塞，客观上是错的：

- `IndexerRequest::Status` 曾直接在工作线程调 `state.status()`，而 `status()` 第一行
  就是 `index.read()`，还要遍历索引与拼音侧车累加 `memory_bytes`。`Search` 早已用
  `spawn_blocking`，Status 没有，属遗漏。
- `wait_generation` 是 async 函数，却在循环里两次同步调 `self.status()`。
- accept 循环只保留 1 个监听实例，`connect()` 返回到 `create_pipe` 之间无人监听。

已分别改为 `spawn_blocking`、新增只读世代的 `generation()`、监听池扩到 4
（`JoinSet`）。这些改动可独立辩护，但**不得标记为「已修复 ERROR_PIPE_BUSY」**。

### 复现条件（下次遇到时按此采集）

真实触发需要同时满足：完整规模索引（`memory_bytes` ≥ 150MB）、CPU 接近饱和的
外部负载（系统更新 / 大批量解压 / 编译）、以及并发的 Status 轮询 + 重查询。
探针见 `artifacts/g4-test-build/probe-under-flood.ps1` 与 `ab-compare.ps1`
（后者需提权，会换服务二进制并在 `finally` 里恢复）。

---

## 缺陷 A：indexer 管道在高流失下返回 ERROR_PIPE_BUSY

**现象**：800 次搜索请求中 517 次失败于
`indexer service is unavailable: 所有的管道范例都在使用中。(os error 231)`，
另有 34 次 `indexer service request timed out`，合计失败率 69%。

**根因（待确认）**：`indexer_runtime.rs` 的 `serve` 循环只保留一个监听实例，
且在 `server.connect().await` 返回之后才 `create_pipe(false)` 建下一个。
两者之间存在无人监听的窗口。客户端侧 `indexer_client.rs::connect` 重试
5 次 × 20ms，窗口被拉长时会穷尽重试。

**关键反证**：单客户端连续压测（0ms 间隔、重查询 `root=E:\YX` + `max=1000`）
**零错误**。所以不是固定容量上限，而是索引吸收 USN 洪峰时 accept 循环被饿死。
`spawn_blocking` 池被搜索与重建任务占满是首要嫌疑。

**验收**：USN 洪峰期间（世代推进 > 300/s）连续 800 次搜索，
`ERROR_PIPE_BUSY` 为 0；若确实无法服务，必须是结构化降级而非裸错误字符串。

## 缺陷 B：broker 静默崩溃

**现象**：同一轮压测后 `prism-core.exe` 进程消失，只剩 indexer 服务。
`%LocalAppData%\Prism\broker.jsonl` 最后一条是 2026-08-04，崩溃当天无任何记录。

**影响**：前端下次搜索会重新拉起 broker（`PipeClient.EnsureBackendRunning`），
所以用户可能只感觉到「卡了一下」，但崩溃原因完全不可追溯。

**验收**：能复现崩溃并拿到栈或退出码；panic 必须落盘。

## 附带发现：日志消息被哈希，不可读

`C:\ProgramData\Prism\indexer.jsonl` 的条目形如
`{"event":"message_61bb0e1982dffed2","generation":null,"timestamp":null}`——
消息文本被替换成哈希，且 `timestamp` / `generation` 为 null。
排查缺陷 B 时因此拿不到任何线索。

需要确认这是刻意的隐私设计还是 bug。若是设计，至少要保留时间戳与世代，
否则日志无法用于事后排查。

## 不在范围

- 不改 G4 的宿主联动行为
- 不改 root 作用域的搜索语义
