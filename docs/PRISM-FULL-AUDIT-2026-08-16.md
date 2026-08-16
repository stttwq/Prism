# Prism 全仓库审计报告与修改建议

- **日期**:2026-08-16
- **范围**:摒弃既有文档结论,从源码从头通读——WPF UI(`src/Prism`,全部 XAML/code-behind/服务/ViewModel)+ Rust broker 与索引服务(`src/prism-core`,约 1.5 万行,含内存专项与稳定性专项两轮完整阅读)+ 构建配置
- **审计维度**:动画连贯性 / 界面美观 / 内存占用 / 软件稳定性
- **编号约定**:A=动画,U=美观,P=broker 内存,I=indexer 内存,S/H/M/L=稳定性严重度,C=C# 侧

## 总体结论

代码质量整体较高:防图标闪烁(行身份键 + 原地重绘)、防管道孤儿响应(配对读 + 超时销毁流)、原子落盘(temp + fsync + ReplaceFileW)、句柄/钩子清理、单实例与 JobObject 守护等细节都做得很扎实。索引内存布局(不存全路径、12B NodeSlot、槽表上限护栏)设计良好,无泄漏级问题。

主要短板集中在四处:

1. **动画渲染路径**:分层窗口 + 逐帧布局动画 + 阴影 Effect,三者叠加是掉帧主因;
2. **broker 每击键分配**:历史库全量 clone、逐字拼音编码等,高频打字下每键约 0.7-1.2MB 瞬时分配;
3. **三处严重稳定性缺陷**:索引服务落盘失败重启循环、单 STA Shell worker 无超时挂死、resolve_window 阻塞仅有的 2 个 tokio worker;
4. **界面细节**:字体/图标字体未跟上 Win11、Actions 占位文案错误、浅色 hover 不可见等。

---

## 一、动画连贯性

### A1【高】分层窗口 + 每帧高度动画是掉帧主因

**位置**:`src/Prism/Windows/SearchWindow.xaml:5-11`、`SearchWindow.xaml.cs:839-875`

窗口使用 `AllowsTransparency="True"`(强制分层窗口)+ `SizeToContent="Height"`,而 `AnimatePanelHeight` 对 `PanelHost.Height` 做逐帧**布局属性**动画。每一帧触发:整棵 StackPanel 重排 → 窗口因 SizeToContent 重测 → 分层窗口表面整面重上传 → 卡片 `DropShadowEffect`(`RenderingBias=Performance` 仍是逐帧栅格化)。在 125%/150% DPI 或核显机器上,这是逐键搜索时结果区展开/收起不跟手的根源。

**建议**:
- Windows 11 改用 DWM 原生方案:`WindowStyle=None + WindowChrome`(去 AllowsTransparency),圆角用 `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND)`,阴影由 DWM 边框帧提供——整窗硬件合成,高度动画不再走分层上传;
- `DropShadowEffect` 换九宫格预渲染阴影(行数离散变化,可按高度档位缓存),或交给 DWM;
- 若暂不动窗口框架,动画期间把 `SizeToContent` 置 Manual,由动画直接驱动窗口高度,避免每帧两次布局。

### A2【中】ActionPanel 输入过滤时整表重建闪烁

**位置**:`src/Prism/Controls/ActionPanel.xaml.cs:32`(`List.ItemsSource = _items` 整体替换)

ResultList 为防闪烁专门实现了 `SynchronizeDisplayItems`(只增删移、按 RowKey 原地重绘,`ResultList.xaml.cs:104-121`),ActionPanel 没有同等待遇:动作面板打字过滤时行容器全部重建,行闪烁且滚动位置丢失。

**建议**:移植 ResultList 的同步逻辑(ActionItem 需补 RowKey 概念,可按 Id+Label 构造稳定键)。

### A3【中】ScopeChip 入场把输入框横向挤开(瞬移)

**位置**:`src/Prism/Controls/SearchHeader.xaml.cs:58-73`(注释已承认"输入框的横向占位仍会瞬移——不做")

呼出后约 300ms 宿主识别完成,chip 滑入,已输入文字瞬间右移几十像素——每次呼出都可见的连贯性破绽。

**建议**:chip 列保留固定宽度(内容淡入、宽度不变),或对列宽做 120ms 补间。

### A4【中】每次隐藏清空图标缓存,重开首帧图标空白

**位置**:`SearchWindow.xaml.cs:309-328`(`ReleaseIdleMemory` 中 `_icons.Clear()` + `GC.Collect` + `EmptyWorkingSet`)

