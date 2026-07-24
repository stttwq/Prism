# Prism 执行计划（第一版，Rust 后端 + C# WPF 前端）

## 实施顺序（每步完成后可运行验证）

1. **双工程骨架**：`src/prism-core`（Rust）+ `src/Prism`（C# WPF）；前端能拉起后端并通过命名管道收发一条测试消息。
2. **全局快捷键**：双击 Ctrl 呼出 / Esc 隐藏；快捷键读取自设置文件。
3. **后端文件索引**：目录遍历版全盘索引 + 磁盘缓存；再加 MFT/USN 加速与增量更新（带降级）。实测索引内存。
4. **搜索链路**：前端输入 → 后端即时搜索 → 结果列表；回车打开、打开所在文件夹。
5. **程序启动**：开始菜单扫描 + 中文名匹配，结果置顶。
6. **网页快捷搜索**：预设 g/b/bi + 设置自定义添加。
7. **托盘与自启**：托盘图标（设置/退出）、注册表自启开关。
8. **设置界面**：中文设置页（快捷键、网站关键词、自启）。
9. **界面精修**：圆角、阴影、亚克力质感、动画、图标、深浅色主题——按 design.md 视觉规格逐项做，交用户过目确认"够好看"。
10. **内存验收**：任务管理器实测前后端合计 ≤ 100MB。
11. **安装包**：Inno Setup 中文安装向导（选目录/快捷方式/自启勾选）；实现"安装目录\data 优先、不可写退回用户数据夹"的数据目录策略；在中文目录和 Program Files 各装一遍做验收；写中文使用说明。

## 验证命令

- 后端：`cargo build --manifest-path src/prism-core/Cargo.toml`、`cargo test ...`
- 前端：`dotnet build src/Prism`、`dotnet run --project src/Prism`
- 发布：`cargo build --release` + `dotnet publish src/Prism -c Release -r win-x64 /p:PublishSingleFile=true`
- 每步按 prd.md 验收标准手动验证对应条目；第 3、10 步用任务管理器记录内存数值。

## 风险点与回滚

- 键盘钩子、MFT 读取、命名管道通信是三处最易出问题的地方，各自独立模块，可单独降级（钩子失败→RegisterHotKey；MFT 失败→目录遍历；管道断开→前端自动重启后端）。
- 内存超标风险在第 3 步就实测拦截，不留到最后。
- 索引/设置均可再生，删除即重建，无数据丢失风险。

## task.py start 前检查

- prd.md / design.md / implement.md 齐备（inline 工作流，无 implement.jsonl 门槛）。
- 用户已明确批准最终规划摘要。
