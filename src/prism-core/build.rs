// 生成构建指纹，供 hello 握手回传。
//
// 存在的理由：`VERSION` 取自 Cargo.toml，改代码不会变，所以无法回答
// 「现在跑的这个进程，是不是我刚编出来的那份」。G4 排查时就因此绕了很久：
// 前端连的是 target\debug 里一个陈旧的 broker，而所有改动都编到了 release。
//
// 指纹取源码树的最新 mtime，而不是编译时刻——同一份源码重复编译得到同样的
// 指纹，便于判断「装上去的和源码是否一致」。
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

/// 目录树里最新的修改时间（Unix 秒）。读不到的条目直接跳过：
/// 指纹只用于对比，宁可粗糙也不要让构建失败。
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
