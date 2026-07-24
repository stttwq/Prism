# Prism 前端规格书（frontend-spec.md · v2 复刻版）

> 用途：交给任意 AI 直接编写 Prism 前端代码，无需二次猜测。
> 视觉基准：`界面/呼出界面.png`、`界面/搜索界面.png`、`界面/右键文件界面.png`（Listary 截图）。**目标是 1:1 复刻这三张图的观感**，仅品牌名换成 Prism。
> 技术栈（已定，不得更换）：C# / .NET 8 / WPF，进程名 `Prism.exe`。
> 后端为独立 Rust 进程 `prism-core.exe`，命名管道 `\\.\pipe\prism-core`，UTF-8 JSON、每行一条消息。
> 界面文字全部中文；常驻内存预算 ≤ 30MB。

---

## 0. 三种界面状态（对应三张截图，同一个窗口切换）

| 状态 | 触发 | 外观要点 |
|---|---|---|
| **Idle 待输入** | 双击 Ctrl 呼出、输入为空 | 只有一条 68px 高的白色圆角输入条，无列表。占位文字"搜索应用和文件"，右侧淡灰放大镜 Logo，右上角外挂一个 28px 白色圆形按钮（内有 45° 斜向箭头图标，点击=固定窗口不自动隐藏，再点取消） |
| **Results 结果** | 输入非空且有结果 | 输入条下方展开结果面板（同一块白色圆角卡片内，输入区与列表间有 1px 浅灰分隔线）。最多显示 9 行，右侧细滚动条。第 1~9 行右端显示 `Ctrl+1`…`Ctrl+9`；列表末尾固定追加一行"展示更多 '<关键词>' 的文件搜索结果"（图标为蓝底白色双箭头方块，副标题"热键: 双击 Ctrl"） |
| **Actions 动作** | 在选中某个文件结果时按 → 键 | 输入区左侧出现大号灰字"动作"+ 竖直分隔线，占位文字复位为"搜索应用和文件"（此时输入可过滤动作）。列表变为动作列表（见第 1 节 ActionPanel），按 ← 或 Esc 返回结果状态 |

## 1. UI 组件树（Component Tree）

```
App (App.xaml — 资源字典、DI、单实例、托盘、快捷键钩子)
├── SearchWindow (无边框、置顶、宽 660px、水平居中、顶部距屏幕 25% 高度)
│   ├── PinButton          右上角外挂圆形按钮(28px, 斜箭头图标, 切换"固定窗口")
│   ├── SearchHeader       输入区(高68px)
│   │   ├── ModeLabel      仅Actions状态可见: 大号灰字"动作" + 右侧1px竖分隔线
│   │   ├── QueryTextBox   无边框输入框, 占位"搜索应用和文件"
│   │   └── BrandIcon      右侧24px放大镜Logo(淡灰描边风格, 装饰用)
│   ├── ResultList         Results状态: 虚拟化列表, 行高62px, 最多9行+末尾"展示更多"行
│   │   └── ResultItem     [32px图标] [标题(匹配段蓝色)/灰色小字副标题 两行] [右端 Ctrl+N 灰字]
│   ├── ActionPanel        Actions状态: 行高44px
│   │   ├── ActionItem     [20px单色线性图标] [动作名] [有子菜单时右端">"箭头]
│   │   └── SectionLabel   灰色小标题"快捷菜单"(分隔基础动作与系统右键菜单项)
│   └── (无StatusBar——截图中没有, 索引中/空态提示放在列表区域内以单行文字显示)
├── SettingsWindow (普通窗口: 常规页/网页搜索页/关于页, 同v1)
└── TrayIcon (托盘: 打开设置/重建索引/退出)
```

## 2. 状态管理设计（State Management）

MVVM，CommunityToolkit.Mvvm。全局单例 `AppState`：

