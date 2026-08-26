# Prism 前端六项缺陷修复施工方案（2026-08-26）

本文面向执行者（另一个模型/开发者）。所有结论都已在代码与运行日志中定位到具体行，
不需要重新排查。请按本文顺序施工，不要扩大范围。

涉及的构建基线：分支 `feature`，最新提交 `c2140cf`（批次3）。
问题 1 / 3 / 5 / 6 都是批次 2（`ebb712e` 托盘 WPF 菜单 + 别名列表分组折叠 + 整页折叠）引入的回归。

---

## 总体施工原则

1. **只改这三个文件**：`src/Prism/Windows/SettingsWindow.xaml`、
   `src/Prism/Windows/SettingsWindow.xaml.cs`、`src/Prism/Services/TrayService.cs`。
   如果你觉得需要动第四个文件，先停下来说明原因，不要顺手改。

2. **根治而不是抑制**。本轮两个崩溃都不许用「加 try/catch 让它别炸」收尾：
   问题 1 必须消除异常源本身（只读属性的 TwoWay 绑定），问题 5 必须消除窗口被
   关闭后仍被复用的状态。兜底 try/catch 只作为**额外**的第二层防线加在
   WinForms 回调边界上（见 5.4），不能替代前面两条。

3. **改共享位置，不要逐处打补丁**。问题 6 的黑字来自 `SettingsExpanderStyle`
   模板里 ToggleButton 没接 Foreground；一处修好会同时修掉「动作快捷键」
   「暂存区」「文件别名」三个标题。不要在三个 Expander 上各写一遍 Foreground。

4. **主题色一律走令牌**（`Tokens.Light.xaml` / `Tokens.Dark.xaml` 的键，
   用 `DynamicResource`）。本轮不允许新增任何硬编码颜色。
   例外：`SettingsPrimaryButton` 现有的 `Foreground="White"`（蓝底主按钮）是
   有意为之，不要动它。

5. **不改行为，只改渲染与生命周期**。别名的分组规则、动作目录、快捷键解析、
   保存流程一律不动。

6. **每项改完单独验证**（见文末验证清单），不要六项一起改完再跑。
   问题 1 / 2 / 6 由新增的单元测试覆盖；3 / 4 / 5 是托盘与窗口非客户区，
   只能手工验证，请如实报告手工结果，不要写「应该没问题」。

---

## 问题 1：点击「文件别名」分组折叠，前端进程直接消失

### 现象与证据

前端进程终结，`prism-core.exe` / `prism-indexer-service.exe` 存活。
`%LOCALAPPDATA%\Prism\logs\frontend.log` 第 17389–20106 行：

```
DispatcherUnhandledException: XamlParseException: 无法对
"MS.Internal.Data.CollectionViewGroupInternal" 类型的只读属性"Name"
进行 TwoWay 或 OneWayToSource 绑定。
...（同一条异常在 40ms 内重复 50 余次）
DispatcherUnhandledException: 60s 内超过 50 次，放弃拦截（异常: XamlParseException）
AppDomain.UnhandledException (isTerminating=True): XamlParseException: ...
```

### 位置

`src/Prism/Windows/SettingsWindow.xaml:747-758`，别名 ListBox 的
`GroupStyle.ContainerStyle` → `GroupItem` 模板 → `Expander.Header`：

```xml
749  <TextBlock FontSize="12" FontWeight="SemiBold"
750             Foreground="{DynamicResource TextTitle}">
751      <Run Text="{Binding Name}"/>
752      <Run Text=" (" FontWeight="Normal" .../>
753      <Run Text="{Binding ItemCount}" FontWeight="Normal" .../>
754      <Run Text=")" FontWeight="Normal" .../>
755  </TextBlock>
```

### 根因

`Run.Text` 这个依赖属性的元数据带 `FrameworkPropertyMetadataOptions.BindsTwoWayByDefault`
（WPF 为了支持 `RichTextBox` 编辑），所以 `{Binding Name}` 在不写 `Mode` 时**默认是 TwoWay**。

