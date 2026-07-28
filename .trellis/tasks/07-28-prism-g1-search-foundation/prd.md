# G1 搜索正确性、协议与前端测试

## Goal

以 G0 基线为对照，修复文件搜索按扫描顺序提前终止造成的漏排，建立跨卷全局可解释 Top-K、延迟路径构造、协议截断/generation 语义和 WPF 自动化测试地基，同时保持首屏 8 条与展开 1000 条兼容。

## Requirements

- 强依赖 G0 已归档且基线可复跑。
- 文件候选先只保留卷、record、名称引用、flags、匹配等级、score 和稳定 tie-break 信息；只有最终 Top-K 才构造完整路径。
- 按请求 `max` 维护跨卷全局 Top-K，不允许命中 `max` 后提前结束扫描、每卷分别截断再拼接或固定 Top-K=200。
- 第一版排名顺序为字面精确、前缀、中间位置，再比较集中定义的 score；文件、文件夹、应用统一竞争，类型只作最终 tie-break。相同索引与请求必须输出稳定顺序。
- broker handshake 增加显式协议版本；结果增加 `is_truncated`、稳定的 `index_generation`、稳定字符串 `kind` 和可选匹配元数据。未知 kind 在 C# 映射为 `Unknown`。
- 新旧 reader/writer 采用可选字段优先策略；明确不兼容返回协议错误，不能静默返回不完整结果。
- 前端增量缓存仅在查询前缀增长、旧结果未截断、generation/模式/root/filters/排序配置不变时可本地过滤；其余情况重新请求。
- 管道客户端取消旧业务请求后仍保持请求/响应配对，迟到响应不得覆盖新查询。
- 新建 C# 单元测试项目，把搜索客户端、generation 客户端、调度器/计时器抽为可替换依赖；timer 事件只绑定一次。
- WPF 状态测试覆盖 Idle、Results、Actions、More、Pin、失焦、generation、迟到响应、Unknown kind 和截断缓存。

## Acceptance Criteria

- [ ] 构造的跨卷语料证明较晚扫描到的高分项可以进入 `max=8`，输出不依赖 MFT record 顺序。
- [ ] `max=8` 与 `max=1000` 都按同一全局排序契约返回，结果数、`is_truncated` 和稳定 tie-break 有测试。
- [ ] 路径构造计数证明非最终候选不会批量构造完整路径。
- [ ] 旧/新 reader-writer 兼容矩阵、未知 kind、缺失可选字段、超限请求和协议不兼容均有测试。
- [ ] 截断首屏不能作为完整缓存；删除字符、generation/root/mode/filter 变化会重查后端。
- [ ] 迟到响应、取消和管道重连不会造成响应错位或旧结果覆盖新结果。
- [ ] C# 测试项目可在命令行独立运行，覆盖 SearchViewModel 关键状态转换。
- [ ] G0 同口径复测满足三进程总内存 ≤100MB、暖 `max=8` P95 ≤100ms、`max=1000` P95 ≤300ms；超门槛则不得启动 G2-G8。
- [ ] Rust tests、Clippy `-D warnings`、C# tests 与 WPF Release build 全部通过。

## Out Of Scope

- 历史、拼音、root、`ext:`/`path:`、窗口和网页功能；
- 更改 `NodeSlot` 12B 布局；
- 用固定候选上限换取表面延迟；
- 流式结果作为用户可见功能。

## Dependencies

强依赖 G0。G2、G3、G7、G8 均依赖本任务归档后的稳定契约。
