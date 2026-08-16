# Prism 审计修复实施计划

- **日期**:2026-08-16
- **依据**:`docs/PRISM-FULL-AUDIT-2026-08-16.md`(52 项发现)+ 同日全量核对结论(逐项对照 HEAD `40a720e` 源码核实)
- **核对结论摘要**:发现层 50/52 属实、行号基本精确;**修复处方层 8 处需修正后才可执行**(S1 假恢复、M1 EOF 误判、H1 select! 无效、L2 丢崩溃日志、L1 一刀切超时、P4 环置换、P12 上限误杀、M5 轮询不可靠),2 处误报(H3 附带 reject_remote_clients、L3"回退 rename"机制),难度重估 5 项低估 + 2 项高估。本计划已把全部修正吸收进各条目做法。

## 全局执行规则

1. **测试**:每项改动附带或更新单测;凡触发 Win32 对话框/实机窗口的测试一律 `#[ignore]`(C# 侧 Live 测试同理,参照 `WindowActivatorLiveTests`)。
2. **质量门**(每批次):`cargo fmt && cargo clippy && cargo test`(src/prism-core)+ `dotnet build` / `dotnet test`(src/Prism、src/Prism.Tests)。
3. **提交**:一个编号一个 commit(同源编号可合并),沿用 `fix:/feat:/chore:` 中文说明;工作在 `feature` 分支,逐 commit 可单独 revert。
4. **安装包**:里程碑批次(1/2/3/6/8)结束用 ISCC(`D:\LS\Setup 7`)重建并实机冒烟;其余批次可选。
5. **协议变更**集中在本计划批次 5(`INDEXER_PROTOCOL` 1→2),其余批次不动协议;同仓同版发布,新旧混跑由 hello 校验拒连(预期行为)。
6. **大改先行试点**:A1③、S2b、I1 均先开关化/真机验证再全量。
7. 会话收尾走 `/trellis:finish-work` 记 journal;建议批次 1-3 各建 `.trellis/tasks` 条目归档。

## 批次总览

| 批次 | 主题 | 内容 | 工时 | 风险 |
|---|---|---|---|---|
| 1 | 稳定性止血 | S1(修正版)、S3、S2a、L4 | 1-1.5 天 | 中 |
| 2 | 管道与连接健壮性 | H3、C2、L1、M2 保守版、L3 修正版 | 1 天 | 中低 |
| 3 | 前端交互冻结消除 | C1、A2、U3 | 1 天 | 中 |
| 4 | broker 每击键分配快赢 | P2/P8/P6/P9/P10/P13/P15/P12/P5-app | 1-1.5 天 | 低 |
| 5 | indexer 快赢 + 拼音开关治理 | I2/I4/I5、M1(EOF 特判)、M4+M6 | 1-1.5 天 | 中(协议) |
| 6 | 内存主项 | P1+P3+M3、P4(take 法)、P7 | 2 天 | 中高 |
| 7 | 服务长任务与锁治理 | H1、H2 | 1.5-2 天 | 中高 |
| 8 | UI 美观与动画 | U1-U7、A4、A5、A3、A1②+③试点 | 2 天 | 低中 |
| 9 | 专项大项(按需) | M5、S2b、I1、I3、L2、P11、M2 池版、A1①+U8 | 按需 | 高 |

批次 1-8 合计约 10-12 个工作日。若实际使用中已观察到 8s 搜索超时/重连风暴,批次 7 提前到批次 4 之前。

---

## 批次 1:稳定性止血(Rust)

**S1 — 首建落盘失败降级(修正版,不是一行)**
- `indexer_runtime.rs:939-957` `persist_first_build_with`:save 失败时**同时**调 `finish_first_build()`(打开 checkpoint 的 R2 门,`:1249`)与 `set_error`,与 rebuild 分支(`:726-734`)对齐。
- 必须改写两个编码了相反语义的既有测试:`:1673`、`:1692` — 新语义为"完成但持久化待重试,6h checkpoint 会真实落盘"。
- 新增测试:save 失败 → `checkpoint()` 不再被 R2 门静默跳过;成功后 `clear_error()`。
- 注意:`building=false` 对外可见(前端不再显示"构建中",改显示降级错误),确认前端提示链路。

**S3 — resolve_window / record_window_switch 包 spawn_blocking**
- `ipc.rs:609-629`;`WindowProbe` trait 加 `Sync` supertrait(`&dyn WindowProbe` 非 Sync 正是当年拆两步的原因,`ipc.rs:675` 注释),闭包克隆 `Arc<WindowSnapshotStore>`,与 `:1099` 既有模式同构;保持"先 probe 后 `write_window_history` await"顺序。
- 严重度按核对结论降一档(GetWindowTextW 跨进程走缓存),属防御性统一。

**S2a — zip 外部进程挪出 STA**
- `shell.rs` 分流:`ActionId::Zip` 且 `detect_zip_program` 为 External 时走独立 `spawn_blocking`(纯 `std::process`,零 COM);`zip_with_shell_com` 留 STA。
- 不动超时与 worker 重建(归批次 9 的 S2b)。

**L4 — 缓存命中路径检查 stop**
- `indexer_runtime.rs:820-834` 前加 `stop.is_requested()` 分支,回填 `descriptors` + `interrupted: true`(`watchers_started=false` 组合已被 run() 正确处理)。

验证:cargo test + 新增测试;手动冒烟(搜索/动作/服务启停);重建安装包。

## 批次 2:管道与连接健壮性

**H3 — broker 管道实例失败重试**
- `ipc.rs:336-339` / `main.rs:47-51`:仅**首个**实例 create 失败(真双实例)才退出;循环内失败退避重试。
- 重试前以 `first_pipe_instance(true)` 探测,区分"自己占着"与"被第三方抢注"(抢注才退出)。
- 可选顺手:搬 indexer 的 `PIPE_LISTENERS=4` 模式消 connect/create 间隙的 BUSY。
- 误报澄清:tokio 1.53.1 `ServerOptions` 默认已 `reject_remote_clients=true`,无需补。

**C2 — 握手读超时**
- `PipeClient.cs:175` hello 读加 3s 超时(broker Hello 同步内联生成,无合法慢握手);超时触发补 `DisposeStreamOnly` 清理(现在只有协议不匹配分支清理)。
- `SendAsync` 快速重连(`:497` `TryConnectPipeOnlyAsync`)同样加。
- watchdog 判活从纯 `IsConnected` 升级为"连通 + 最近请求有响应"。

**L1 — 分阶段空闲超时(不能一刀切 60s)**
- hello 阶段 10s;已握手连接 10min 级或不掐。历史教训:每请求新建连接曾导致 ERROR_PIPE_BUSY / 517/800 失败(`indexer_client.rs:3-14` 注释)。

**M2 保守版 — 单连接锁快速降级**
- `indexer_client.rs:128-143`:`try_lock`/短超时拿不到锁 → 开一次性短连接发本次请求(零协议改动)。注意 tokio Mutex 不暴露等待者计数,"已有等待者降级"需自建 AtomicUsize;连接池完整版归批次 9。

**L3 修正版 — 原子替换次序**
- `fs_util.rs`:`ReplaceFileW` 优先,仅 `ERROR_FILE_NOT_FOUND` 回退 rename,消掉 `exists()` 预判;对 hidden/system 属性导致的 ACCESS_DENIED 记日志(真实环境更可能的失败源)。

验证:cargo test + dotnet test;手动:broker 半死(连上不握手)场景、杀进程重连。重建安装包。

## 批次 3:前端交互冻结消除

**C1 — 动作走独立管道连接(broker 端零改动)**
- `ipc.rs::serve()` 已是每连接独立 task 的多客户端架构,直接开第二条连接即可。
- `PipeClient.cs`:第二套 stream/reader/writer + 独立锁 + 握手 + watchdog/Dispose 双流管理(约 200 行);`ExecuteAsync/RevealAsync/RunActionAsync` 走连接 2。
- 语义边界写进注释:独立连接解"动作阻塞搜索";"动作阻塞动作"(单 STA worker)仍在,归 S2b。
- 注意协议无请求 id、按行严格配对 — 不要采用报告原文的"同连接 ack+通知"替代方案。

**A2 — ActionPanel 同步化**
- 移植 `ResultList.SynchronizeDisplayItems`(`ResultList.xaml.cs:104-121`);RowKey 直接用 `ActionItem.Id`(已核实稳定唯一,broker 侧 ActionId 不重复且无节标题)。
- 保留绑定式行模板(容器复用后绑定内容天然正确,不搬 DecorateVisibleItems 脏检查)。
- 对齐 `SelectedIndex/ClampToSelectable/MoveSelection/UpdateHeight` 改用 `_displayItems` 语义。

**U3 — 占位文案**:`SearchHeader.xaml.cs:102` 一行:`actions ? "输入以筛选动作" : "搜索应用和文件"`。

验证:dotnet build/test;手动:属性页开着继续打字搜索、Actions 模式过滤不闪、右键动作。重建安装包。

## 批次 4:broker 每击键分配快赢(全部低风险,可独立 commit)

| 项 | 做法 | 关键契约 |
|---|---|---|
| P2 | history index 改 interned 单串 key(`kind+'\0'+value` + `Borrow<str>`)或排序 Vec 二分 | `is_recordable` 已拒 NUL,分隔符安全 |
| P8 | 加 `query_pick_by_key(target, &str)` 直传已归一化 key | 归一化幂等,行为不变 |
| P6 | dedup 集合单串 intern;连同 `injected_history_targets` 构造处(`ipc.rs:1150-1158`) | — |
| P9 | 合并 rank_title/match_spans 的双 `to_lowercase` 为一次降幂 | UTF-16 偏移必须在**小写串**上算(`ipc.rs:1899` 注释) |
| P10 | `apps::search` 返回 (top-N, total_count),消除 `usize::MAX` 物化 | 保 `matched_count` 精确计数语义 |
| P13 | `path_is_under_root` 无分配版本;排除表预归一化一次 | 消掉 `:1416` 预归一化被重复做的浪费 |
| P15 | 连接循环外复用 Vec,`serde_json::to_writer` | — |
| P12 | `indexer_client.rs` 接入 `BoundedLineReader` | indexer 方向单独 **4-8MB** 上限 + 测试(1000 条×长路径合法超 1MB,勿复用 1MB) |
| P5-app | `AppEntry` 加 `pinyin: Option<Vec<u8>>`,`encode_compact` 扫描时预编码(仓库已有 indexer 侧范式) | 缓存随 `PINYIN_DICTIONARY_VERSION` 失效;不动 history schema、窗口编码归批次 9 |

验证:cargo test 每项独立回归 + 排序等价性测试(P9/P10 影响排序输入)。

## 批次 5:indexer 快赢 + 拼音开关治理

**快赢**:I2(max 改迭代器聚合)、I4(sidecar save 抄 `index_cache.rs:56-67` 的 `BufWriter+to_io` 模板)、I5(load 改 `from_io`/mmap)。

**M1 — MFT 枚举错误不再静默(必须特判)**
- `ntfs.rs:555`:`ERROR_HANDLE_EOF` 是 `FSCTL_ENUM_USN_DATA` 的**正常结束**返回,视为成功结束;其余错误记日志 + `Err` 上抛,接入既有 `failed_volumes`/degraded 链路(`indexer_runtime.rs:869-886`)。

**M4+M6 联动 — 拼音开关改管理命令(协议演进)**
- `IndexerRequest` 新增 `SetPinyinEnabled`;`INDEXER_PROTOCOL` 1→2(`lib.rs:42`)。
- `indexer_runtime.rs:505` `search()` 删 `store` 只读;`handle_connection` 增臂;maintenance/checkpoint 的 `Disabled` 语义保持("用户显式关闭后不重建")。
- broker `UpdatePreferences`(`ipc.rs:571-586`)改调新命令,**删除 `:581` 空查询 side-channel**;失败记日志(M4 的吞错误),可后台 spawn 但必须保住"disable 释放 / enable 加载"语义。
- 顺带修 `indexer_runtime.rs:1447` `unwrap_or(false)` 缺省翻转。
- 验证:两端同装全流程(开关→搜索→重启服务→sidecar 状态);老协议混跑被拒测试。

## 批次 6:内存主项(history/排序,独立 PR + 专项测试)

**P3+M3 — 记录路径出锁 + 增量维护**
- `record_at`:锁内只做内存更新 + 一次克隆,JSON 编码 + fsync 出锁。
- prune/index 增量化,必须复刻不变量:**entries 按 last_used 降序 + (kind,target) tie-break**(`history.rs:455-456`,空查询 MRU 依赖)。
- 防抖落盘是语义变化(崩溃丢最近计数),本期不做。

**P1 — weights() 命中才拷贝**
- 首选 `Arc<Vec<HistoryWeight>>` 快照发布(成本从每击键移到每次动作);或过滤回调进读锁只 clone 命中条目 — `Path::exists()` 磁盘 stat **必须留在锁外**。

**P4 — 最终排序去 clone(勿写环跟随置换)**
- `Vec<Option<SearchResult>>` + `Option::take()` 按序重建 — 零 String clone、无环特例(`aabb5fe` 刚在此翻过 2/3 元环 bug,当前全量 clone 是那次修复的产物)。
- 保 items/picks/indices 三者下标对齐(`:1744-1747` 契约)与 kind→picked→metadata 层级;`rank_window_list` 入口同步。

**P7 — SearchResult 路径字段 `Arc<str>` 化**
- 7 处构造点(ipc.rs:920/944/1015/1478/1606、websearch.rs:75);**execute_id 必须继续序列化**(wire 契约);三处所有权来源不同,统一 Arc;测试内路径断言跟随调整。

## 批次 7:服务长任务与锁治理(H1/H2)

**H1**(报告原方案 select! 无效 — 只能放弃等待,恰丢关停落盘):
- 新增服务层↔runtime 状态通道(现仅 Shutdown 一个通道),StopPending 期间周期向 SCM 递增 checkpoint 上报。
- rebuild 的 `build_all` 传 `should_cancel` 回调(对齐首建 `build_volume_reporting_with` 模式);checkpoint 的 validate/serialize 分块 + 阶段检查 stop。
- 澄清:强杀截不坏 `index-v5.bin`(tmp+ReplaceFileW 原子),实际后果是缓存偏旧/USN 回滚。
- 验证:`sc stop` 在 rebuild 进行中 → 优雅退出且关停 checkpoint 保留;实机测试 `#[ignore]`。

**H2**:
- 拼音加载移出读锁:锁外 mmap+解码 → 重取读锁 → **世代比对**(generation 每批 +1,比全量 names 哈希廉价),不一致丢弃重试;同类入口 `checkpoint():1271 rebuild_pinyin_from_live` 一并处理。
- compaction 改"读锁克隆 → 锁外压缩 → 写锁 `mem::swap`"(仓库已有 checkpoint 克隆范式)或死字节计数器消预扫;注意触发点对所有卷循环(`:1226-1231`)。真·分段(改扁平 u32 偏移池 schema)归批次 9 的 I3,不做。
- 验证:多客户端并发搜索 + 大 USN 批次压测,读锁等待 < REQUEST_BUDGET。

## 批次 8:UI 美观与动画

**快赢(≤1 小时级)**:U1(两 Token 各一行,`WebIconProvider.cs:138` 顺手;Win10 回退与现状等价)、U2(新增 `IconFontFamily` 令牌 + 4 处替换;Fluent 是 MDL2 超集,现用码位全兼容)、U6(thumb hover/dragging 触发器)。
**小项**:U4(hover **两处模板同改** `Styles.xaml:155-174` + `ActionPanel.xaml:163-182`,双主题验证)、U5(1px Divider 边框)、U7(MenuItem 模板扩图标列,IconGlyph 空时收起)。
**A4**:IconCache.Clear 保留 `ext:`/`dir:` 键只清路径键;TrimWorkingSet + GC.Collect 延迟数分钟(隐藏起 DispatcherTimer,再呼出则取消);统一 5 个 Trim 调用点策略(`App.xaml.cs:141/200/340`、`SearchWindow.xaml.cs:164/321`)。
**A5**:StatusMessage/ActionStatus 仅"出现"淡入 100ms、"消失"瞬时(防按键周期新闪烁);StatusText 高度参与 `UpdateListHeight`,淡入期间不得提前 Collapsed。
**A3**:chip 列宽 120ms 补间(需自写 GridLengthAnimation 或 attached property 中转);与窗口淡入叠加观感先真机验证。
**A1②**:九宫格预渲染阴影按高度档缓存(档位按行高 62/44px 取整、DPI 贴齐),保留 `DropShadowEffect` 回退开关。
**A1③(试点,开关化)**:动画期 SizeToContent 置 Manual;处理回切 1-2px 跳变、动画中目标突变、隐藏/首显/ScopeNotice 旁路(漏一处永久卡高度);真机帧率对比后定去留。
**A1①+U8**:仅当决定放弃 Win10 或做双平台双路径时启动(manifest 声明支持 Win10;DWM 圆角仅 Win11、Win10 无 WindowStyle=None 阴影、20px 边距点击穿透会变)。

验证:浅/深双主题 + 125%/150% DPI 目视回归;dotnet test;重建安装包。

## 批次 9:专项大项(按需单独决策)

| 项 | 推荐方向 | 关键风险 |
|---|---|---|
| M5 zip 假成功 | 返回"后台进行中"语义 + 失败/超时删残留;评估 PowerShell `Compress-Archive`/强制 7-Zip | 轮询 `Folder.Items()` 不可靠(无完成通知、计数滞后、失败不回滚、STA 无消息泵、占死唯一 worker) |
| S2b STA 治理 | 超时仅限无 UI 操作(open/reveal);worker 报废重建(Drop 改可放弃) | properties/UAC/IFileOperation 确认是刻意产品行为;孤儿对话框;被放弃操作"稍后成功→重试→重复执行"竞态 |
| I1 MFT 分批消化 | 架构级重构(枚举形态、建卷时机、64 轮 defer、容量护栏联动),1-2 天 + 真机大盘验证 | MFT 槽复用卷 deferred 集退化;须保留多轮 pass;M1 的 EOF 特判联动 |
| I3 名字池分段 | 仅在批次 7 锁外压缩后实测内存仍不达标时考虑 | 强制 CACHE_VERSION 6 → 全量重建;降阈值会加剧写锁停顿 |
| L2 日志异步化 | channel + 专职写线程;**panic hook 保留同步直写兜底** + 退出 drain | panic=abort 下 channel 缓冲必丢崩溃日志(hook 就是为 08-07 事故存在的) |
| P11 空闲 trim | 空闲数分钟 `EmptyWorkingSet`,严格 idle 门控 | 收益偏观感;软缺页反噬 |
| M2 连接池 | 2-4 条小池 round-robin(服务端 4 listener 天然支持) | 重连风暴时多连接同时握手放大 indexer 负担 |
| A1①+U8 | 见批次 8 | 见批次 8 |

## 依赖与顺序约束

- M4 ↔ M6 必须同批(同一条开关应用机制);协议变更只在批次 5。
- P1/P3/M3 同源同批;P4、P7 独立但都在批次 6(共享 SearchResult 改动语境)。
- H2 的锁外压缩与 I3 schema 重设计互斥 — 先前者,后者凭实测。
- C1 与 S2b 正交:前者解"动作阻塞搜索",后者解"动作阻塞动作"。
- A1① 依赖"放弃 Win10"决策;U8 随 A1①。
- 批次 4/5 与批次 1-3 无依赖,可穿插;批次 6 依赖批次 4 的 P2(index 结构改动叠加)。
