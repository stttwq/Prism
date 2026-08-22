// 生成构建指纹，供 hello 握手回传。
//
// 存在的理由：`VERSION` 取自 Cargo.toml，改代码不会变，所以无法回答
// 「现在跑的这个进程，是不是我刚编出来的那份」。G4 排查时就因此绕了很久：
// 前端连的是 target\debug 里一个陈旧的 broker，而所有改动都编到了 release。
//
// M20（全仓复审 2026-08-22）——两个诚实的限定，原先的注释说反了/说过头了：
// 1) 指纹取源码树的最新 mtime，**只在同一份签出内**重复编译才稳定；
//    `git clone` 会把所有文件 mtime 设为签出时刻，同一提交在两台机器上
//    出不同指纹。它是「同机对比」线索，不是跨机器的内容哈希。
// 2) 目前没有任何工具把它用进自动校验：prism-build.ps1 的 Invoke-Verify
//    只比磁盘文件 SHA-256。「运行中的进程是旧的（文件已换新）」这一 stamp
//    唯一能答的问题，现只能靠 hello 握手里回显的 build_id 人工比对。
use std::path::Path;
use std::time::UNIX_EPOCH;

fn main() {
    // 源码变了就重跑本脚本。
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let newest = newest_mtime(Path::new("src")).max(newest_mtime(Path::new("Cargo.toml")));
    println!("cargo:rustc-env=PRISM_BUILD_STAMP={newest}");

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=PRISM_BUILD_PROFILE={profile}");
}

/// 目录树里最新的修改时间（Unix 秒）。读不到的条目返回 0（跳过）：
/// 指纹只用于对比，宁可粗糙也不要让构建失败。注意 0 也是合法的
/// 极早期时间戳——错误路径与合法值不可区分，但这只影响「指纹恰好为 0」
/// 的对比歧义，不影响构建。
fn newest_mtime(path: &Path) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };

    let own = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if !meta.is_dir() {
        return own;
    }

    let Ok(entries) = std::fs::read_dir(path) else {
        return own;
    };

    entries
        .flatten()
        .map(|entry| newest_mtime(&entry.path()))
        .fold(own, u64::max)
}