分组头的 DataContext 是 WPF 内部类型 `MS.Internal.Data.CollectionViewGroupInternal`，
它的 `Name` 与 `ItemCount` 都是只读属性。TwoWay 绑定在建立时即抛
`XamlParseException`。

这个异常发生在布局（measure）路径上，`ContextLayoutManager.UpdateLayout` 每帧重试，
于是变成异常风暴。`src/Prism/App.xaml.cs:530 ExceedsExceptionBudget()` 的设计是
60 秒内超过 50 次 Dispatcher 异常就认定为持续性故障、放弃拦截
（`e.Handled = false`），进程随即终结。所以用户看到的是「点一下折叠就闪退」。

只有别名列表会触发：分组描述是在 `SettingsViewModel.LoadAliasesAsync()`
（`src/Prism/ViewModels/SettingsViewModel.cs:366-370`）里挂到 `AliasEntries` 默认视图上的，
必须真的有别名数据、分组头被实体化，才会命中。

### 改法

把 header 的四个 `Run` 换成一个普通 `TextBlock` 绑定，彻底不碰 `Run.Text`：

```xml
<Expander.Header>
    <TextBlock FontSize="12" FontWeight="SemiBold"
               Foreground="{DynamicResource TextTitle}">
        <TextBlock Text="{Binding Name, Mode=OneWay}"/>
        <TextBlock Text="{Binding ItemCount, Mode=OneWay, StringFormat=' ({0})'}"
                   FontWeight="Normal"
                   Foreground="{DynamicResource TextSubtitle}"/>
    </TextBlock>
</Expander.Header>
```

（内联 `TextBlock` 在 `TextBlock.Inlines` 里是合法的 `InlineUIContainer` 简写；
`TextBlock.Text` 默认 OneWay，不会重现这个坑。）

如果希望保留 `Run` 的写法，最小改动是给两个数据绑定显式加 `Mode=OneWay`：
`<Run Text="{Binding Name, Mode=OneWay}"/>`、
`<Run Text="{Binding ItemCount, Mode=OneWay}"/>`。两种都可接受，选一种。

**同时**给这个 `Expander` 套上现有样式（原来用的是 WPF 默认 Expander 模板，
深色下头部是黑字箭头，与问题 6 同一类缺陷）：

```xml
<Expander Style="{StaticResource SettingsExpanderStyle}"
          IsExpanded="True" Margin="0,0,0,4">
```

### 为什么这么改

- 消除异常源本身，而不是提高异常预算或加 `Handled=true`：只读属性的 TwoWay
  绑定在任何主题、任何数据量下都会抛，抑制只会把闪退换成「UI 卡顿 + 日志风暴」。
- `Mode=OneWay` 是这类分组头绑定的唯一正确写法；分组名与计数本来就不该回写。
- 不要改 `App.xaml.cs` 的异常预算逻辑。它这次的行为是正确的（持续性故障时
  诚实退出比无限冻结更好），改它等于把真实故障藏起来。

---

## 问题 2：动作快捷键行的「选择动作」框极细，与右侧录键框不同高

### 位置

`src/Prism/Windows/SettingsWindow.xaml:162-233`，`SettingsComboBoxStyle` 的
`ControlTemplate`，第 196-201 行：

```xml
196  <ContentPresenter Grid.Column="0"
197                    Margin="{TemplateBinding Padding}"
198                    HorizontalAlignment="Left"
199                    VerticalAlignment="Center"
200                    IsHitTestVisible="False"
201                    TextElement.Foreground="{TemplateBinding Foreground}"/>
```

### 根因

这个 `ContentPresenter` **没有绑定 Content**。裸 `ContentPresenter` 的隐式行为是
`Content="{TemplateBinding Control.Content}"`，而 `ComboBox` 派生自
`ItemsControl`／`Selector`，**根本没有 `Content` 属性**。所以选中项永远不渲染。

