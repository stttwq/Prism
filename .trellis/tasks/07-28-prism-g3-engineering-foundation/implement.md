# Implementation Plan

1. [x] 绘制当前生产/测试模块引用图，列出旧链测试与现行链的一一迁移表。
2. [x] 迁移测试到 `hierarchy/indexer_client/indexer_runtime`，确认覆盖后移除 `index.rs` 旧实现、`lib.rs` 的 `pub mod index;` 以及 `search.rs`、`installer/setup.iss` 两个占位文件。
3. [x] 定义 typed action target（复用 G1 已定的稳定 result kind，不重定义）、稳定错误类别和新旧协议兼容测试。
4. [x] 抽取公共 Shell adapter，并实现有界队列、专用 STA 线程、COM 生命周期和安全关闭。
5. [x] 将现有 reveal/属性/打开方式/启动路径迁入公共层，做行为对照。
6. [x] 定型 settings/history/favicon metadata 的 schema 版本与兼容默认值约定，供 G2 直接沿用。
7. [x] 分离机器硬排除与用户过滤快照，写入 G1 预留的可选 `filters` 字段，在 Top-K 前执行并限制条数/长度。
8. [x] 在 `dist/prism.iss` 补 `%ProgramData%\Prism\` 卸载清理，并在验收机各跑一次安装与卸载。
9. [x] 为 broker/indexer 建立独立结构化 rolling log，注入目录失败、磁盘只读和轮转失败。
10. [x] 审计 LocalSystem IPC 命令集合，加入拒绝越权请求的回归测试。
11. [x] 运行全套质量门和 Windows 11 Shell/COM 机器测试，更新 backend/frontend spec。

## Validation Commands

```text
cargo test --manifest-path src/prism-core/Cargo.toml
cargo clippy --manifest-path src/prism-core/Cargo.toml --all-targets -- -D warnings
dotnet test <G1 创建的 C# 测试项目> -c Release
dotnet build src/Prism/Prism.csproj -c Release
```

## Review Gates

- 删除旧模块前必须先提交测试迁移和引用证据。
- STA worker 的取消/退出/崩溃边界通过评审后，G6 才能复用。
- 日志轮转配置必须与实际 crate/writer 能力一致。

## Rollback Points

测试迁移、Shell adapter、STA executor、typed protocol、排除、日志、安装脚本分别形成提交。协议回滚保留新字段 reader；权限边界不可回滚到 indexer 执行 Shell。安装脚本回滚只影响卸载清理，不影响已装用户。

## Legacy Index Test Migration

| Deleted `index.rs` test | Current production-path coverage |
| --- | --- |
| `walkdir_builds_nonempty_index` | `indexer_runtime::first_merge_makes_the_index_searchable_while_still_building` |
| `search_substring_match` | `hierarchy::search_is_case_insensitive_name_only_empty_safe_and_bounded` |
| `search_case_insensitive` | `hierarchy::search_is_case_insensitive_name_only_empty_safe_and_bounded` |
| `search_empty_query_returns_empty` | `hierarchy::search_is_case_insensitive_name_only_empty_safe_and_bounded` |
| `search_respects_max` | `hierarchy::search_is_case_insensitive_name_only_empty_safe_and_bounded` |
| `search_matches_filename_not_parent_path` | `hierarchy::search_is_case_insensitive_name_only_empty_safe_and_bounded` |
| `parent_dir_is_interned_across_siblings` | Replaced layout: `node_slot_is_twelve_bytes` + parent-chain rename test |
| `same_filename_interned_across_dirs` | Replaced layout: compact node slot + v5 cache round-trip |
| `drive_root_parent_normalized` | `hierarchy::create_rename_move_delete_and_sequence_reuse` |
| `path_excludes_noise_dirs` | `hierarchy::unicode_and_exclusion_boundary_are_explicit` |
| `skipped_names_case_insensitive` | `hierarchy::windows_installer_is_excluded_by_hierarchy_not_a_fake_flat_name` |
| `cache_roundtrip` | `index_cache::v5_roundtrip_and_corruption_recovery` |
| `old_version_cache_rejected` | `index_cache::older_cache_versions_are_rejected` |
| `corrupt_cache_returns_none` | `index_cache::v5_roundtrip_and_corruption_recovery` |
| `entry_size_is_compact` | `hierarchy::node_slot_is_twelve_bytes` |
| `pool_smaller_than_full_paths` | Replaced layout: compact slots plus parent-chain path reconstruction tests |

## Acceptance Evidence

- 2026-08-01 在 Windows 11 验收机使用 `dist/PrismSetup-1.0.0.exe` 静默安装成功，三个 Release 二进制部署到已有自定义安装目录，`PrismIndexer` 以 LocalSystem 运行并生成 `%ProgramData%\Prism\index-v5.bin` 与 `indexer.jsonl`。
- 同日静默卸载返回码为 0；`sc.exe query PrismIndexer` 返回 1060，`%ProgramData%\Prism` 不存在，安装目录中当前 Prism 二进制和卸载器已删除。目录仅留存安装前已存在的 `prism-indexer-service.g0-backup-20260728.exe`。
- 全量门禁通过：Rust 91 tests，C# 14 tests，Clippy `-D warnings`，WPF Release build 0 warnings / 0 errors，以及 `git diff --check`。
