# G3 工程地基与权限边界

## Goal

在扩展宿主联动和文件动作前，清理旧索引测试链、建立普通权限 broker 的公共 Shell/COM 执行边界、固化协议类型与版本策略、可配置排除规则和可靠脱敏日志。

## Requirements

- 强依赖 G1 稳定搜索与协议契约。
- 迁移 `index.rs` 旧链仍有价值的测试到当前 `hierarchy/indexer_client/indexer_runtime` 生产路径，再删除无生产用途的旧模块与孤立占位文件；测试数量下降必须逐项说明等价覆盖。
- 抽取公共 Shell 能力，统一 reveal、属性、打开方式、外部启动和后续文件操作的目标解析与错误分类。
- broker 建立专用 STA Shell worker：在线程内初始化/释放 COM apartment，所有 Shell COM 调用串行调度，Tokio worker 不临时承担 apartment 所有权。
- Shell 错误稳定区分成功、用户取消、拒绝访问、目标失效、冲突、权限提升和系统错误；取消不记为故障。
- LocalSystem indexer 禁止执行 Shell、剪贴板、用户命令、联网请求或读取特定用户 LocalAppData。
- Rust/C# 共享稳定 result kind 与 typed action target 语义；未知 kind 安全降级，不再只靠 `execute_id` 字符串猜测 URL、路径、应用或窗口。
- settings/history/favicon metadata 等持久用户数据带 schema 版本，缺字段使用安全默认，写新格式前验证。
- 保留机器级硬排除及理由；`Windows\Installer` 保持父路径语义。用户排除保存在普通用户设置，由 broker 作为有界只读过滤快照随 search 传给 indexer，在 Top-K 前生效。
- broker 与 indexer 使用不同日志文件；日志有 level/event/elapsed/generation 等结构字段，默认不记录完整 query、路径、窗口标题或网页查询。
- 若要求按大小轮转，所选 writer 必须真实支持；日志目录不可写或轮转失败不得阻塞搜索，服务启动失败保留 Windows Event Log 兜底。

## Acceptance Criteria

- [ ] 生产依赖图不再编译无生产用途的旧索引实现，迁移测试覆盖当前生产链。
- [ ] Shell/COM 调用全部经过普通权限 broker 的专用 STA worker，apartment 初始化、串行执行、取消和关闭有自动化/机器测试。
- [ ] LocalSystem indexer 协议没有 Shell、任意命令、联网、配置写入或 sidecar 重建入口。
- [ ] typed target、稳定 kind、缺失可选字段、未知 kind 和明确不兼容组合有兼容测试。
- [ ] 用户排除在 Top-K 前生效，机器硬排除和 `Windows\Installer` 语义无回归。
- [ ] broker/indexer 日志不争用文件；只读磁盘、目录创建失败、轮转失败和异常退出不影响主流程。
- [ ] 默认日志经过测试确认不含完整 query、路径、窗口标题和网页查询。
- [ ] Rust tests、Clippy `-D warnings`、C# tests 和 WPF Release build 全部通过。

## Out Of Scope

- 实现 G6 的完整文件动作；
- DLL 注入、JSON/DLL 插件或任意 Shell verb；
- indexer 读取每用户设置目录；
- 把日志当遥测上传。

## Dependencies

强依赖 G1。G4 和 G6 依赖本任务归档后的 Shell、协议、日志与权限边界。