后果有两层：
1. 选中的动作名（图标 + 名称 + 说明）从来没显示过；
2. 框的期望高度只剩 `Padding="8,5"` 的 10px 加 1px 边框 ≈ 12px，
   而同一行右侧的 `HotkeyRecorderBox` 是 `MinHeight="32"`
   （`src/Prism/Controls/HotkeyRecorderBox.xaml:5`），于是两个框明显不齐。
   截图里那条斜纹是被压扁的内容加上 WPF 默认虚线焦点框。

`ActionHotkeyEditItem.Id` 本身是有值的（`SettingsViewModel.AddActionHotkey()`
在 `src/Prism/ViewModels/SettingsViewModel.cs:274` 就填了首个可用动作 id），
右侧 `ScopeText`「文件 / 文件夹 / 应用」能正常显示也证明数据没问题——
纯粹是模板缺绑定。

### 改法

模板里补上标准 ComboBox 选择框的三个绑定，并把高度对齐：

```xml
<ContentPresenter Grid.Column="0"
                  Margin="{TemplateBinding Padding}"
                  Content="{TemplateBinding SelectionBoxItem}"
                  ContentTemplate="{TemplateBinding SelectionBoxItemTemplate}"
                  ContentStringFormat="{TemplateBinding SelectionBoxItemStringFormat}"
                  HorizontalAlignment="Left"
                  VerticalAlignment="Center"
                  IsHitTestVisible="False"
                  TextElement.Foreground="{TemplateBinding Foreground}"/>
```

并在 `SettingsComboBoxStyle` 的 Setter 区补三项：

```xml
<Setter Property="MinHeight" Value="32"/>
<Setter Property="VerticalContentAlignment" Value="Center"/>
<Setter Property="FocusVisualStyle" Value="{x:Null}"/>
```

`Path`（下拉箭头）右侧 `Margin="0,0,10,0"` 保持不变。

### 为什么这么改

- `SelectionBoxItem` / `SelectionBoxItemTemplate` 是 WPF 为「选择框显示什么」
  专门提供的只读属性，正是原生 ComboBox 模板用的东西。用它就自动继承了
  `ItemTemplate`（图标 + 标签 + 说明），不需要为选择框再写一份模板。
- `MinHeight="32"` 与 `HotkeyRecorderBox.MinHeight` 取同一个数值，两个框同高；
  不要改 `HotkeyRecorderBox` 去迁就 ComboBox，那个控件在暂存区页也在用。
- `FocusVisualStyle="{x:Null}"` 去掉与整套自绘样式冲突的系统虚线框，
  与本项目其他自绘控件（`ResultListBoxStyle` 的 ItemContainerStyle 等）一致。

---

## 问题 3：深色模式下右键托盘图标，菜单是白框黑字

### 位置

`src/Prism/Services/TrayService.cs:132-134`：

```csharp
132  _menu.Placement = PlacementMode.MousePoint;
133  _menu.StaysOpen = false;
134  _menu.IsOpen = true;
```

### 根因

这里**没有设置 `PlacementTarget`**。对照搜索结果的右键菜单
`src/Prism/Windows/SearchWindow.xaml.cs:691-696`：

```csharp
var menu = new ContextMenu
{
    PlacementTarget = Results,
    Placement = PlacementMode.MousePoint,
};
menu.SetResourceReference(FrameworkElement.StyleProperty, "PrismContextMenuStyle");
```

两者用的是同一套 `PrismContextMenuStyle` / `PrismContextMenuItemStyle`
（`src/Prism/Themes/Styles.xaml:191`、`:215`），搜索窗那个在深色下正常。
唯一差别就是 `PlacementTarget`：没有它的 `ContextMenu` 不挂在任何视觉/逻辑树上，
`DynamicResource` 的令牌解析不经过应用主题字典，退回系统默认（浅色）。