下次呼出所有行图标需重新走 SHGetFileInfo/系统图像列表异步加载,图标空白一帧起步;`EmptyWorkingSet` 造成的软缺页同样拖慢下一次呼出。128 条 × 48px 位图上限约 1MB,为省这点内存牺牲首绘流畅不划算。

**建议**:扩展名图标(`ext:` 键)不清,只清路径类条目,或不清;`EmptyWorkingSet` 改为空闲数分钟后才执行,而非每次隐藏。

### A5【低】小项

- `ResultList.StatusMessage` 与底部 `ActionStatus` 显隐瞬时,与 Divider 的 100ms 淡入不一致,可统一加淡入;
- `ReleaseDeactivateGuardAfterDelay`(`SearchWindow.xaml.cs:498`)每次新建不追踪的 DispatcherTimer,轻微 GC 噪音,可复用单实例。

---

## 二、界面美观

| 编号 | 问题 | 位置 | 建议 |
|---|---|---|---|
| U1 | 字体未换代:微软雅黑优先,Win11 上拉丁字符偏粗偏旧 | `Tokens.*.xaml:39` | `Segoe UI Variable Text, 微软雅黑, Segoe UI` |
| U2 | 图标字体全用 Segoe MDL2 Assets,Win11 下应为 Fluent 线稿 | 全局(`SearchHeader.xaml:62` 等) | `Segoe Fluent Icons, Segoe MDL2 Assets` 回退栈 |
| U3 | Actions 模式占位文案错误:空过滤仍显示"搜索应用和文件" | `SearchHeader.xaml.cs:102` | Actions 态显示"输入以筛选动作" |
| U4 | 浅色 hover 几乎不可见:0.55×#ECEEF0 叠白底 ≈ #F5F6F7 | `Styles.xaml:155-174` | hover 用独立令牌色、全不透明 |
| U5 | 卡片无边框:浅色桌面下白卡与背景融合,仅阴影兜底 | `SearchWindow.xaml:15` | 加 1px `Divider` 色边框 |
| U6 | 滚动条 4px thumb 无 hover 反馈,细且难抓 | `Styles.xaml:28-59` | hover 时 thumb 变宽变深 |
| U7 | 右键菜单无图标,与动作面板视觉不统一 | `SearchWindow.xaml.cs:439` | 复用 `ActionItem.IconGlyph` |
| U8 | 进阶:去 AllowsTransparency 后可上 Mica/Acrylic 材质,观感对齐 PowerToys Run | — | 随 A1 改造一并评估 |

---

## 三、内存占用

### 3.1 C# 侧(UI 进程)

- `IconCache`(128 条上限,扩展名合并键)、`FaviconCache`(磁盘 64 条 LRU + 256KB/128px 上限)、`WebIconProvider._resolved`(origin 数量级)均有界,**无问题**;
- 唯一建议即 A4:不要每次隐藏清空 IconCache;`GC.Collect` + `EmptyWorkingSet` 每次隐藏 + 清空查询时都执行(`SearchViewModel.cs:309` 触发 `IdleMemoryReleaseRequested`)过于激进,建议空闲数分钟一次性执行;
- `Prism.csproj` 已显式关闭 ServerGC、启用并发 GC,配置正确。

### 3.2 Rust broker(每击键热路径为主)

架构前提:broker 不驻留索引(IndexState 在 indexer 服务进程),常驻内存 = 历史库 + 应用清单 + 窗口快照 + tokio runtime。

