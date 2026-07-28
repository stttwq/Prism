# Design

## Ownership

broker 拥有用户级 `history-v1`，按 typed stable target 聚合成功操作。indexer 拥有机器级文件/文件夹拼音 sidecar；应用和瞬态窗口由 broker 使用同一版本的紧凑音节/词组规则即时编码。两侧共享生成规则和测试语料，不共享可写文件。

实现第一步以原型比较 mmap sidecar、紧凑偏移表和增量 overlay；默认选择“indexer mmap 主 sidecar + 有界 rename delta”，除非同一 G0 语料证明无法满足 10MB/P95 门禁。该技术门只改变存储表示，不得改变已锁定匹配行为。

## Pinyin Sidecar

sidecar 头包含 magic、schema、字典版本、索引 generation/identity、长度和校验信息；主体保存名称记录到音节序列/首字母及汉字 span 的紧凑映射。USN 新增/改名进入有界 delta，删除进入 tombstone，达到阈值后由服务内部重建。

普通客户端只能选择是否启用拼音，不暴露重建或任意写命令。关闭时释放 mmap/overlay；失败时返回字面结果并暴露脱敏状态。

## Ranking

匹配等级是不可越级的枚举：Literal、FullPinyin、Initials。每级内部再比较 exact/prefix/substring；历史只调整同级 score。所有结果复用 G1 的稳定 tie-break。高亮 spans 是可选协议字段，旧 UI 可忽略。

## History Persistence

history 保存 stable target、kind、execute/reveal/destination count、last used UTC。窗口展示前与当前枚举求交集，不持久化 HWND。写入采用同目录临时文件、flush、原子替换；schema 不兼容或损坏时隔离旧文件并从空历史继续。

## Privacy, Compatibility And Rollback

日志只记录 schema、条目数、错误类别和耗时。新 sidecar 可以删除并自动退回字面搜索；关闭历史不删除数据，只有“一键清除”删除历史。回滚代码前保留旧数据文件但旧版本不得错误解析新 schema。