另外 `_menu ??= BuildMenu()`（`:102`）把菜单永久缓存了一次，
主题切换后也不会重建，会把错误状态固化下来。

### 改法

在 `ShowContextMenuOnDispatcher()` 里，**锚点窗创建之后**、`IsOpen = true` 之前：

```csharp
_menu.PlacementTarget = _anchor;
_menu.Placement = PlacementMode.MousePoint;
_menu.StaysOpen = false;
_menu.IsOpen = true;
```

注意执行顺序：现有代码是先 `_menu ??= BuildMenu()`（`:102`）再创建 `_anchor`（`:107`），
赋 `PlacementTarget` 的语句必须挪到 `_anchor` 已非 null 之后。

### 为什么这么改

- `PlacementTarget` 给 ContextMenu 提供了继承上下文，令牌解析走应用资源，
  与已验证可用的搜索窗菜单走同一条路径。这是同类问题的既有解，不引入新机制。
- 顺带保证菜单与锚点窗生命周期一致：锚点窗被隐藏/重建时菜单不会指向悬空目标。
- 不需要订阅 `ThemeWatcher.ThemeApplied` 手动重建菜单：接上树之后
  `DynamicResource` 自己会重解析。**不要**为此加事件订阅。

---

## 问题 4：深色模式下设置窗口的标题栏（非客户区）仍是白色

### 位置

- `src/Prism/Windows/SettingsWindow.xaml:1-11`：标准窗口边框，
  只设了 `Background="{DynamicResource BgSettingsPage}"`（客户区）。
- 全仓 `grep -rn "DwmSetWindowAttribute" src/` 零命中：从来没有做过深色标题栏。

### 根因

WPF 只管客户区。Windows 的标题栏、边框、最小化/最大化/关闭按钮属于非客户区，
由 DWM 绘制，必须显式调用 `DwmSetWindowAttribute` 打开
`DWMWA_USE_IMMERSIVE_DARK_MODE` 才会变深色。

同一页面还有第二处同类缺陷：`SettingsWindow.xaml:493` 和 `:562` 两个
`ScrollViewer` 没套样式，用的是 WPF 默认滚动条模板，深色页面上是浅色滚动条
（截图右侧那条浅色竖条）。项目里已有 `PrismScrollViewer`
（`src/Prism/Themes/Styles.xaml:68`，`ResultListBoxStyle` 已在用）。

### 改法

**4.1 深色标题栏** —— 在 `src/Prism/Windows/SettingsWindow.xaml.cs` 加：

```csharp
private const int DwmwaUseImmersiveDarkMode = 20;        // Win10 2004+ / Win11
private const int DwmwaUseImmersiveDarkModeLegacy = 19;  // Win10 1809–1909

[DllImport("dwmapi.dll")]
private static extern int DwmSetWindowAttribute(
    IntPtr hwnd, int attr, ref int value, int size);

private void ApplyTitleBarTheme()
{
    var hwnd = new WindowInteropHelper(this).Handle;
    if (hwnd == IntPtr.Zero) return;
    var dark = ThemeWatcher.ReadSystemTheme() == AppTheme.Dark ? 1 : 0;
    if (DwmSetWindowAttribute(hwnd, DwmwaUseImmersiveDarkMode, ref dark, sizeof(int)) != 0)
        DwmSetWindowAttribute(hwnd, DwmwaUseImmersiveDarkModeLegacy, ref dark, sizeof(int));
}
```

在构造函数里挂 `SourceInitialized += (_, _) => ApplyTitleBarTheme();`。

复用现有的 `ThemeWatcher.ReadSystemTheme()`（`src/Prism/Services/ThemeWatcher.cs:54`，
已经是 `static`），**不要**为此往 `SettingsWindow` 注入 `ThemeWatcher` 或 `AppState`，
也不要订阅 `ThemeApplied`：设置窗是短生命周期的模态式窗口，
开着的时候改系统主题属于可以忽略的边角情况。

