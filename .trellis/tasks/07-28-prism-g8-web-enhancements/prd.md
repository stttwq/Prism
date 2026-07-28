# G8 网页图标与在线联想

## Goal

把网页关键词结果收敛为独立、立即可用的网页模式，并在明确用户授权下提供有界、可取消、默认关闭的内置引擎联想和自定义引擎 favicon 缓存，不影响本地搜索或隐私边界。

## Requirements

- 强依赖 G1 的模式、迟到响应和 C# 测试地基。
- 识别网页关键词后进入专用网页模式：首行立即显示提交原查询，其后最多 5 条联想，不混排文件、应用或窗口。
- 在线联想默认关闭，只支持内置 Bing、百度、Google；自定义引擎不支持 suggestion API。
- 查询变化立即取消旧请求并按 request identity 丢弃迟到响应；直接网页结果不等待网络。
- 单次联想 800ms 未返回即放弃；断网、限流、证书、超时、取消、解析失败静默保留直接结果，不显示干扰性错误。
- 使用 WPF 用户会话中的单例可取消 HTTP 服务；LocalSystem indexer 和 broker 不发联想/favicon 网络请求。
- 限制 response bytes、JSON 深度、条目数、单条长度、重定向次数和允许协议；三个内置 adapter 使用固定样本测试。
- 内置引擎图标随程序打包。自定义引擎保存或 origin 变化时单独征求 favicon 联网许可；拒绝、失败或损坏回退通用图标。
- favicon 仅允许 http/https，限制重定向、响应大小、MIME、像素/解码尺寸；缓存按规范化 origin 键控，metadata 版本化、磁盘大小有界。
- favicon 授权与在线联想开关相互独立。
- 网页 query、联想响应、联想选择和 favicon URL 默认不写历史或日志。

## Acceptance Criteria

- [ ] 默认设置下输入网页关键词不会发出联想网络请求，直接结果立即可用。
- [ ] 网页模式只含 1 条直接结果和最多 5 条联想，不混排本地结果。
- [ ] 查询变化取消旧请求，迟到响应、800ms 超时和无效响应不会污染新结果或阻塞 UI。
- [ ] Bing、百度、Google 各有固定解析夹具；自定义引擎不会调用联想 API。
- [ ] favicon 授权、拒绝、origin 变化、缓存命中/过期/损坏、重定向和超限响应有测试。
- [ ] 联想与 favicon 开关独立，失败统一回退直接结果/通用图标。
- [ ] history 和默认日志不含网页 query、联想内容或 favicon URL。
- [ ] 在线服务不计入本地搜索门槛，但本地直接结果仍满足 `max=8` P95 ≤100ms。
- [ ] C# tests、WPF Release build，以及适用的 Rust tests/Clippy 全部通过。

## Out Of Scope

- 自定义 suggestion API；
- 网页查询历史或联想学习；
- broker/indexer 联网；
- 本地结果与网页联想混排；
- 后台预抓全部 favicon。

## Dependencies

强依赖 G1。
