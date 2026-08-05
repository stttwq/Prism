# Design

## Task Tree Architecture

父任务维护单一需求入口和集成门禁；G0-G9 子任务分别拥有代码、测试、机器验收和规格更新。父任务不运行 `task.py start`，除非未来出现只属于集成层的直接工作。

```text
G0 -> G1 -> G9 -> G3 -> G2 -> G4 -> G5 -> G6
             \-> G7
             \-> G8
```

上图表示默认交付顺序，不取代真实强依赖：G3 依赖 G1；**G9 依赖 G0 与 G1**（G0 提供首建时间基线，G1 提供 `is_truncated` 语义与 C# 测试地基）；**G2 依赖 G1、G3 与 G9**（G3 定型 schema 版本约定，G2 的历史文件是第一个使用者；G9 的逐卷发布框架供 G2 的 sidecar 首建复用）；G5 只强依赖 G2；G7/G8 只强依赖 G1。满足强依赖后可以调整 G5/G7/G8 的先后，但每次只启动一个子任务，避免同一文件和协议并行漂移。

## Stable Boundaries

- WPF：UI、前台宿主上下文、图标、在线联想和状态机；
- broker：用户历史、应用/网页/窗口结果、execute/reveal/actions、用户设置；
- indexer：MFT/USN、缓存、generation、文件候选和受限只读搜索；
- broker IPC 与 indexer IPC 分别版本化，新增字段一律可选、reader 安全忽略未知值；
- 本地数据按所有者分离：机器索引/拼音 sidecar（`%ProgramData%\Prism\`）、用户 history/settings/favicon cache。

## Compatibility Strategy

- 协议：字段先可选、reader 安全忽略未知值、不兼容 protocol 返回明确错误。**不需要 reader 版本先于 writer 版本发布**：三个二进制由同一 Inno 安装包一次替换（`dist/prism.iss` 的 `PrepareToInstall` 先 `sc stop PrismIndexer`，`CloseApplications=force` 处理 WPF 与 broker），不存在新旧混版同时运行的窗口。真正的兼容边界在磁盘持久数据，不在线上协议灰度；
- 缓存：只有持久结构变化才升级版本，旧缓存触发可解释重建；
- 持久文件：任何阶段新增机器级或用户级文件，必须在同阶段更新 `dist/prism.iss` 的安装与卸载清单；
- 功能：history、pinyin、宿主联动、在线联想独立开关；
- 降级：拼音失败 -> 字面搜索；宿主识别失败 -> 全局搜索；联想失败 -> 直接网页搜索；
- 回滚：每个子任务保留自己的回滚点，不跨阶段同时迁移多份用户数据。

## Integration Gates

1. G0 给出可复跑基线和环境元数据，并把 spec 的内存门槛口径改写为三进程。
2. G1 固定搜索和协议语义（含 `filters` 字段预留、`is_truncated` 只表示 `max` 截断），后续功能只能扩展该契约。
3. G9 固定首建可用性契约：逐卷发布、`ready && building` 组合语义、进度字段、落盘门。
4. G3 固定 Shell/权限地基与 schema 版本约定，G2 在其之上固定个性化能力。
5. G4-G8 分别在已稳定地基上增加用户功能。
6. 最终审查只在全部已批准子任务归档后执行；未批准子任务保持 planning，不计为缺陷。

## Rollback

- 子任务失败只回滚该子任务引入的协议字段、数据版本或功能开关；
- 已发布 reader 对未知可选字段保持兼容；
- 数据迁移必须保留旧文件或可安全回到空状态；
- 不用破坏性 Git 操作回滚用户已有修改。