| 编号 | 级别 | 问题 | 位置 |
|---|---|---|---|
| P1 | 高 | 每次搜索 `weights()` 将全部历史(上限 5000 条)逐条 clone(每条 2 个 String),候选筛选只消费前几十条;满历史时每击键 ~0.3-0.5MB 瞬时分配 | `history.rs:286-308`、`ipc.rs:1132` |
| P2 | 高 | 历史 `lookup()` 每次为 HashMap key clone 两个 String;调用密度 = 每击键 600-1000 次 | `history.rs:93-96` |
| P3 | 高 | 每次动作记录:全量排序(O(n log n))→ 重建整个索引 → `entries.to_vec()` 整表 clone → JSON 落盘,**全程持写锁并 fsync**,期间搜索读锁被阻塞 | `history.rs:245-247, 450-481` |
| P4 | 中高 | 最终排序对整个结果集完整 clone(每项 6 个 String + spans),300 项 ≈ 90KB/击键,"more"模式 ≈ 300KB | `ipc.rs:1748-1772` |
| P5 | 中高 | 拼音即时编码每个汉字分配一个 String,app/历史/窗口名每次查询重复编码、零缓存 | `pinyin.rs:22-27, 114-172` |
| P6 | 中 | dedup 查找为每条 indexer 结果构造临时 (String, String) 元组 | `ipc.rs:991-993` |
| P7 | 中 | SearchResult 内同一路径存 3 份(subtitle/execute_id/target) | `ipc.rs:1012-1015` 等 |
| P8 | 中 | 查询记忆循环内对同一查询串重复归一化 | `ipc.rs:1227-1233`、`history.rs:268-284` |
| P9 | 中 | 匹配打分对同一 title 重复 `to_lowercase`(rank_title 与 match_spans 各一遍) | `ipc.rs:1710-1727, 1890-1903` |
| P10 | 中 | 应用搜索以 `usize::MAX` 全量物化后再截断 | `ipc.rs:902` |
| P11 | 中 | 无任何空闲内存回收,稳态工作集由峰值请求决定 | 全局 |
| P12 | 低中 | indexer 响应行读取无长度上限(broker 入站侧有 1MB `BoundedLineReader`,防御不一致) | `indexer_client.rs:97-123` |
| P13 | 低 | 路径前缀/排除判断在循环内反复分配 | `ipc.rs:1487-1516` |
| P14 | 低 | AppEntry.target_path 与 launch_path 冗余双份 | `apps.rs:13-23` |
| P15 | 低 | 每响应新建 serde 序列化缓冲 | `ipc.rs:495-498` |

**修复要点**:P1/P3 改增量维护(命中才拷贝、锁内只克隆锁外落盘);P2 改 interned 单串 key(`kind + '\0' + value` 配 `Borrow<str>`)或排序 Vec 二分;P4 实现正确的环跟随原地置换(修掉旧 2/3 元环 bug 的正确写法);P5 编码结果在 app/窗口清单里缓存一次;P11 空闲数分钟调 `EmptyWorkingSet`。

### 3.3 Rust indexer 进程

| 编号 | 级别 | 问题 | 位置 |
|---|---|---|---|
| I1 | 中高 | MFT 枚举整卷记录全量驻留(`records.extend(batch)`)后才逐轮消化,3M 记录卷峰值约 200-400MB——构建期内存峰值主项 | `ntfs.rs:526-560` |
| I2 | 中 | max_record 计算分配全量 Vec(3M×4B=12MB)只为取 max | `ntfs.rs:398-404` |
| I3 | 中 | 名字池 append-only(重命名/删除旧名滞留),压缩阈值 `max(8MB, initial/4)` 偏高,压缩时新旧双驻留 2× 峰值 | `hierarchy.rs:532-570, 624-632` |
| I4 | 低中 | pinyin sidecar 落盘用 `to_allocvec` 全量缓冲(index_cache 已改流式 `to_io`,此处未跟进) | `pinyin_sidecar.rs:265-288` |
| I5 | 低 | 索引缓存加载字节与结构双驻留 | `index_cache.rs:32-46` |

### 3.4 正面确认

无上限容器仅历史 5000/查询键 8/窗口 512/sidecar delta 4096/行缓冲 1MB,均有界;任务与线程无泄漏;全局静态无大数据;索引不存全路径、`path_for` 按需构造。

---

## 四、软件稳定性

背景:release 配置 `panic = "abort"`(`Cargo.toml`),任何 panic 直接终止进程;非测试代码 unwrap 仅 2 处且受前置条件保护,两进程均装有 panic hook。

### 4.1 严重(进程死亡 / 重启循环 / 全局挂死)

**S1 首建落盘失败 → 索引服务自杀式无限重启循环**
`indexer_runtime.rs:918` 把首次构建后的 `index_cache::save` 失败作为致命错误向上传播(`acquire_initial_index` Err → `run()` Err → 服务退出)。对比 rebuild 分支(`indexer_runtime.rs:723-733`)已把同样失败明确降级("先发布继续服务")。触发条件:磁盘满 / ACL 损坏 / 杀软锁住 `index-v5.bin`。后果:SCM 重启 → 全盘 MFT 重扫(分钟级 IO)→ 再失败 → 再退出,无限循环。
**修复**:与 rebuild 路径对齐,save 失败只 `set_error` 降级,内存索引继续服务,留给 6 小时 maintenance checkpoint 重试落盘。