```csharp
public enum PanelMode { Idle, Results, Actions }

public partial class AppState : ObservableObject
{
    [ObservableProperty] private PanelMode mode = PanelMode.Idle;
    [ObservableProperty] private string query = "";
    [ObservableProperty] private IReadOnlyList<SearchResult> results = [];
    [ObservableProperty] private int selectedIndex = 0;
    [ObservableProperty] private SearchResult? actionTarget = null;   // Actions状态针对的文件
    [ObservableProperty] private IReadOnlyList<ActionItem> actions = [];
    [ObservableProperty] private int selectedActionIndex = 0;
    [ObservableProperty] private bool isPinned = false;               // PinButton 状态
    [ObservableProperty] private bool isIndexing = false;
    [ObservableProperty] private bool isBackendConnected = false;
    [ObservableProperty] private AppTheme theme = AppTheme.Light;
    [ObservableProperty] private Settings settings = Settings.Default;
}

public record SearchResult(
    string Kind,        // "app" | "file" | "folder" | "web" | "more"(展示更多行)
    string Title, string Subtitle, string ExecuteId,
    int[]  MatchSpans); // 标题中要染蓝的区间 [start,len,...]

public record ActionItem(
    string Id,          // 回传后端执行: "open_folder"|"copy"|"cut"|"copy_path"|"shell:<n>"
    string Label,       // "打开所在文件夹"/"复制"/...
    string IconGlyph,   // Segoe Fluent Icons 字形码, 无图标传""
    bool   HasSubmenu,  // 右端">"箭头
    bool   IsSectionHeader); // true=渲染成灰色小标题"快捷菜单"

public record Settings(string HotkeyMode, string ComboHotkey, bool AutoStart,
                       List<WebEngine> WebEngines);
```

流转规则：`Query` 只由 QueryTextBox 写；`Results/Actions/IsIndexing/IsBackendConnected` 只由 PipeClient 写；`Mode` 只由 SearchViewModel 的命令写；`Theme` 只由 ThemeWatcher 写。空 Query → `Mode=Idle`；有结果 → `Mode=Results`；对文件按 → 键 → 请求动作列表成功后 `Mode=Actions`。

## 3. 组件 API 定义

| 组件 | 输入（绑定） | 输出（事件/命令） |
|---|---|---|
| `SearchHeader` | `Mode`, `Query`(双向), 占位文字 | `TextChanged`；↑↓/Enter/Esc/←/→/Ctrl+1..9 按键上抛 |
| `PinButton` | `IsPinned`(双向) | 无 |
| `ResultList` | `ItemsSource`, `SelectedIndex`(双向), `ShowHotkeyHints:bool` | `ItemInvoked(SearchResult)` |
| `ResultItem` | `Result`(DataTemplate) | 点击冒泡 |
| `ActionPanel` | `ItemsSource:IReadOnlyList<ActionItem>`, `SelectedIndex`(双向) | `ActionInvoked(ActionItem)` |
| `HotkeyRecorderBox` | `Value`(双向) | `ValueChanged` |

SearchViewModel 命令：`ExecuteSelected`(Enter/双击/Ctrl+N 直达第N项)、`EnterActions`(→，仅 Kind=file/folder)、`LeaveActions`(←)、`ExecuteAction`(Enter)、`MoveSelection(delta)`、`Hide`(Esc；`IsPinned=true` 时失焦不隐藏，Esc 仍隐藏)、`ShowMore`(选中"more"行回车＝把结果上限从 100 提到 1000 重新搜索)。

## 4. 目录结构

```
src/Prism/
├── App.xaml(.cs)
├── Windows/  SearchWindow.xaml(.cs) · SettingsWindow.xaml(.cs)
├── Controls/ SearchHeader · PinButton · ResultList(含ResultItem模板) · ActionPanel · HotkeyRecorderBox
├── ViewModels/ SearchViewModel.cs · SettingsViewModel.cs
├── Models/   AppState · SearchResult · ActionItem · Settings
├── Services/ PipeClient · HotkeyService · ThemeWatcher · TrayService · AutoStartService · SettingsStore · IconCache(按路径取32px系统文件图标并缓存)
└── Themes/   Tokens.Light.xaml · Tokens.Dark.xaml · Styles.xaml
```

## 5. 样式约束（设计令牌 · 依截图取值）

浅色=复刻截图；深色=同布局的等价配色。控件内禁止写死数值，一律引用资源键：

| 资源键 | 浅色(截图实测近似) | 深色 | 用途 |
|---|---|---|---|
| `BgWindow` | #FFFFFF 不透明 | #202124 | 卡片背景（截图为实色白，**不用**亚克力） |
| `BgItemSelected` | #ECEEF0 | #2E3033 | 选中/悬停行整行浅灰 |
| `TextQuery` | #37393E | #E8EAED | 输入的大字 |
| `TextPlaceholder` | #B8BCC2 | #6E7276 | 占位文字/放大镜Logo/"动作"字样 |
| `TextTitle` | #303237 | #E8EAED | 结果标题非匹配部分 |
| `TextMatch` | #1E7AD4 | #6FB5F2 | 标题匹配段（蓝色，不加粗不下划线） |
| `TextSubtitle` | #9B9FA6 | #9AA0A6 | 路径副标题/Ctrl+N提示/"热键: 双击 Ctrl" |
| `Divider` | #E8EAEC | #3C4043 | 输入区与列表间1px横线、"动作"竖线 |
| `ScrollThumb` | #D4D7DA | #5F6368 | 4px宽圆头滚动条 |

