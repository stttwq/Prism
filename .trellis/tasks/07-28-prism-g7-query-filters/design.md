# Design

## Parser Contract

broker 使用单次有限状态扫描解析 query，识别未转义的 `ext:`、`path:` 和双引号值，输出 `name_query` 与结构化 filters。解析器保留原 token span，任何无效语法都回退为普通文本而非报错丢弃。

```text
raw query -> parser -> name_query + FilterSet
FilterSet = extensions(OR) AND paths(AND) AND optional root
```

filters 使用稳定协议对象传给 indexer，限制过滤器数、单值长度、扩展数量和总序列化大小。WPF 不自行重写语法，只负责展示输入和接收解析结果。

## Execution

名称候选先通过低成本 extension 条件，再按需构造/验证 path，合格后进入 G1 有界堆。`is_truncated` 只针对全部过滤后的候选。相同规范化 FilterSet 参与缓存键和稳定排序输入。

## Compatibility And Rollback

filters 是可选协议字段，旧客户端缺失时等同空集合。新客户端连接不支持 filters 的后端时应明确禁用语法/返回兼容错误，不能发送名称被剥离但后端未过滤的请求。功能可整体关闭并将所有 token 恢复为普通查询。