**4.2 深色滚动条** —— 给 `SettingsWindow.xaml:493` 与 `:562` 两个 `ScrollViewer`
加 `Style="{StaticResource PrismScrollViewer}"`。
`:219`（ComboBox 下拉内部的 ScrollViewer）可选，改不改都行；`DataGrid` 的内部
滚动条不在本轮范围。

### 为什么这么改

- `DwmSetWindowAttribute` 是这件事**唯一**的官方途径。替代方案（`WindowChrome`
  自绘标题栏）要重做最小化/最大化/拖拽/贴靠/双击最大化全套交互，代价与风险都远高。
- 先试 20 再回退 19：属性编号在 Win10 1809–1909 是 19、2004 起是 20。
  失败时 `DwmSetWindowAttribute` 返回非 0 且无副作用，两次调用是安全的。
- 滚动条复用 `PrismScrollViewer` 而不是新写一套：项目里已有并已在搜索窗验证。

---

## 问题 5：桌面/任务视图里多一个来源不明的小窗口（也是 `闪退.txt` 那次崩溃的根因）

### 位置

`src/Prism/Services/TrayService.cs:104-135`，托盘 WPF 菜单的锚点窗。

### 根因（两个缺陷，同一个对象）

**5.a 窗口从不隐藏，且能被用户看见/关闭。**
`:107-117` 创建锚点窗（`Width=0`、`Height=0`、`WindowStyle=None`、
`AllowsTransparency=true`、`Left/Top=-32000`、`ShowInTaskbar=false`），
`:126` `_anchor.Show()` 之后**菜单关闭时没有对应的 `Hide()`**，窗口一直处于
Visible。`ShowInTaskbar=false` 只去掉任务栏按钮，**挡不住 Alt+Tab 与
任务视图（Win+Tab）**——那需要扩展样式 `WS_EX_TOOLWINDOW`。
它的 `Background` 也没设，用的是系统默认白色。这就是截图里那个白色小窗。

**5.b 窗口被关闭后仍被复用 → 崩溃。**
既然用户能在任务视图里看到它，就能把它关掉。关闭后 `_anchor` 字段仍非 null、
`_anchorShown` 仍为 true，下一次右键托盘执行 `:126 _anchor.Show()` 抛
`InvalidOperationException`。`闪退.txt` 的栈正是如此：

```
System.InvalidOperationException: 关闭窗口后，无法设置可见性，也无法调用 Show、
ShowDialog 或 WindowInteropHelper.EnsureHandle。
   at System.Windows.Window.VerifyCanShow()
   at System.Windows.Window.Show()
   at Prism.Services.TrayService.ShowContextMenuOnDispatcher()
   at Prism.Services.TrayService.ShowContextMenu()
   at Prism.Services.TrayService.OnMouseClick(Object sender, MouseEventArgs e)
   at System.Windows.Forms.NotifyIcon.WmMouseUp(MouseButtons button)
   at System.Windows.Forms.NotifyIcon.WndProc(Message& msg)
   at System.Windows.Forms.NativeWindow.Callback(...)
```

注意这次异常直接弹了 JIT 调试对话框、没被 `App.xaml.cs` 的
`DispatcherUnhandledException` 接住：`ShowContextMenu()`（`:92-93`）走的是
`_dispatcher.CheckAccess()` 为真的同线程直调分支，异常沿 WinForms
`NotifyIcon.WndProc` 原生回调栈向上抛，**不经过任何 `DispatcherOperation`**，
WPF 的兜底钩子覆盖不到。

### 改法（四步，全部在 `TrayService.cs`）

**5.1 菜单关闭即隐藏锚点窗。** `BuildMenu()` 里给菜单挂一次：

```csharp
menu.Closed += (_, _) => _anchor?.Hide();
```

