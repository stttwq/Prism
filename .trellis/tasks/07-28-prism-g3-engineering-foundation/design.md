# Design

## Process And Thread Boundaries

```text
WPF -> broker IPC -> typed dispatcher
                       |-> async search/application services
                       \-> bounded queue -> dedicated STA Shell worker -> Shell/COM

broker -> read-only bounded search request -> LocalSystem indexer
```

STA worker 拥有 COM apartment 和 Shell 对象生命周期。IPC handler 只提交 typed request 并等待 one-shot 结果；关闭时停止接收、处理/取消有界队列、在线程内释放 COM。任何需要 UAC 的动作通过 Windows 正式 elevation 机制，不把 broker 整体常驻提升。

## Typed Contracts

result kind 和 action target 使用稳定线格式，内部 enum 与协议字符串由显式 converter 隔离。target 至少区分 file、directory、application、window、web，payload 按类型限长并验证；未知类型不执行，仅返回 unsupported。

版本升级遵循 G1 reader-first。settings/history/cache metadata 各自有 schema，不共享一个全局版本号。

## Exclusion Flow

机器硬排除由 indexer 本地固定规则执行；用户规则由 WPF/broker 校验、限量、规范化为只读快照并随查询发送。indexer 不持久化用户规则，只对当前请求在 Top-K 前过滤。无效规则返回明确验证错误，不回退为任意全局排除。

## Logging

每进程独立 rolling writer 和文件名，结构字段使用稳定 event id。默认只写计数、耗时、generation、错误类别和脱敏 id。诊断模式单独启用并标明敏感性；即使 writer 初始化失败也只降级到 Event Log/无文件日志，不终止服务。

## Cleanup And Rollback

先迁移测试并证明生产引用为零，再删除旧模块。公共 Shell adapter、STA executor、typed protocol、排除和日志各自独立提交。回滚时保留 reader 兼容和数据 schema 读取能力，不能恢复 LocalSystem 执行用户动作。
