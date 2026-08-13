# Design

## Web Mode Data Flow

```text
keyword parse -> immediate local WebSubmit item
              -> if opted-in built-in engine: cancellable suggestion request
              -> validate/bound/parse -> append up to 5 items for same request id
```

WPF WebSearchService 拥有单例 HTTP client、per-query cancellation token 和 request sequence。adapter 只接收 query 并返回纯文本候选；UI 合并前再次验证 mode、engine、query sequence。网络异常转换为空联想，不进入全局错误提示。

## Network Limits

每个 adapter 固定 endpoint、method、编码和解析上限。HTTP client 禁止非 http/https，限制重定向、连接/总超时和响应读取字节。解析器先做媒体类型/长度校验，再使用结构化 JSON/parser API，不用字符串切割。

## Favicon

内置图标来自打包资源。自定义引擎以规范化 origin 作为授权与缓存键；每次新 origin 保存时弹一次明确联网许可。下载到临时文件/内存上限内，验证 MIME、实际格式和像素后再原子写缓存。metadata 保存 schema、origin、etag/expiry、文件名和大小；LRU/总大小上限防止无界增长。

## Privacy, Compatibility And Rollback

设置分别保存 `suggestions_enabled` 与 origin favicon grant。日志只记 provider、状态码类别、耗时和错误类别，不记 query/URL。功能关闭立即取消请求。回滚可删除 HTTP service 和缓存 reader；内置直接网页搜索保持可用。favicon 缓存目录必须进入 `dist/prism.iss` 的卸载清单——它是本阶段新增的持久文件，不得留成孤立数据等后续清理。
