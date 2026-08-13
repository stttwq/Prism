# Design

## Typed Action Flow

```text
selected typed target -> list allowed built-in actions
                      -> optional WPF subflow/parameters
                      -> broker validation -> STA Shell worker
                      -> structured outcome -> history/UI/generation wait
```

action id 是稳定封闭枚举。broker 根据 target kind 重新验证 action 与参数，不信任 WPF 传来的路径/命令。所有目标在执行前重新解析存在性和类型，opaque application/link 信息由 broker 自己持有或重建。

## File Operations

复制、移动、删除使用 `IFileOperation` 并保留系统进度/冲突 UI。permanent delete 强制确认 flag，协议不提供跳过字段。用户取消映射 `Cancelled`；Windows 已完成部分/全部操作时返回可解释 outcome，WPF 不伪造原子回滚。

重命名参数只传新 leaf name，broker 拒绝路径分隔符、空名和超限。复制/移动 destination 是 typed directory target，不接受任意命令行。

## ZIP Adapter

压缩 adapter 按优先级选择压缩程序：

1. **设置自定义路径**：用户在 `settings.json` 的 `ZipProgram` 字段指定压缩程序可执行文件完整路径。若设置且文件存在，优先使用。
2. **自动探测**：设置未指定时，按已知注册表/安装路径探测本机已安装的 7-Zip（`7z.exe`）。
3. **Windows 内置回退**：两者都不可用时，使用 Windows Shell COM `CopyHere` 到 `.zip` folder 对象（系统内置，无需第三方依赖）。

无论哪条路径，输出固定 `.zip`，冲突交给系统确认。外部进程退出码、取消和目标生成分别验证。adapter 探测结果缓存，避免每次压缩都查注册表/文件系统。

## WPF State

SearchViewModel/独立 flow state 表达 ActionList、DestinationPicker、RenameEditor、Running、WaitingGeneration 和 Error，不用叠加布尔值。mutation 成功后等待比执行前更大的 generation，超时仍保留成功 outcome。

## Rollback

每个动作 family 可通过 allowlist 独立关闭。回滚 WPF 子流程或 adapter 不改变 typed 协议 reader。文件系统副作用不自动逆向回滚；只回滚代码/功能开关并明确用户已完成的操作。