**5.2 锚点窗被关闭时清空状态，下次重建。** 创建锚点窗后立即挂：

```csharp
_anchor.Closed += (_, _) => { _anchor = null; _anchorShown = false; };
```

**5.3 让锚点窗彻底不可见于任何窗口切换器。** 创建时加
`Background = System.Windows.Media.Brushes.Transparent`，并挂：

```csharp
_anchor.SourceInitialized += (s, _) =>
{
    var hwnd = new WindowInteropHelper((Window)s!).Handle;
    var ex = GetWindowLong(hwnd, GwlExStyle);
    SetWindowLong(hwnd, GwlExStyle, ex | WsExToolWindow | WsExNoActivate);
};
```

常量与声明：`GWL_EXSTYLE = -20`、`WS_EX_TOOLWINDOW = 0x00000080`、
`WS_EX_NOACTIVATE = 0x08000000`，`GetWindowLong` / `SetWindowLong`
（64 位用 `GetWindowLongPtr` / `SetWindowLongPtr`，或直接用
`GetWindowLong`/`SetWindowLong` 的 `IntPtr` 重载）。

> `WS_EX_NOACTIVATE` 与现有 `SetForegroundWindow(hwnd)`（`:130`）并不冲突：
> 显式 `SetForegroundWindow` 仍然生效，而窗口不会被 Alt+Tab 选中。
> 如果实测发现加了它之后菜单点外部不收起，就只加 `WS_EX_TOOLWINDOW`，
> 并在提交说明里记一句。

**5.4 WinForms 回调边界补兜底。** 把 `ShowContextMenuOnDispatcher()` 的整个方法体
包进 `try/catch`，`catch` 里调 `App.LogException("TrayService.ShowContextMenu", ex)`
（`src/Prism/App.xaml.cs:545`，已是 `internal static`）。
同样处理 `OnMouseClick` 中的 `Raise(ShowSearchRequested)` 路径——
`Raise()`（`:165-172`）在同线程时也是直调，同样绕过 WPF 兜底。

**5.5 `Dispose()` 保持现状即可**（`:212-222` 已经在关锚点窗并置 null）。
只需确认 5.2 挂的 `Closed` 处理器不会在 Dispose 里造成二次赋值问题——
它把字段置 null，与 Dispose 的行为一致，无害。

### 为什么这么改

- 5.1 + 5.3 从两个方向根治「多出一个窗口」：不该显示的时候不显示，
  必须显示的短暂期间也不进入任何窗口切换器。只做其中一条都不够
  （只 Hide 的话菜单打开期间仍会短暂出现在任务视图）。
- 5.2 是崩溃的根治点：状态与对象真实生命周期一致，即使将来又有别的原因
  让窗口被关闭（系统关机通知、第三方工具、未来代码路径），也不会再复用死对象。
- 5.4 是第二层防线，针对的是一整类问题：WinForms `NotifyIcon` 回调栈上抛出的
  任何异常都杀进程且 WPF 兜底覆盖不到。这里必须加，但它不是 5.2 的替代品。
- **保留锚点窗这个方案本身。** 它的注释（`:104-106`）说明了存在理由：
  托盘弹 WPF 菜单要有一个能成为前台的窗口，否则点击外部不收起。
  更彻底的做法是换成 0 尺寸的 `HwndSource` 消息窗（天然带
  `WS_EX_TOOLWINDOW`、用户无法关闭），但那是更大的改动，本轮不做；
  如果 5.3 实测效果不理想，再作为升级路径提出。

### 关于「前端不会自动重连」

日志 `11:59:43.900 崩溃后由 WER 自动重启（上次进程非正常终结…）` 表明那次
`RegisterApplicationRestart`（`src/Prism/App.xaml.cs:66`）是生效了的。
该 API 要求进程崩溃前存活 ≥60 秒，启动后很快崩溃则不会被拉起。
**本轮不要改 WER 相关代码**，真正的修法是上面两个崩溃的根治。

