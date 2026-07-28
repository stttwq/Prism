# Design

## Root Search

broker 把规范化 root 解析为卷与 record，indexer 对名称候选沿 parent 链验证 `is_descendant_or_self`，通过后才进入 G1 全局堆。祖先遍历使用访问深度上限和小型 per-request memo，检测环、跨卷、缺父和 tombstone；memo 生命周期限于请求，不扩大常驻 NodeSlot。

```text
query + root -> normalize/map -> name candidates -> ancestor verify
                                              -> Top-K -> final paths
```

## Host Context State

WPF 显式状态包含 host kind、captured HWND、root、识别状态和 scope。每次呼出重新捕获，任何识别失败清空 root 后再切 Global。`Ctrl+G` 只在当前 root 仍有效时切回 CurrentDirectory。

## Adapter Boundary

每类宿主实现 `Detect/GetFolder/NavigateOrFill` adapter：Explorer adapter 使用 Shell COM；SystemFileDialog adapter 使用可访问性/UIA 控件；Opus adapter 使用 13.23 官方外部命令。adapter 返回结构化 capability 和 failure reason，不共享原始 UIA selector 或命令字符串给 indexer。

产品化前用独立原型验证：识别、取目录、定位/回填、取消、宿主关闭、多窗口/标签和失败降级。某 adapter 未过门禁时不发布该宿主支持，但不阻塞 root 搜索本身。

## Security And Rollback

所有宿主调用发生在当前普通用户会话。拒绝控制完整性级别更高的宿主。适配器各有开关，可单独关闭；关闭或失败只清空 host context，不影响全局搜索。协议 root 字段保持可选，旧客户端继续全局搜索。
