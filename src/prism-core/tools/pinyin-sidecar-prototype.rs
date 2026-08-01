use std::time::Instant;

use prism_core::pinyin_sidecar::prototype_names;
use walkdir::{DirEntry, WalkDir};

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: pinyin-sidecar-prototype <root> [root ...]");
        std::process::exit(2);
    }
    let scan_started = Instant::now();
    let mut names = Vec::new();
    let mut errors = 0u64;
    for root in &roots {
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(include_entry)
        {
            match entry {
                Ok(entry) if entry.depth() > 0 => {
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
                Ok(_) => {}
                Err(_) => errors = errors.saturating_add(1),
            }
        }
    }
    let report = match prototype_names(&names, 30) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("prototype failed: {error}");
            std::process::exit(1);
        }
    };
    let output = serde_json::json!({
        "schema_version": 1,
        "roots": roots,
        "scan_ms": scan_started.elapsed().as_secs_f64() * 1000.0,
        "scan_errors": errors,
        "report": report,
        "g0_name_candidates_reference": 660751,
        "memory_gate_bytes": 10 * 1024 * 1024,
        "max_8_p95_gate_ms": 100,
        "max_1000_p95_gate_ms": 300,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).unwrap_or_default()
    );
}

fn include_entry(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !matches!(
        name.as_ref(),
        "$Recycle.Bin"
            | "System Volume Information"
            | "WinSxS"
            | "node_modules"
            | ".git"
            | ".svn"
            | "__pycache__"
    )
}
