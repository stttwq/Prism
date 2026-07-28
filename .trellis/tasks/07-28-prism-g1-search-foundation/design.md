# Design

## Search Pipeline

```text
scan name metadata -> classify + score -> global bounded heap(max)
                                      -> stable sort -> path_for(finalists)
                                      -> protocol results
```

候选结构不得持有完整路径。排序键集中定义为 `(match_class, score, stable_tie_break)`，其中 match class 的优先级不可被历史或类型权重越级。`max` 由协议限幅后直接决定堆容量；`max=1000` 不经过固定 200 中间上限。

`is_truncated` 表示存在至少一个符合当前查询/范围/过滤条件但未进入返回集的候选，不能简单用 `items.len == max` 推断。generation 来自形成该响应的索引快照。

## IPC Evolution

broker 和 indexer 各自维护协议版本。升级顺序为：reader 接受缺失新字段并提供安全默认值，部署后 writer 才发送新字段。稳定 kind 使用小写字符串；C# converter 将未知字符串保留原值并映射 `Unknown`，UI 只能提供无副作用降级。

每条请求保留 request id 或严格串行语义。若客户端业务层取消显示，传输层仍需读完对应响应，或关闭连接并为后续请求建立新连接，禁止把旧响应误配给新请求。

## WPF State And Cache

缓存键至少包含 query、mode、root、filters、sort version 和 index generation，缓存值包含完整性。只有“前缀增长 + 完整结果”允许派生；首屏 8 条通常是截断结果，不能派生更窄查询。

SearchViewModel 使用注入的搜索网关、generation 网关、调度器和 debounce clock。状态转换与 XAML/窗口句柄分离，生产 timer 只注册一次回调。

## Compatibility And Rollback

新字段均先可选，旧客户端仍可搜索；若新排序发生性能回归，可用阶段内开发开关在同一 G0 语料对照，但合并前必须只保留正确实现。回滚不能让新 reader 无法解析旧响应，也不能改写索引缓存格式。