尺寸/字体令牌（两主题相同）：
`WindowWidth=660` `RadiusWindow=10` `ShadowBlur=40,Opacity=0.22,OffsetY=8`
`HeaderHeight=68` `FontSizeQuery=26`(常规体) `FontSizePlaceholder=22`
`ItemHeight=62` `IconSizeItem=32` `FontSizeTitle=17` `FontSizeSubtitle=13` `FontSizeHotkeyHint=15`
`ActionItemHeight=44` `IconSizeAction=20` `FontSizeAction=16` `FontSizeSectionLabel=15`
`PadX=20`(卡片左右内边距) `FontFamily=微软雅黑, Segoe UI`
行内布局：图标左缘距卡片 20px，文本块距图标 14px，标题在上副标题在下、行距 2px；Ctrl+N 靠右距边 20px。
PinButton：直径 28px、白底、圆形阴影，锚在卡片右上角外侧(右移 12px、上移 12px)。
动画：窗口淡入 120ms + 上移 6px；Results/Actions 面板展开高度动画 100ms EaseOut；隐藏淡出 80ms。

## 6. 关键交互流程伪代码

```
流程A：呼出/隐藏
  双击Ctrl(400ms内) 或 自定义组合键 → 清空Query、Mode=Idle → 显示+淡入 → 焦点到输入框
  Esc: Actions状态→退回Results; 否则→隐藏
  失焦: IsPinned==false 时隐藏, true 时保持

流程B：输入→搜索→更新(复刻图2)
  QueryTextBox.TextChanged(text):
      AppState.Query=text
      text为空 → Results=[]; Mode=Idle; return
      防抖50ms → PipeClient.send {"type":"search","query":text,"max":100}
  收到 {"type":"results","items":[...]} 且对应最新query:
      Results = items + [more行(Kind="more",Title="展示更多 '"+query+"' 的文件搜索结果",Subtitle="热键: 双击 Ctrl")]
      SelectedIndex=0; Mode=Results
      渲染: 每项标题按MatchSpans把匹配段染TextMatch蓝色; 前9项显示Ctrl+1..9

流程C：执行
  Enter/双击/Ctrl+N: 若选中Kind=="more"→ShowMore重搜; 否则 send {"type":"execute","id":..} → 成功后隐藏
  Ctrl+Enter(文件): send {"type":"reveal","id":..} → 隐藏

流程D：动作面板(复刻图3)
  Results状态按→ 且选中Kind∈{file,folder}:
      send {"type":"actions","id":ExecuteId}
      收到 {"type":"actions","items":[...]}:   # 前5项固定: 打开所在文件夹/复制/剪切/复制路径至剪贴板/(可用时)推荐编辑器
                                               # 之后: SectionHeader"快捷菜单" + 系统右键菜单项(shell:<n>, 可带HasSubmenu)
      ActionTarget=选中项; Actions=items; Mode=Actions; 输入框清空可过滤动作(按Label子串)
  Actions状态 Enter: send {"type":"run_action","id":ExecuteId,"action":ActionItem.Id} → 隐藏
  ← 键: Mode=Results(结果与选中位置保持)

流程E：后端守护
  管道断开 → IsBackendConnected=false → 列表区显示单行灰字"正在重新连接…" → 指数退避重启后端(最多5次) → 重连后重发当前Query
```

## 7. 与后端的消息合同（新增部分）

前端会用到的全部消息：`search`/`results`、`execute`、`reveal`、`actions`、`run_action`、`status`(后端推送 isIndexing)。Rust 后端必须按第 6 节字段实现；系统右键菜单的枚举与执行（IContextMenu）由后端完成，前端只展示与回传。

## 给实现 AI 的硬性要求

1. 视觉以三张截图为准，令牌取值照第 5 节，不自行发挥；先做浅色主题达到与截图肉眼难辨，再做深色。
2. 类名/目录/资源键严格按本文件；用户可见文字全中文。
3. ResultList 启用 UI 虚拟化；文件图标经 IconCache 异步加载，不阻塞输入。
4. 后端未就绪/断开时 UI 不卡死不崩溃；支持中文安装路径（全程 Unicode API）。
5. Ctrl+1..9 仅在 Results 状态注册为窗口内快捷键，不做全局热键。
