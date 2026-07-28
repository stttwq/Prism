# Design

## Measurement Boundary

基准驱动器以普通用户连接现有 broker 管道，复用真实端到端搜索路径；服务控制和进程采样放在独立 PowerShell 脚本中。所有输出写入显式指定的结果目录，不写入产品缓存目录。

```text
Release binaries -> wait for index ready -> warm-up -> query samples
                                      \-> synchronized process samples
samples + environment manifest -> deterministic summary
```

## Data Contract

建议采用 JSONL 原始样本与 Markdown/JSON 摘要：每条样本包含运行 id、query id、max、cold/warm 分类、开始时间、elapsed、result count、进程内存快照和可用的后端计数器。环境清单与运行 id 绑定，避免跨机器误比较。

查询文本可保存在仓库测试夹具中；用户真实查询、真实完整路径和窗口标题不得进入基线。需要路径语料时使用仓库内或临时目录中的合成名称。

## Reproducibility

- Release 构建产物与 commit 绑定；脚本拒绝把 Debug 进程作为正式结果。
- 索引 ready/generation 通过协议判断，不依赖固定 sleep。
- 预热次数、正式轮数、请求顺序和超时固定；跨卷查询显式记录卷集合。
- 进程消失、服务未 ready、响应错位或超时使该次运行失败，不生成“成功”摘要。

## Compatibility And Safety

G0 优先调用现有协议字段。G1 才新增工作量计数器时，G0 脚本以可选字段兼容升级。所有脚本默认只读；任何临时测试数据必须位于经校验的临时子目录并在文档中说明清理方式。

## Rollback

删除新增的基准脚本和结果样例即可回滚；不得修改或删除产品缓存、服务注册或用户配置。基线报告属于历史测量记录，不因后续优化而覆盖，应新增一次运行记录。
