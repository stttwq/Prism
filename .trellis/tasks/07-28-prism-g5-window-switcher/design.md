# Design

## Enumeration And Identity

broker 在普通用户会话按请求枚举顶层窗口，采集 HWND、PID、process start identity、title、application identity 和可切换标志。返回给 WPF 的 opaque id 只在该次枚举 snapshot 内有效；broker 保存有界短期 snapshot，execute 时重新核对全部身份。

窗口历史使用应用稳定身份与规范化标题指纹，不保存 HWND。展示最近窗口时先实时枚举，再把历史与当前候选关联。

## Mode And Ranking

WPF 将 `>` 解析为显式 Window mode，不与文件/应用/网页混排。broker 将标题和应用名分别匹配，选择更强匹配来源并返回 spans/source。排序复用 G2 的 Literal > FullPinyin > Initials 和同级历史规则。

## Activation

激活在 broker 的有界 window service 中完成：重新验证、必要时恢复最小化窗口、调用 Windows 允许的前台切换路径并验证结果。若 Windows 前台限制拒绝，返回明确 failure；不使用注入、模拟任意输入或结束进程绕过。

## Compatibility And Rollback

`window` 是可选稳定 kind，旧 UI 映射 Unknown。功能有独立开关/模式解析；回滚时移除窗口 provider 和 `>` 入口，历史中 window 条目可保留但不展示。
