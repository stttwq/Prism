# `load_cache` 耗时实测（AC4）

## 结论

- 当前机器加载现有索引缓存耗时为 **83–87 ms**，5 次均值 **84.8 ms**、中位数 **85 ms**。
- 正式复测前的一次独立探测为 **130 ms**，同样远低于设计中的 **2 s** 判断线。
- 因此重启时的缓存加载阶段不需要纳入 R2 的索引进度提示；R2 保持聚焦于无缓存的首次全量构建。

## 环境与样本

- 日期：2026-07-26（Asia/Shanghai）
- Git：`5d8766d590a18cab20da4528f5dcc9bdb7e9d9f4`，测量前工作区干净
- 系统：Windows `10.0.22631.0`，AMD64 Family 25 Model 80 Stepping 0
- Rust：`rustc 1.97.1 (8bab26f4f 2026-07-14)`
- 后端：当前源码执行 `cargo build --release --manifest-path src/prism-core/Cargo.toml` 后的 `prism-core.exe`
- 缓存：`src/prism-core/target/release/data/index.bin`，18,334,306 bytes
- 索引：616,223 条目；字符串池 12.39 MB；条目表 7.05 MB；反序列化后合计约 19.44 MB

## 方法

每次启动一个独立 Release 后端进程，通过重定向 stderr 等待日志 `加载耗时 Nms`，记录后立即终止该测量进程，避免命名管道常驻服务和定时刷新干扰下一次。缓存文件在测试期间未修改。

计时范围来自 `index::build_or_load` 的现有 `Instant`：包含读取 `index.bin`、postcard 反序列化、版本检查、`shrink_to_fit`、统计日志以及把结果写入共享索引。

本次为正常重启/暖系统缓存场景；没有借助外部工具强制清空 Windows 文件缓存。正式复测前的首个独立进程为 130 ms，可作为本次会话中较保守的观测值。

## 原始结果

| 轮次 | `加载耗时` |
| ---: | ---: |
| 1 | 83 ms |
| 2 | 86 ms |
| 3 | 83 ms |
| 4 | 87 ms |
| 5 | 85 ms |

- 最小值：83 ms
- 最大值：87 ms
- 均值：84.8 ms
- 中位数：85 ms

## AC4 判定

AC4 要求把 `load_cache` 的实测毫秒数记入任务 `research/` 或 spec。本文件已记录可复核结果，**AC4 通过**。
