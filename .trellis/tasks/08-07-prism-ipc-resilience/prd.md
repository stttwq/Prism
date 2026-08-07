# IPC 韧性：broker 静默崩溃与 indexer 单实例管道饱和

## 来源

G4 步骤 8 采集 root 作用域性能基准时实测暴露。两个缺陷都不属于 G4 范围
（G4 只负责宿主联动），但都会让用户看到「搜索失败」，优先级高于剩余的 G 阶段。

采集环境：Windows 更新（`SetupHost`，累计 CPU 1380s）持续重写磁盘，
索引在 6 秒内推进 2059 个世代。这是真实用户会遇到的场景——系统更新、
大批量解压、Git 检出大仓库都会产生同级别的 USN 洪峰。

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
