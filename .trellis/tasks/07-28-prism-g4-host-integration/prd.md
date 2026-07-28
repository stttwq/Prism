# G4 当前目录与宿主联动

## Goal

在 Prism 独立弹窗中提供默认开启、可关闭的当前目录递归搜索，并通过公开接口、Shell COM、UIA 或 Directory Opus 官方接口与受支持宿主安全联动，全程不注入 DLL。

## Requirements

- 强依赖 G1、G2、G3 全部归档。
- 支持 Windows Explorer、Windows 系统应用的标准打开/保存对话框、Directory Opus 13.23；不承诺浏览器、Office、其他第三方管理器或提权宿主。
- 呼出前保存前台 HWND，识别 HostContext 与当前目录；识别失败、宿主关闭、路径无效或不在索引中时立即切到全局并提示，禁止复用旧目录。
- 当前目录搜索默认开启、可关闭，递归包含全部子目录。UI 显示范围标签，点击或 `Ctrl+G` 在当前目录与全局切换。
- root 规范化为绝对路径并映射到已索引卷/record；非 NTFS、不存在、过长、拒绝访问或不在索引中返回可解释降级。
- indexer 先做名称匹配，再沿 `parent_record` 验证祖先，合格项才进入全局 Top-K；设置最大深度、环检测和缺父节点处理，不给 12B `NodeSlot` 增加伪零内存子指针。
- 只有 G0/G1 基准证明父链方案无法满足 P95，才允许另立内存明确的祖先/子树缓存设计。
- Explorer 优先公开 Shell COM；系统对话框可用 UIA 操作可访问控件，但不得把 UIA 描述为 `IFileDialog::SetFolder`；Opus 优先官方 `dopusrt`/命令接口并结构化转义参数。
- 标准对话框只导航/回填选择，最终打开或保存由用户确认。Explorer/Opus 中 Enter 正常打开，`Ctrl+Enter` 才让原宿主定位结果。
- 支持宿主上下文空输入时显示 root 内相关最近项；其他场景空输入显示当前仍存在的最近窗口。
- 先完成兼容性原型矩阵，通过后才产品化；任一联动失败都保留全局搜索与普通打开。

## Acceptance Criteria

- [ ] root 搜索递归且在 Top-K 前过滤，跨卷、8/1000、环、缺父、深路径和失效 root 有测试。
- [ ] 识别失败或宿主失效不会复用旧目录，UI 明确显示已回到全局。
- [ ] `Ctrl+G`、范围标签、总开关和空输入规则符合规范并有 C# 状态测试。
- [ ] Explorer 多窗口/标签、系统打开/保存对话框、Opus 13.23 各有原型兼容矩阵和 Windows 11 x64 机器验收。
- [ ] 标准对话框不会自动确认打开/保存；Enter 与 `Ctrl+Enter` 在 Explorer/Opus 中保持既定语义。
- [ ] 中文路径、长路径、宿主关闭、路径变化和访问拒绝均有降级测试。
- [ ] 无 DLL 注入、无提权宿主控制、无 LocalSystem Shell/UIA 操作。
- [ ] 三进程总内存 ≤100MB，暖 8/1000 查询满足 G0 门槛。
- [ ] Rust tests、Clippy、C# tests 和 WPF Release build 全部通过。

## Out Of Scope

- 浏览器/Office 对话框、其他第三方文件管理器和提权宿主；
- DLL 注入或把 Prism UI 嵌入宿主；
- 非 NTFS 临时全文扫描；
- 自动点击打开/保存确认。

## Dependencies

强依赖 G1、G2、G3。