**S2 单 STA Shell worker 无超时,一个弹 UI 操作挂死所有文件动作**
`shell.rs:96-99`(单线程 worker)、`shell.rs:147-166`(`recv()` 无限期等待)。所有 Shell 动作(UAC、properties 对话框、IFileOperation 冲突确认、openas)与 `zip_with_external` 的 `cmd.status()`(`zip.rs:233`,分钟级)都串行在这一个 STA 线程。任何弹窗没人点 → worker 永久阻塞 → 后续 execute/reveal/recycle 无限排队,32 槽位满后每个调用再各占一个 blocking 线程,`ShellExecutor::drop` 的 `join()` 也挂。
**修复**:zip 外部进程挪出 STA 线程;STA 操作加超时 + worker 报废重建;确认类 UI 交前端完成或设 `FOF_NOERRORUI`。

**S3 resolve_window / record_window_switch 直接跑在 tokio worker 上**
`ipc.rs:609-628`:同步调用 `SystemWindowProbe::probe`(`window_list.rs:412-464`,含 `GetWindowTextW`——对挂死窗口是同步 `SendMessageW`,可无限阻塞)。同文件 window 搜索路径(`ipc.rs:1092-1099`)已明确注释并用了 `spawn_blocking`,这两处漏了。broker 只有 2 个 worker,两个此类请求即**全进程僵死**(连管道 accept 都停)。
**修复**:与 window_search 一致,包 `spawn_blocking`(结果为 owned 数据,无 Sync 约束问题)。

### 4.2 高

- **H1** `run()` select 分支内长任务不响应 SCM Stop:`indexer_runtime.rs:712-743`(rebuild 分钟级)、`:762/782`(checkpoint 数秒-数十秒),stop 最长等分支结束;wait_hint 仅 10 秒(`prism-indexer-service.rs:45-58`)→ 被强杀 → 关停 checkpoint 截断 → 缓存偏旧甚至全量重建。**修复**:分支内 `tokio::select!` 包裹 blocking 任务与 `stop.cancelled()`,或按阶段拉长 wait_hint。
- **H2** 读锁内重活与写锁内全卷 compaction 对撞:`indexer_runtime.rs:507`(持读锁做拼音 sidecar 磁盘加载)、写侧 `:1207-1237`(写锁内 `apply_records` + 全卷 O(n) compaction,注释自认秒级)。搜索读锁等待数秒 → broker `REQUEST_BUDGET=8s` 超时 → 重连风暴。**修复**:拼音加载移出读锁(锁外加载再装回);compaction 分段/增量。
- **H3** broker 管道实例创建失败即退出 serve:`ipc.rs:336-339`,`create`/`connect` 任一失败 `?` 传播 → `main.rs:48-51` `exit(1)`,活动连接全断。**修复**:仅首个实例创建失败(真双实例)才退出;循环内失败重试退避。附带:broker 管道未设 `.reject_remote_clients(true)`(indexer 侧有),多一条远程连接面。

### 4.3 中

- **M1** MFT 枚举中途 IO 错误被静默吞掉(`ntfs.rs:555`,`Err(_error) => break` 连日志都没有)→ 索引永久缺文件且"看似完整"。**修复**:记日志并让该卷走既有的跳过 + degraded 路径。
- **M2** 全局单连接 Mutex 把所有索引搜索串行化,排队上限 `LOCK_WAIT=10s` + `REQUEST_BUDGET=8s`(`indexer_client.rs:128-143`);慢期间前端表现为整体卡死。**修复**:失败快速化(已有等待者直接降级)或少量并发连接池。
- **M3** `record_at` 持写锁做 fsync 落盘(`history.rs:208-247`),读侧 `weights()` 又在 async 上下文直接调用(`ipc.rs:1132`)——与 P1/P3 同源,修复方案一致(锁内克隆、锁外落盘、读侧进 `spawn_blocking`)。
- **M4** UpdatePreferences 吞错误且可阻塞约 18 秒(`ipc.rs:581`,`let _ = ...` 无日志)。**修复**:短超时 + 失败 log,或后台 spawn 异步生效。
- **M5** zip 的 Shell COM 路径立即假成功(`zip.rs:273-307`,`Folder.CopyHere` 异步提交却直接 `Ok(Success)`)→ UI 显示成功但只有 22 字节空壳,且失败残留损坏空 zip。**修复**:轮询 `Folder.Items()` 计数至稳定,或返回"后台进行中"语义;失败删除残留。
- **M6** 任一 search 请求可翻转全局 `pinyin_enabled`(`indexer_runtime.rs:505-510`,false → 释放 sidecar;maintenance 对 Disabled 不再重建)→ sidecar 反复释放/重载并放大 H2。**修复**:开关只由显式管理命令修改,Search 请求只读。