---

## 问题 6：深色模式下折叠分节的标题是黑字

### 位置

`src/Prism/Windows/SettingsWindow.xaml:390-453`，`SettingsExpanderStyle`。
样式在 `:391` 设了 `Foreground="{DynamicResource TextTitle}"`，
但模板里 `:400-436` 的 `ToggleButton x:Name="HeaderSite"` 没有接 Foreground：

```xml
400  <ToggleButton x:Name="HeaderSite"
401                IsChecked="{Binding IsExpanded, Mode=TwoWay, ...}"
402                Background="Transparent"
403                BorderThickness="0"
...
432  <ContentPresenter Grid.Column="1" ContentSource="Header" .../>
```

### 根因

`Control.ForegroundProperty` 就是 `TextElement.ForegroundProperty`（AddOwner），
是可继承属性。`ToggleButton` 的默认主题样式（Aero2）把自己的 `Foreground` 设为
`SystemColors.ControlTextBrush`（黑），于是 `:432` 那个 Header 的
`ContentPresenter` 继承到的是黑色，Expander 上设的 `TextTitle` 被截断在
ToggleButton 这一层。

浅色模式下黑字碰巧正确，所以只在深色下暴露。受影响的是三个纯字符串 Header：
「动作快捷键（搜索窗口内生效）」「暂存区」「文件别名」；
问题 1 里那个别名分组 Expander 套上样式后也一起受益。

### 改法

给 `HeaderSite` 接上模板父的 Foreground：

```xml
<ToggleButton x:Name="HeaderSite"
              IsChecked="{Binding IsExpanded, Mode=TwoWay, RelativeSource={RelativeSource TemplatedParent}}"
              Foreground="{TemplateBinding Foreground}"
              Background="Transparent"
              ...>
```

### 为什么这么改

- 修在样式里，一处覆盖全部使用方；不要在三个 Expander 标签上各写一遍 Foreground
  （那是问题会复发的写法，下一个 Expander 又会忘）。
- `TemplateBinding Foreground` 而不是直接写 `{DynamicResource TextTitle}`：
  这样调用方仍可以通过设 `Expander.Foreground` 覆盖，样式默认值继续由 `:391` 提供。
- 顺便检查（不必修改）：模板里 `Arrow` 用的是 `{DynamicResource TextSubtitle}`，
  显式设了，不受这个继承问题影响。

---

## 施工顺序

1. 问题 1（崩溃，先修）
2. 问题 5（崩溃 + 幽灵窗口）
3. 问题 6（一处修好，也让问题 1 的分组头受益）
4. 问题 2
5. 问题 3
6. 问题 4

---

## 回归测试（必须新增）

新建 `src/Prism.Tests/SettingsWindowVisualTests.cs`，一个文件三条断言，
覆盖问题 1 / 2 / 6。STA 线程与布局泵的写法**照抄现有的**
`src/Prism.Tests/WebIconFlickerVisualTests.cs`（`RunOnSta` 在 `:214`，
`FindDescendant<T>` 在 `:231`，还有 `Pump()`），不要另造一套。

构造 ViewModel 与窗口：

```csharp
var vm = new SettingsViewModel(
    new SettingsStore(tempDir), new AutoStartService(),
    onAliasList: () => Task.FromResult<IReadOnlyList<AliasEntry>>(new[] { /* 2~3 条不同 Kind */ }));
var win = new SettingsWindow(vm);
// 深色令牌合并进窗口自己的资源，避免依赖 Application.Current
win.Resources.MergedDictionaries.Add(new ResourceDictionary
{
    Source = new Uri("pack://application:,,,/Prism;component/Themes/Tokens.Dark.xaml"),
});
```

