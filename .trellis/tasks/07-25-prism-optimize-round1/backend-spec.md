# Backend Spec — Everything 式实时索引

## 适用范围

本规范约束 R1 的 Rust broker、Windows 索引服务、NTFS 层级索引、内部 IPC 与安装器接线。R2–R5 已完成行为不得回归。

## 强制架构

- `Prism.exe`：普通用户 WPF。
- `prism-core.exe`：普通用户 broker，拥有 apps/web/actions/settings 和前端 IPC，不拥有全盘文件索引。
- `prism-indexer-service.exe`：LocalSystem Windows Service，只拥有 NTFS 索引、USN 监听、文件搜索和 ProgramData 缓存。
- 禁止把 clipboard、ShellExecute、用户 settings 或任意文件执行带入服务。
- 禁止 broker 与服务同时常驻完整索引。

## 索引不变量

- 每卷 `nodes[record_number]` 唯一表示当前 FRN sequence 的一个 MFT 节点；slot 更新必须校验 sequence。
- NodeSlot 目标大小 ≤12 bytes；任何字段扩张必须重新计算 616,223 与 2,000,000 节点内存。
- 路径只由 `mount_path + parent chain + name` 构造，不为所有节点缓存完整路径。
- 一般目录 rename/move 只能更新目录节点，不允许扫描或重写后代。
- record number > `u32`、父链 >64、循环、断链或稀疏槽异常都必须显式失败/自愈，不得截断或猜测根路径。
- 所有 Win32 buffer 解析先验证记录长度、版本、偏移、UTF-16 长度与输出边界；unsafe 块必须有中文不变量注释。

## USN 与一致性不变量

- MFT 枚举前采样 journal id/NextUsn，发布前重放到高水位。
- 事件批次内按 Journal 原始顺序处理 rename old/new。
- 节点变化、next_usn 与 generation 在同一写锁临界区提交；位点绝不能领先索引内容。
- 快照必须同时序列化索引与位点；释放索引锁后才写磁盘。
- 健康状态禁止周期性 MFT/walkdir 重建。仅缓存损坏、journal 失效、断链不变量或排除边界跨越可请求重建。
- 重建由单一协调器串行化；新状态发布前旧状态持续服务搜索。

## 服务 IPC 不变量

- 管道固定为 `\\.\pipe\prism-indexer-v1`，拒绝远程客户端，首包版本握手。
- 只允许 `hello/status/search/wait_generation`；未知命令返回错误，不 panic。
- 每个请求必须对应完整响应；长轮询使用独立连接，不占用 broker 正常 search 连接。
- 服务返回路径字符串给 broker，但不执行、不打开、不写入路径。
- 服务不可用时 broker 仍服务 apps/web/actions，并返回结构化索引错误。

## 缓存与磁盘

- 机器索引目录 `%ProgramData%\Prism`；用户 settings 目录不变。
- cache version v5，与 v3/v4 隔离，不迁移旧数据。
- 写入 `index-v5.bin.tmp`，flush 成功后原子替换；损坏快照自动重建。
- 检查点只在首建、优雅停止、60 分钟或 100,000 事件；运行健康时不得全盘定时扫描。
- 名称池垃圾优先内存压缩，不以定时磁盘重建代替内存管理。

## 安装与权限

- 安装器可在安装/升级时请求管理员权限；`Prism.exe` manifest 继续 `asInvoker`。
- 服务 LocalSystem、自动启动，失败恢复策略最多有限重启，避免崩溃循环。
- 升级必须先停止服务再覆盖 binary；卸载必须停止/删除服务后删文件。
- 卸载不删除 NTFS USN Journal；ProgramData 缓存默认保留。

## 测试门禁

- 层级模型：create/delete/sequence reuse/file+directory rename/move/断链/循环/Unicode/排除边界。
- USN parser：截断 header、零长度、越界 filename、未知 major version、混合 reason。
- 一致性：构建窗口重放、停机重放、journal wrap、缓存损坏、并发 search + batch apply。
- IPC：协议版本、远程拒绝、只读命令集、服务断线重连、长轮询隔离。
- 安装：首次安装、覆盖升级、卸载重装、中文路径、Program Files、普通用户启动无 UAC。
- 性能：AC2/AC3/AC4 延迟、AC7 磁盘行为、AC9 三进程内存与 10,000 事件增长。

## 禁止事项

- 禁止新增定时全量扫描作为“安全兜底”。
- 禁止把完整路径常驻到每个节点。
- 禁止用 `HashMap<u64, Node>` 作为百万节点主存储；异常稀疏卷需单独设计并重新过内存门禁。
- 禁止服务执行用户动作或信任未经版本握手的 IPC。
- 禁止修改既有前端 IPC 字段语义；新增字段必须可选。
- 禁止未测量就声称“零延迟”或“低内存”。
