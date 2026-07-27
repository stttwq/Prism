# Design — Everything 式实时索引

## 进程与权限边界

```text
Prism.exe (普通用户, WPF)
  ├─ 现有请求管道 ──> prism-core.exe (普通用户 broker)
  │                    ├─ apps / web / actions / settings
  │                    └─ 文件搜索 RPC ──> prism-indexer-service.exe (LocalSystem)
  └─ 事件管道 <──────── index_changed generation

prism-indexer-service.exe
  ├─ MFT 初建 / USN Journal 监听
  ├─ 内存层级索引与文件搜索
  └─ %ProgramData%\Prism\index-v5.bin
```

- 不能提升现有 broker：剪贴板、ShellExecute 和用户配置必须留在交互用户会话。
- 服务只处理机器级文件名索引和只读查询，绝不执行搜索结果路径。
- Rust crate 重构为共享 library + 两个 binary：保留 `prism-core`，新增 `prism-indexer-service`。服务同时支持 SCM 模式和 `--console` 开发模式。

## 紧凑层级索引

每个 NTFS 卷独立维护：

```rust
struct VolumeIndex {
    volume_id: VolumeId,       // 卷 GUID + serial，盘符变化后仍可识别
    mount_path: String,        // 当前主挂载点，例如 C:\
    journal_id: u64,
    next_usn: i64,
    root_record: u32,
    nodes: Vec<NodeSlot>,      // 下标 = MFT record number
    names: Vec<u8>,            // UTF-8、NUL 结尾，增量追加
}

#[repr(C)]
struct NodeSlot {              // 目标 12 bytes
    parent_record: u32,
    name_off: u32,
    sequence: u16,
    flags: u16,                // present / directory / excluded
}
```

- `record_number = FRN & 0x0000_FFFF_FFFF_FFFF`，`sequence = FRN >> 48`。record number 超过 `u32`、节点槽稀疏率异常或内存估算越界时拒绝该卷的紧凑快路径并记录明确错误，禁止截断。
- 搜索只扫描 present、非 excluded 节点的名称；最多对命中项沿 `parent_record` 链构造路径，深度上限 64，检测断链和循环。
- create 覆盖/新增对应 slot；delete 清 present；rename/move 更新同一 slot 的 name/parent。目录后代引用父 record，因此一般目录改名不需要触碰后代。
- 高噪声目录：所有目录节点保留以维持父链；排除子树中的文件节点不保留名称。目录跨越排除边界时触发一次自愈重建，这是普通目录操作唯一允许的重建例外。
- 名称池增量追加。垃圾超过 `max(8MB, 初建池 25%)` 时仅在内存中压缩 live names 并原子换池，不读取磁盘；压缩需记录暂停时间与瞬时内存。

## 初建与一致性

单卷构建顺序：

1. 枚举固定卷并用 `GetVolumeInformationW` 严格确认 NTFS，取得卷 GUID、serial 和挂载点。
2. 打开卷并 `FSCTL_QUERY_USN_JOURNAL`；Journal 不存在时创建 32MB/8MB allocation delta，不缩小已有 Journal。
3. 先记录 `{journal_id, NextUsn}`，再用 `FSCTL_ENUM_USN_DATA` 填充 slots/name pool。
4. 第二遍解析排除状态与根节点，不构造所有完整路径。
5. 从步骤 3 的 NextUsn 重放到当前高水位；若期间 Journal wrap，则丢弃本次构建并重试一次，仍失败则保持旧索引并报告 degraded。
6. 将完整 `IndexState` 保存为 v5 快照后原子发布；旧索引直到发布完成仍可搜索。

缓存中的索引与每卷位点必须来自同一个读锁快照。序列化可持读锁，磁盘写入必须释放锁后执行；使用 `index-v5.bin.tmp` + Windows 原子替换。v3/v4 不迁移，服务首次启动重新建立 v5。

## USN 实时流水线

- 每卷一个 `spawn_blocking` 读取循环，`READ_USN_JOURNAL_DATA_V0` 只订阅 create/delete/rename old/rename new，`ReturnOnlyOnClose=0`。
- 输出缓冲逐记录验证 `RecordLength`、版本、文件名偏移/长度和 UTF-16 边界；只接受 NTFS `USN_RECORD_V2`。
- 最多等待 50ms 或累计 256 条后提交，先到为准。rename old/new 保持原始顺序并在同一写锁批次内应用。
- 节点变更、目录父关系、`next_usn` 和 `generation` 在同一写锁内推进。断链不会猜测路径；记录聚合错误并请求自愈。
- `ERROR_JOURNAL_ENTRY_DELETED`、journal id 变化、checkpoint 早于 `FirstUsn` 进入单一重建协调器。协调器合并请求，全局任何时刻最多一个重建。
- 健康状态不做周期 MFT/walkdir 扫描。检查点条件：首建、SCM stop、60 分钟、100,000 事件。

## 服务 IPC 与前端刷新

内部管道：`\\.\pipe\prism-indexer-v1`。

- 服务端拒绝远程客户端，DACL 只允许 SYSTEM 与本机交互用户读写。
- 首包必须 `hello {protocol:1}`；不兼容立即断开。
- 请求仅包含 `status`、`search {query,max}`、`wait_generation {after,timeout_ms}`。
- broker 使用短请求连接做 search/status；服务不可用时 broker 保持 apps/web/actions，并在现有 `results` 中返回可选 `index_error`。
- WPF 使用独立服务连接长轮询 `wait_generation`；窗口可见且查询非空时收到 generation 变化，100ms 去抖后调用现有 broker 搜索路径。现有前端请求/响应管道不允许插入未经请求的消息，broker 不重复订阅 generation。

## 安装、升级与运行

- `dist/prism.iss` 增加 `prism-indexer-service.exe`，安装器改为管理员权限；应用 manifest 仍为 `asInvoker`。
- 安装/升级顺序：停止旧服务 → 覆盖文件 → create/configure service（LocalSystem, auto start）→ 设置 failure restart → start → 验证状态。
- 卸载顺序：停止并删除服务 → 终止 broker/UI → 删除程序文件。保留 `%ProgramData%\Prism` 缓存以支持升级/重装；不删除 NTFS USN Journal。
- 用户设置仍在现有 per-user data dir；机器索引只在 ProgramData。旧 per-user `index.bin` 被 broker 忽略但不主动删除。
- 无服务/便携开发模式可启动 `prism-indexer-service --console`，但不承诺普通权限 USN；正式安装验收只认 SCM 服务模式。

## 兼容、内存与回滚

- 前端外部协议只增加可选 `index_error/index_generation`，原字段语义不变。
- 当前 616,223 条样本下：NodeSlot 约 7.1MB，名称池预计 ≤12.39MB。索引从 broker 移到服务，不双份常驻。
- 硬门槛：UI ≤30MB；broker + service ≤70MB；总计 ≤100MB。池压缩允许短时峰值但必须记录，若总峰值超过 120MB 则改为重启时压缩。
- 回滚以安装器停止服务、恢复旧 broker binary 为边界；v5 缓存与旧 v3 隔离，不影响旧版本启动。

## 已知限制

- 多用户 Windows 主机上，机器级索引可能向其他本地交互用户暴露文件名；本轮不做逐结果 ACL 过滤。
- 完整硬链接多名称一致性、junction、ReFS、非 NTFS 与网络盘留待后续。
- 服务安装扩大了安装/升级测试面，是 Everything 级普通权限实时更新的必要成本。
