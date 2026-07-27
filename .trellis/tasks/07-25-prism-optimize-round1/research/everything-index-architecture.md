# Everything 式实时索引调研（2026-07-26）

## 公开资料结论

- Everything 官方说明：本地固定 NTFS 卷自动纳入索引，运行时数据库驻留内存，名称、路径和已选属性实时维护；数据库通常在退出时保存。
- Everything 官方说明：NTFS 使用 USN Change Journal 建立并监控变更；Everything Service 允许普通用户界面通过特权服务索引和监控系统 NTFS 卷。
- Listary 官方只确认本地 NTFS 自动实时索引；未公开内部节点布局、位点协议或权限进程边界。
- 来源：
  - https://www.voidtools.com/support/everything/indexes/
  - https://www.voidtools.com/support/everything/options/#ntfs
  - https://help.listary.com/options-index

## 当前仓库约束

- `prism-core.exe` 同时负责文件索引、用户会话动作、网页配置和应用搜索，不能整体提升为 LocalSystem；否则剪贴板、ShellExecute、用户配置都会落入错误会话。
- WPF 通过 `PipeClient` 按需启动 `prism-core.exe`；当前 Inno 安装器只部署两个 exe，应用 manifest 为 `asInvoker`。
- 当前 v3 索引 616,223 条：缓存 18.33MB，反序列化后约 19.44MB；前后端空闲私有工作集合计约 38MB，硬门槛 100MB。
- 当前 `IndexEntry` 保存预拼父目录字符串，不保存 FRN。目录改名会使整棵子树路径失效，这一布局不能达到 Everything 式目录实时更新。

## 已选架构

1. 拆出 `prism-indexer-service.exe`，由 LocalSystem 运行，只拥有 NTFS 枚举、USN 监听、文件名搜索和机器级索引缓存。
2. `prism-core.exe` 保持普通权限，作为用户 broker：保留应用、网页、动作和现有前端协议，把文件搜索转发给索引服务后合并结果。
3. 每卷使用紧凑层级节点表：MFT record number 直接索引 `NodeSlot`，节点保存 parent record、name offset、FRN sequence 与 flags。路径只在返回命中项时沿父链构造。
4. 安装器以管理员权限安装/升级服务；日常 UI 与 broker 均不提权。服务缺失时只降级应用/网页搜索并报告索引不可用，不启动每 60 秒全盘扫描。
5. 首建在枚举前记录 USN 位点，MFT 枚举后重放构建窗口再发布；运行时 USN 批量延迟目标 50ms。缓存保存一致快照和位点，重启从位点追平。
6. 健康状态不做周期性磁盘重扫；缓存仅在首建完成、优雅停止、每 60 分钟或累计 100,000 个事件时检查点，先到为准。

## 容量估算

- `NodeSlot` 目标 12 bytes；以当前 616,223 条估算约 7.1MB，与现有条目表相当。
- 新池只保存节点名称，不再保存每条完整父目录串，预计不高于当前 12.39MB 字符串池。
- 索引从 broker 移到服务，避免双份常驻；目标仍为 UI ≤30MB、两个 Rust 进程合计 ≤70MB、三进程合计 ≤100MB。
- MFT record number 超过 `u32` 或槽表稀疏率异常时必须拒绝紧凑布局并记录不支持，不允许静默截断 FRN。

## 明确限制

- 本轮只保证本地固定 NTFS；ReFS、FAT/exFAT、网络盘、内容索引不在范围内。
- 硬链接完整语义不在本轮保证范围；不跟随 junction。
- 服务提供机器级文件名索引；本轮不按 Windows 文件 ACL 过滤路径名称，定位为单用户工作站产品。
- 高噪声排除目录跨边界移动是唯一允许触发自愈重建的普通目录场景；一般目录改名/移动必须纯增量。