### 4.4 低

- **L1** 连接无空闲超时(`ipc.rs:440`、`indexer_runtime.rs:1398-1416`),挂死客户端任务常驻——加 60s 级 idle timeout;
- **L2** 日志同步 IO 在调用线程含 async 上下文(`logging.rs:185-234`)——改 channel + 专职写线程;
- **L3** `fs_util.rs:15-19` TOCTOU——`ReplaceFileW` 失败回退 `std::fs::rename`;
- **L4** 缓存命中路径发布前不检查 stop(`indexer_runtime.rs:821-834`),与首建循环不对称。

### 4.5 C# 侧(自查)

- **C1【中高】** 交互式动作无限期占住管道锁:`PipeClient.SendAsync`(`PipeClient.cs:483`)对动作类请求按设计不设读超时("属性页/复制确认合法等待任意久"),持有 `_ioLock` 期间**所有后续搜索请求排队冻结**,直到对话框关闭。与 Rust S2 是同一设计的两端。**修复**:动作走独立管道连接(或 broker 先回 ack、完成后再通知),搜索通道永不被交互动作占用。
- **C2【中】** 握手读无超时:`ConnectInnerAsync` 读 hello 用 `CancellationToken.None`(`PipeClient.cs:175`),broker 半死(连上不握手)时 StartAsync 永久挂住且持有 `_ioLock`。**修复**:握手读加 2-5 秒超时。
- **C3【低】** `PollUntilReadyAsync`(`SearchViewModel.cs:995`)轮询期间每个请求都全量走管道,已有 seq 守卫,可接受;`RefreshAsync` 的 `Task.Delay(3000)` 未随取消释放,轻微。
- 正面确认:单实例互斥 + FirstPipeInstance、JobObjectGuard、低级键盘钩子 keep-alive 与卸载、WinEventHook 卸载、IImageList vtable 手工对齐、favicon 授权门控与原子写、`OnContextMenuRequested` 的序号防串台——均正确。

---

## 五、修复优先级路线图

| 批次 | 内容 | 预期收益 |
|---|---|---|
| 1(立即) | S1(一行语义修改)→ S3(包 spawn_blocking)→ S2(zip 挪出 STA + 超时重建) | 消除重启循环与全进程僵死风险 |
| 2 | H1 / H2 / H3 + M6 | 服务停机体验、搜索可用性、重连风暴 |
| 3 | A1 窗口渲染路径改造(去 AllowsTransparency + DWM 圆角阴影)+ A4 图标缓存策略 | 动画流畅度的一次性架构投资 |
| 4 | P1 / P3 / P2 / P4 / P5(+M3 同源修复) | 每击键分配减半以上,打字流畅 + 内存双收 |
| 5 | C1 管道分通道 + C2 握手超时 + A2 ActionPanel 同步化 | 前端交互冻结消除 |
| 6 | U1-U7 界面美化 + M5 zip 假成功 + M1/M2/M4 | 观感与长尾稳定性 |
| 7(收尾) | I1 MFT 分批消化 + I3/I4 + P11 空闲 trim + L 系列 | 峰值内存与资源卫生 |

## 附:审计方法

- UI/动画/美观与 C# 稳定性:人工逐文件通读(`SearchWindow`、`ResultList`、`ActionPanel`、`SearchHeader`、`PinButton`、`Styles`、`Tokens`、`SearchViewModel`、`App`、`AppState`、`PipeClient`、`IndexerGenerationClient`、`HostProcessGuard`、`JobObjectGuard`、`SingleInstance`、`HotkeyService`、`ThemeWatcher`、`TrayService`、`HostScopeController`、`ExplorerHostAdapter`、`FaviconCache`、`IconCache`、`WebIconProvider`、`SettingsWindow`、`Prism.csproj`);
- Rust 内存专项与稳定性专项:独立两轮完整通读(ipc.rs / hierarchy.rs / history.rs / indexer_runtime.rs / indexer_client.rs / indexer_ipc.rs / ntfs.rs / pinyin*.rs / window_list.rs / shell.rs / file_ops.rs / zip.rs / actions.rs / apps.rs / index_cache.rs / config.rs / persistence.rs / logging.rs / main.rs / lib.rs / fs_util.rs / root_scope.rs / prism-indexer-service.rs)。