（`SettingsStore` 的构造参数以现有 `src/Prism.Tests/SettingsStoreTests.cs` 的用法为准。
若 pack URI 在无 `Application` 的测试进程里加载失败，参照
`WebIconFlickerVisualTests` 里加载控件资源的既有做法处理。）

三条断言：

1. **问题 1**：切到快速访问页、`await vm.LoadAliasesAsync()`、展开「文件别名」、
   泵一次布局，再折叠分组、泵一次。
   在 STA 线程上挂 `Dispatcher.CurrentDispatcher.UnhandledException`
   收集异常，断言集合为空（修复前会收到大量 `XamlParseException`）。
2. **问题 2**：`vm.AddActionHotkeyCommand.Execute(null)`，泵布局，
   找到该行的 `ComboBox` 与 `HotkeyRecorderBox`：
   断言 `ComboBox.ActualHeight >= 32` 且与录键框高度差 ≤ 2px；
   断言选择框里能找到文本非空的 `TextBlock`（选中项真的渲染了）。
3. **问题 6**：找到「暂存区」Expander 的 header `ContentPresenter` 下的
   `TextBlock`，断言其 `Foreground` 等于 Tokens.Dark 的 `TextTitle`
   （`#E8EAED`），而不是 `SystemColors.ControlTextBrush`。

问题 3 / 4 / 5 无法用单元测试覆盖（托盘、DWM 非客户区），走手工验证。

---

## 手工验证清单

在**深色**系统主题下逐条确认（`AppsUseLightTheme = 0`），然后在浅色下回归一遍：

| # | 步骤 | 期望 |
|---|------|------|
| 1 | 打开设置 → 快速访问 → 展开「文件别名」（需先有别名数据）→ 反复折叠/展开 10 次 | 不闪退；`frontend.log` 无新增 `XamlParseException` |
| 2 | 快速访问 → 点「＋ 添加动作」 | 左侧框显示图标 + 动作名，与右侧录键框同高、上下居中 |
| 3 | 右键托盘图标 | 菜单深底浅字，悬停高亮为深灰，文字不变黑 |
| 4 | 打开设置窗口 | 标题栏与边框深色；右侧滚动条为细深色条 |
| 5 | 右键托盘弹出菜单后按 Esc / 点空白处关闭 → Win+Tab 与 Alt+Tab | 看不到任何多余的 Prism 小窗口；反复右键 20 次不闪退 |
| 6 | 快速访问页 | 「动作快捷键」「暂存区」「文件别名」「别名分组头」全部为浅色字 |

补充回归：切换系统深浅色时搜索窗与结果右键菜单仍正常（本轮没动这条路径，
但问题 3 改了共享的 `PrismContextMenuStyle` 使用方，顺手确认一下）。

---

## 明确不要做的事

- 不要改 `src/Prism/App.xaml.cs` 的异常预算（`ExceedsExceptionBudget`）或
  `RegisterApplicationRestart` 相关逻辑。
- 不要改 `src/Prism/Controls/HotkeyRecorderBox.xaml` 的 `MinHeight`
  去迁就 ComboBox。
- 不要为托盘菜单新增 `ThemeWatcher.ThemeApplied` 订阅。
- 不要把托盘锚点窗改成 `HwndSource`（记为升级路径，本轮不做）。
- 不要用 `WindowChrome` 自绘设置窗标题栏。
- 不要新增任何硬编码颜色，不要新增依赖，不要改 `Tokens.*.xaml` 的键名。
- 不要顺手重排/格式化未涉及的 XAML 区块——保持 diff 可读。

---

## 提交建议

按施工顺序拆两个提交（崩溃修复与外观修复分开，便于回滚）：

1. `fix(ui): 别名分组头只读属性 TwoWay 绑定导致闪退 + 托盘锚点窗生命周期`
   （问题 1、5）
2. `fix(ui): 深色模式折叠标题/托盘菜单/标题栏配色 + 动作选择框选中项渲染`
   （问题 2、3、4、6）
