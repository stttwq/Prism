# Prism 综合优化与功能演进

## Goal

在不突破三进程权限边界、100MB 总内存硬门槛和现有 8/1000 结果行为的前提下，按可独立验收的阶段提升 Prism 搜索正确性、可测试性和中文体验，并增加已确认的目录联动、窗口切换、内置动作、有限过滤及网页增强。

本任务是父任务，只管理需求来源、阶段依赖、跨阶段门禁和最终集成验收，不直接承载产品代码实施。

## Source Of Truth

- `docs/PRISM-COMPREHENSIVE-PLAN.md`：阶段、接口、门禁、回滚和风险；
- `docs/POST-ROADMAP-REVISED.md`：已确认的产品行为与明确排除项；
- `docs/PRISM-OPTIMIZATION-REPORT.md`：当前仓库事实、问题审计和未经验证数字；
- `.trellis/spec/backend/quality-guidelines.md` 与 `.trellis/spec/frontend/quality-guidelines.md`：现行代码契约，若实施改变契约必须在对应子任务结束前更新。

冲突优先级：当前源码与已提交 Trellis 规格 > 综合计划 > 收敛稿 > 历史原稿。任何新增实测结果高于旧估算。

## Child Task Map

| 顺序 | 子任务 | 交付目标 | 强依赖 |
| --- | --- | --- | --- |
| G0 | `07-28-prism-g0-baseline` | 可复现搜索与三进程内存基线 | 无 |
| G1 | `07-28-prism-g1-search-foundation` | 全局 Top-K、协议截断语义、正确缓存、C# 测试地基 | G0 |
| G2 | `07-28-prism-g2-history-pinyin` | 版本化使用历史与可卸载拼音 sidecar | G1 |
| G3 | `07-28-prism-g3-engineering-foundation` | Shell/COM、协议类型、日志、排除规则与旧链清理 | G1 |
| G4 | `07-28-prism-g4-host-integration` | root 搜索与 Explorer/系统对话框/Opus 联动 | G1、G2、G3 |
| G5 | `07-28-prism-g5-window-switcher` | `>` 窗口搜索、最近窗口与可靠切换 | G2 |
| G6 | `07-28-prism-g6-built-in-actions` | 完整、安全的单项内置动作 | G2、G3 |
| G7 | `07-28-prism-g7-query-filters` | Top-K 前的 `ext:` / `path:` 过滤 | G1 |
| G8 | `07-28-prism-g8-web-enhancements` | 专用网页模式、受控联想与 favicon | G1 |

父子关系不替代依赖控制。启动任何子任务前必须检查其强依赖已完成并通过门禁。

## Global Requirements

- Windows 11 x64，个人使用；
- WPF、普通权限 broker、LocalSystem indexer 三进程和两条命名管道保持职责分离；
- LocalSystem indexer 不执行 Shell、剪贴板、用户命令或联网请求；
- 不使用 DLL 注入，不提供 JSON/DLL 动作扩展；
- 文件、文件夹、应用和窗口使用可解释匹配层级，类型只作最终稳定 tie-break；
- 首屏 8 条、展开 1000 条行为保持兼容；
- 三进程 Release 总内存 ≤100MB；
- 暖查询 `max=8` P95 ≤100ms，`max=1000` P95 ≤300ms；
- 未实测数字必须标记为目标或估算，旧 38MB/43-45MB 不得当作当前事实；
- 每个阶段独立提交、验收、更新规格和回滚，不把 G0-G8 合并为一次实现。

## Out Of Scope

- 剪贴板历史、文件预览、收藏、内容索引或临时全文扫描；
- HTTP API、跨设备、文本片段与模板；
- 贴靠/内嵌第三方 UI、DLL 注入；
- JSON 自定义动作、DLL 插件、任意 Shell verb；
- 窗口关闭、最小化、置顶、分屏、跨屏和进程管理；
- Windows 10、ARM64、x86、浏览器/Office 对话框和其他第三方文件管理器兼容承诺；
- `size:`、`datemodified:`、自然语言日期和负向过滤。

## Cross-Child Acceptance Criteria

- [ ] G0-G8 每个子任务都有收敛后的 `prd.md`、`design.md`、`implement.md`，不存在 TBD 或阻塞性开放问题。
- [ ] 每个子任务启动前，其强依赖已归档完成，并引用依赖阶段的实测或稳定接口。
- [ ] 每个子任务完成时 Rust tests、Clippy、C# tests、WPF Release build 按适用范围通过。
- [ ] 涉及索引、范围、拼音或动作的任务完成 Windows 11 x64 机器验收。
- [ ] 三进程总内存始终 ≤100MB；任何超限阶段不得合并或继续下游任务。
- [ ] 8/1000 结果、管道请求/响应配对、generation 刷新和字面搜索降级无回归。
- [ ] 最终集成审查确认明确排除项未被重新引入，所有文档数字区分实测与估算。

## Deferred Decisions

没有阻塞任务树建立的产品决策。各子任务中允许存在“先原型后选择”的技术门，但必须给出候选范围、选择指标和失败降级，且不得改变已确认的用户行为。

