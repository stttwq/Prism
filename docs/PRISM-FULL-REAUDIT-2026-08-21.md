# Prism 全仓从头重审（2026-08-21）

> 动作快捷键特性（A1-A3，提交 49385d9/47e80cd/785270b）合入后，摒弃旧文档，
> 三路通读全部源码（Rust 核心大文件 / Rust 其余 / C# 前端全量）。
> 第一轮：高 2 + 中 7 + 低若干；按约定修中+高（含新特性低危 1 条顺手）。
> 第二轮：验证代理逐项复核修复正确性 + 改动面缺陷扫描，结论「无新中高危」。

## 高（2，全修）

### H1 SearchHeader 只放行 Ctrl 组合——Alt/Shift 系动作快捷键永远到不了匹配表
- `SearchHeader.OnQueryPreviewKeyDown` 的 default 分支原先只上抛含 Ctrl 的组合；
  Alt 组合以 `Key.System` 到达且修饰只有 Alt，直接被吞。A2 写的 SystemKey 匹配是死码。
- 修：default 放行「任意带修饰键组合 + Key.System」；未命中的组合在搜索窗
  不置 Handled，原样落回文本框。导航键系/Ctrl+G/Ctrl+数字行为零差异（二轮复核确认）。

### H2 history record_at 破坏 MRU 降序不变量——空查询「最近使用」列表错序
- entries 降序只在 prune（超容量）/load 恢复；record_at 对已有条目原地更新、
  新条目尾插，两次 prune 之间无序。weights() 按下标序返回，空查询 MRU 注入
  按序截断——刚用过的文件沉底进不了列表，clear 后完全倒序。
- 修：weights() 慢路径重建时显式按 last_used 降序（tie-break 与 prune 一致）。
  锚测试 `weights_returns_true_mru_order_between_prunes` 用「尾插序与 MRU 序
  分歧」的纯粹形（去掉 sort 必挂）。

## 中（7，全修）

1. **rename 无守卫误执行**（新特性+右键菜单同构路径）：动作面板列表无 rename
   时循环未命中仍 `ExecuteActionAsync()`，静默执行 FirstSelectable（若首个是
   mutation 类有误删面）。修：`TrySelectAction` 未命中即退回/中止；右键菜单
   路径补 `IndexOfResult<0` 中止（菜单打开期间 generation 刷新换行场景）。
2. **FaviconCache 直连 URL 双 scheme**：`$"https://{key}/favicon.ico"` 而 key
   已含 scheme，直连下载必 DNS 失败，隐私顺序（先直连后聚合）名存实亡。修：`$"{key}/favicon.ico"`。
3. **favicon 授权不更新 `_hostSettings`**：授权门闭包读旧快照，图标要等下次
   完整保存/重启才显示。修：`DownloadFavicon` 启动下载前把 origin 合入快照
   （UI 线程，引用替换原子；与 ApplySearchExclusions 的写序经二轮复核无覆盖）。
4. **语义错误被当传输错误**（indexer_client）：`IndexerResponse::Error`（如
   >4KB 查询拒绝）产出无 root_rejection 的失败，持久连接路径据此丢连接重连
   重试——每击键一次连接抖动，重试必得同错。修：SearchFailure 增 `semantic`
   标记，重试臂排除语义错误；所有 Error 响应臂改 semantic 构造。
5. **one-off 闸排队无预算**（indexer_client）：`ONE_OFF_GATE.acquire` 在
   REQUEST_BUDGET timeout 之外，慢期排队实际无界（8s×任务数/2）。修：acquire
   挪进 timeout 内，超时 RAII 释放许可。
6. **SetPinyinEnabled 限速按连接计**（indexer_runtime）：last 记在每连接局部，
   开 N 条连接即 N 个独立窗口，AU 进程可绕过限速打成拼音重建风暴（本就是该
   限速要防的）。修：进程级 `static Mutex<Option<Instant>>`，毒化保守拒绝。
7. **initial_name_bytes 载入后恒 0**（hierarchy）：serde skip 字段无载入恢复，
   needs_name_compact 阈值退化为常量 16MB，names 池超 16MB 的卷每次缓存命中
   启动都白压一次（全卷 clone+全池重写）。修：`recompute_derived_counters`
   一并恢复基线（载入/压缩后池长即新基线；compact 内冗余赋值删除）。

## 新特性低危顺手（1）

- 动作快捷键命中后先置 Handled 再查适用性，与「不适用不吞键」注释矛盾。修：
  同步预检 `ActionShortcutApplies`（模式/类型/ExecuteId）通过才置 Handled。

## 未修低危清单（留档）

- window_list process_path MAX_PATH 固定缓冲（超长路径进程 app_name 空，HWND
  复用检测弱化）。
- shell.rs reveal 参数未转义路径内引号（文件名含 `"` 时定位失败，无注入面）。
- ipc.rs 跨类型去重大小写口径不一致（仅大小写差异的 .lnk 目标去重落空）。
- indexer_runtime 启动期 listener 创建失败即致命退出（broker 同场景是降级）。
- rebuild 期间管道服务死亡不被察觉（上报推迟到重建完成）。
- 8MB 响应行上限可被极深目录树合法极值击穿（与语义断连叠加才痛，已修 4）。
- reload_engines 无条目数上限；连接准入 check-after-add 瞬时超额 1-2；
  current_user_sid 错误路径句柄泄漏（一次性）+ Vec 对齐引用（理论 UB）。
- pinyin_sidecar chinese_count 对「有汉字但编码失败」的名字永久漏计（提前
  全量重建，自愈）；logging 单次写失败永久关闭本会话文件日志；zip 探测缓存
  后装 7-Zip 不生效；config index_refresh_secs 死配置；actions validate_path
  只校验 trim 副本。
- C#：Win 修饰键 WPF Keyboard.Modifiers 永不报告（录不出 Win 组合，文案误导）；
  动作面板态过滤词污染 history query 参数；rename 预填名未同步进输入框；
  快捷键 Win 组合同因。

## 测试

cargo 398 通过/0 失败（新增 2：weights MRU 真 MRU 锚、基线恢复锚）；
dotnet 295 通过/0 失败；clippy 0 警告。二轮复核代理实测目标测试全绿。
