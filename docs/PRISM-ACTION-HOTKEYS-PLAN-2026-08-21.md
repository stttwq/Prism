# Prism 动作快捷键实施方案

> 依据：`E:\下载\快捷键设想.md`（2026-08-21）。
> 原则：增量最小侵入，每任务独立测试+提交，导航键与 Ctrl+Enter 语义零改动。

## 目标回顾

动作面板全部动作开放用户绑窗口级快捷键；绑了直触发，没绑走 `→` 面板老路。
设置页把呼出快捷键（全局）、动作快捷键、文件别名归拢成「快速访问」区。
Ctrl+Enter（原宿主定位）保持硬编码，不进可配表。导航键（上下/Enter/←→/Esc/Ctrl+G/Ctrl+数字）保持硬编码。

## 设计决策

### 存储

`Settings.ActionHotkeys: Dictionary<string,string>`（action id → 组合键串，如 `"copy_path": "Ctrl+Shift+C"`）。
- 动作 id 直接复用 broker 现有封闭枚举（actions.rs 15 个面板 id），不另造。
- 空值/缺失 = 未绑定。SchemaVersion 保持 1（纯增量字段，旧文件缺字段反序列化为空表）。

### 动作目录（前端静态镜像）

`ActionHotkeyCatalog`：15 行（id、中文标签、适用类型 file/folder/app），与 broker `allowed_actions`
的并集一致（open_folder/copy/cut/copy_path/properties/open_with/rename/copy_to/move_to/
recycle/delete_permanent/zip/copy_app_path/app_properties/run_as_admin；locate_app 不在任何面板列表，不收录）。
锚测试锁定 id 集合，防止两边漂移。

### 键匹配

`ActionHotkeyTable`（纯逻辑，可单测）：
- 解析：`"Ctrl+Shift+C"` → (Key, ModifierKeys)，接受 HotkeyRecorderBox 全部输出格式
  （含单数字别名 `"Ctrl+1"` → D1，与 HotkeyService.ParseCombo 同规则）。规范化为固定顺序 Ctrl+Alt+Shift+Win+主键。
- 必须含至少一个修饰键（录键框已强制，磁盘手改文件在加载时静默丢弃违规条目）。
- 匹配：修饰键集合精确相等（绑 Ctrl+C 不会误吃 Ctrl+Shift+C）。Alt 组合走 `e.SystemKey`。
- **保留键优先于用户表**：方向键/Enter/Esc（任意修饰）+ Ctrl+G + Ctrl+D0-9/NumPad0-9 永远走原硬编码逻辑，
  用户表查不到它们（保存时校验拒绝绑定，触发时先判保留）。这保证「其他功能正常」为硬不变量，
  而非依赖用户不乱绑。Ctrl+Enter 属 Enter 保留集，自动满足「保持现状」。
- 冲突策略：同一组合键绑两个动作 → 保存时拦截报错（不做后绑覆盖）。

### 触发（SearchWindow.OnHeaderKeyDown）

```
保留键 → 原硬编码逻辑（零改动）
非保留键且用户表命中且 Mode==Results 且选中行类型适用 → 执行动作
否则 → 原逻辑（输入、导航、面板过滤）
```
- 执行复用 `_vm.RunActionOnAsync(target, actionItem)` —— 与动作面板 Enter、右键菜单同一条路
  （成功隐藏/mutation 刷新/copy_to 模态失活守卫/错误文案全部免费继承）。
- 类型不适用（如 app 行按了"重命名"的键）→ 忽略，按键落回输入框。
- rename 特殊：与右键菜单同款序列——先进动作面板再选中 rename 执行（内联编辑需要面板态）。

### 设置页

新「快速访问」tab（常规/网页搜索/关于不动位次只顺延）：
1. 呼出快捷键（标明全局生效）——从常规 tab 原样移入。
2. 动作快捷键——目录 15 行，每行「动作名 + 适用类型 + 录键框（复用 HotkeyRecorderBox）+ 清除」。
3. 文件别名——从常规 tab 原样移入。

### App 接线

启动读盘存副本 → SearchWindow 懒创建时注入 → 设置保存后热更新（`SetActionHotkeys`）。

## 增量任务

| # | 内容 | 影响面 | 测试 | 提交 |
|---|------|--------|------|------|
| A1 | Settings 字段 + 目录 + 匹配表 + Store 载入/保存校验 | 纯新增，无行为变化 | 新增单测（解析/保留/匹配/校验/目录锚）+ dotnet 全量 | 1 |
| A2 | SearchWindow 触发 + rename 路径 + App 接线 | OnHeaderKeyDown 头部加一道查表（保留键先行） | dotnet 全量 + cargo 全量（确认后端无改动回归） | 2 |
| A3 | 设置页快速访问 tab + VM 行集合 + 保存校验 | XAML 移动两节 + 新增一节 | dotnet 全量（新增 VM 保存校验测试） | 3 |
| A4 | 真机总回归 | 三进程重启 + 手工清单（见下） | cargo 396+ / dotnet 全绿 / clippy 0 警告 + 真机 | 4 |

### A4 真机手工清单

- 双击 Ctrl 呼出 → 打字 → Enter 打开（核心路径不变）
- 上下/Enter/→/Esc/Ctrl+G/Ctrl+数字/Ctrl+Enter 全部原行为
- 设置页绑 Ctrl+Shift+O=打开所在文件夹 → 选中文件按键直接开文件夹
- 绑同一键给两个动作 → 保存被拦
- 绑 Ctrl+G → 保存被拦（保留键）
- 类型不匹配（app 行 + 文件动作键）→ 无事发生
- 界面无卡顿、无闪退（呼出动画/面板展开/主题切换）

## 明确不做（YAGNI）

- 录键时实时冲突高亮（保存时拦截已够）
- 每动作多键位/一键多动作
- 导出/导入配置
- 全局动作快捷键（呼出键除外，保持唯一全局）
