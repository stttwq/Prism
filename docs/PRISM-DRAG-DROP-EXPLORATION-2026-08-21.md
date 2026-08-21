# Prism 拖拽探索

> **文档性质:设想探索,非实施计划。** 闲聊中围绕"拖拽"这个方向发散的思考、联网调研和技术方案,供将来参考。
>
> 日期:2026-08-21
> 上下文:盘点未探讨方向时发现拖拽是"搜到→处理"闭环的真实缺口

## 起点:为什么拖拽是缺口

之前判断"搜到→处理"闭环已满(打开/复制路径/压缩/重命名全齐),但那是"选中后触发动作"。拖拽是另一种模态——把搜索结果直接拖到别处去用:拖进邮件当附件、拖进文件夹复制、拖进聊天窗口发文件。这个交互现有动作面板覆盖不了,而它是"用身体记忆完成的操作",比按键更快。

**拖出去比拖进来价值大得多。** 拖出去是"搜到文件→拖到别处用",这是闭环缺的一环。拖进来场景弱——搜索框是打字的,拖文件进来干嘛(除非拖进来自动填路径搜索,但那不如直接打字)。

## 联网调研

### WPF 拖拽 API(Microsoft 官方文档)

**拖出去(drag source)**——结果列表项是拖源:
- 监听 `MouseMove` + 左键按下,调用 `DragDrop.DoDragDrop(dragSource, data, allowedEffects)`
- 数据用 `DataObject.SetFileDropList(StringCollection)` 装文件路径列表
- `FileDrop` 格式是 Windows 标准(CF_HDROP),拖到资源管理器、邮件、聊天窗口都认
- `allowedEffects` 传 `Copy`(从搜索结果拖出去是复制语义,不是移动原文件)
- `DoDragDrop` 返回值是 drop target 的 `Drop` 事件里设的 `DragEventArgs.Effects`,告知拖拽最终效果

**拖进来(drop target)**——搜索框或窗口接受外部文件拖入:
- `AllowDrop = true`
- `Drop` 事件里 `GetDataPresent(DataFormats.FileDrop)` 检查,`GetData` 拿路径
- 拖进来后可把路径填进搜索框搜、或直接打开

**拖拽事件序列**:
- 拖源:`GiveFeedback`(持续,改鼠标光标)、`QueryContinueDrag`(持续,按键状态变化,可取消拖拽)
- 放目标:`DragEnter` → `DragOver`(持续)→ `DragLeave` 或 `Drop`

**重要提醒**:`GiveFeedback` 和 `QueryContinueDrag` 拖拽期间持续触发,handler 里别做重活(别每次 new Cursor,用缓存的)。

### 对标产品情况

- **Listary**:搜索结果是否支持拖出无明确文档,有多选功能(多选+拖出是自然组合)
- **Everything**:Win32 listview 原生支持拖出到资源管理器,拖到其他应用不一定
- **PowerToys Run**:没找到拖拽支持文档,作为现代启动器很可能没做

拖拽不是对标产品标配,但也不是没人做。Prism 做了算补"大家都有点但不统一"的缺口。

## 实现要点(纯前端,不碰 broker)

拖拽是纯 UI 层操作,文件路径搜索结果里已有(path 字段),不需要 broker 参与,不需要新协议。只改 ResultList 控件 + SearchWindow。

### 核心代码模式

```csharp
// ResultList 项的 MouseMove handler
private void ResultItem_MouseMove(object sender, MouseEventArgs e)
{
    if (e.LeftButton == MouseButtonState.Pressed && !_isDragging)
    {
        // 阈值判断:移动超过几个像素才算拖拽,否则算点击
        var pos = e.GetPosition(this);
        if (Math.Abs(pos.X - _dragStart.X) > SystemParameters.MinimumHorizontalDragDistance ||
            Math.Abs(pos.Y - _dragStart.Y) > SystemParameters.MinimumVerticalDragDistance)
        {
            _isDragging = true;
            var data = new DataObject();
            var files = new StringCollection { _selectedItem.Path };
            data.SetFileDropList(files);
            DragDrop.DoDragDrop(this, data, DragDropEffects.Copy);
            _isDragging = false;
        }
    }
}
```

## 真实难点(交互层面,不是 API)

### 1. 拖拽与现有操作的冲突

结果列表现在有:单击选中、双击打开、Ctrl+数字打开、`→`进动作面板。加左键按住拖动,得区分"点击选中"和"按住拖动"——Windows 标准做法是鼠标移动超过 `SystemParameters.MinimumHorizontalDragDistance` / `MinimumVerticalDragDistance` 像素阈值才算拖拽,否则算点击。WPF 不自动做,得自己在 MouseMove 里判断位移。不做的后果:每次点选都误触发拖拽。

### 2. 拖拽时的窗口行为(最关键的坑)

Prism 失焦即隐。但拖拽时用户按住左键往窗外拖——鼠标离开 Prism 窗口,算不算失焦?如果算,拖到一半窗口消失了,拖拽就断了。

解法:用 `QueryContinueDrag` 事件,拖拽进行中标记 `_isDragging = true`,失焦处理里检查这个标记:

```csharp
// SearchWindow 的失焦处理
private void OnDeactivated(object sender, EventArgs e)
{
    if (_isDragging) return;  // 拖拽中不隐藏
    Hide();
}
```

### 3. 拖到哪 + 效果语义

常见目标:资源管理器(复制)、邮件(附件)、聊天窗口(发文件)、桌面。都是 Windows 标准拖放目标,`FileDrop` 格式都认,Prism 不用为每个目标适配。

**例外**:拖到资源管理器同盘同目录时 Windows 默认移动而非复制。Prism 拖出去应明确 `Copy` 语义,不允许 `Move`——搜索结果不是文件所有者,不能让拖拽移动原文件。

### 4. 多选拖拽(可选,分步做)

现在结果列表单选。支持多选(Ctrl/Shift)后可一次拖多个文件。多选是独立改动,和拖拽可分步:先单选拖拽,再多选。

## 建议递进路线

1. **单选拖出**:结果行左键按住拖到外部应用,`FileDrop` + `Copy` 语义。解决失焦隐藏冲突(拖拽中不隐藏)。最小可用。
2. **多选拖出**:加 Ctrl/Shift 多选,拖多文件。`StringCollection` 天然支持多路径。
3. **拖入(可选,价值低)**:搜索框 `AllowDrop`,拖文件进来填路径搜索。场景弱,优先级最低。

## 关联文档

- `docs/PRISM-FUTURE-DIRECTIONS-2026-08-20.md` — 完整设想清单(8 方向)
- `docs/PRISM-HOTKEY-EXPLORATION-2026-08-21.md` — 快捷键探索
- `docs/PRISM-CONTENT-SEARCH-EXPLORATION-2026-08-21.md` — 内容搜索探讨
