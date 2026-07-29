use prism_core::hierarchy::{IndexState, FLAG_EXCLUDED, FLAG_PRESENT};
use std::fs::{self, OpenOptions};
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const COMPILE_TIME_OPT_LEVEL_OVERRIDE: Option<&str> =
    option_env!("CARGO_PROFILE_RELEASE_OPT_LEVEL");

struct Arguments {
    cache_directory: PathBuf,
    output_directory: PathBuf,
    query: String,
    iterations: usize,
    run_id: String,
    opt_level_label: String,
}

#[derive(Debug, Clone, Copy)]
struct ScanCounts {
    scanned_nodes: usize,
    name_candidates: usize,
    matches: usize,
}

fn main() -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err("scan-floor formal runs require a Release build".into());
    }
    let args = parse_arguments(std::env::args().skip(1))?;
    let opt_level_evidence = validate_opt_level_label(&args.opt_level_label)?;
    validate_output_directory(&args.output_directory, &args.cache_directory)?;
    fs::create_dir_all(&args.output_directory)
        .map_err(|error| format!("create output directory: {error}"))?;
    let raw_path = args.output_directory.join("scan-floor-samples.jsonl");
    let summary_path = args.output_directory.join("scan-floor-summary.json");
    if raw_path.exists() || summary_path.exists() {
        return Err("refusing to overwrite an existing scan-floor artifact".into());
    }

    let state = prism_core::index_cache::load(&args.cache_directory)?;
    let query = args.query.to_lowercase();
    let mut elapsed = Vec::with_capacity(args.iterations);
    let mut expected_counts = None;
    let mut raw = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&raw_path)
        .map_err(|error| format!("create {}: {error}", raw_path.display()))?;

    for iteration in 1..=args.iterations {
        let started = Instant::now();
        let counts = scan_all_nodes(black_box(&state), black_box(&query));
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        if let Some(expected) = expected_counts {
            if expected != (counts.scanned_nodes, counts.name_candidates, counts.matches) {
                return Err("scan counts changed within one immutable cache run".into());
            }
        } else {
            expected_counts = Some((counts.scanned_nodes, counts.name_candidates, counts.matches));
        }
        elapsed.push(elapsed_ms);
        let record = serde_json::json!({
            "schema_version": 1,
            "run_id": args.run_id,
            "iteration": iteration,
            "elapsed_ms": round_millis(elapsed_ms),
            "scanned_nodes": counts.scanned_nodes,
            "name_candidates": counts.name_candidates,
            "matches": counts.matches,
            "path_constructions": 0,
            "early_termination": false,
            "opt_level_label": args.opt_level_label,
            "build_mode": "release",
            "opt_level_evidence": opt_level_evidence,
            "query": args.query,
            "g1_decision_input_only": true
        });
        writeln!(raw, "{record}").map_err(|error| format!("write raw sample: {error}"))?;
    }
    raw.sync_all()
        .map_err(|error| format!("sync raw samples: {error}"))?;

    let counts = expected_counts.expect("iterations are validated as non-zero");
    let summary = serde_json::json!({
        "schema_version": 1,
        "run_id": args.run_id,
        "sample_count": elapsed.len(),
        "percentile_method": "nearest_rank",
        "p50_ms": round_millis(nearest_rank(&elapsed, 0.50)),
        "p95_ms": round_millis(nearest_rank(&elapsed, 0.95)),
        "max_ms": round_millis(elapsed.iter().copied().fold(0.0, f64::max)),
        "scanned_nodes": counts.0,
        "name_candidates": counts.1,
        "matches": counts.2,
        "path_constructions": 0,
        "early_termination": false,
        "generation": state.generation,
        "volume_count": state.volumes.len(),
        "reported_index_memory_bytes": state.memory_bytes(),
        "opt_level_label": args.opt_level_label,
        "build_mode": "release",
        "opt_level_evidence": opt_level_evidence,
        "g1_decision_input_only": true
    });
    write_summary_atomically(&summary_path, &summary)?;
    println!("scan floor complete: {}", summary_path.display());
    Ok(())
}

fn parse_arguments(mut values: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut cache_directory = None;
    let mut output_directory = None;
    let mut query = None;
    let mut iterations = 30usize;
    let mut run_id = None;
    let mut opt_level_label = None;
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--cache-directory" => cache_directory = Some(PathBuf::from(value)),
            "--output-directory" => output_directory = Some(PathBuf::from(value)),
            "--query" => query = Some(value),
            "--iterations" => {
                iterations = value.parse().map_err(|_| "iterations must be an integer")?
            }
            "--run-id" => run_id = Some(value),
            "--opt-level-label" => opt_level_label = Some(value),
            _ => return Err(format!("unknown argument: {flag}")),
        }
    }
    if iterations < 30 {
        return Err("iterations must be at least 30".into());
    }
    let query = query.ok_or("--query is required")?;
    if query.trim().is_empty() {
        return Err("--query must not be empty".into());
    }
    let opt_level_label = opt_level_label.ok_or("--opt-level-label is required")?;
    if opt_level_label != "z" && opt_level_label != "3" {
        return Err("--opt-level-label must be z or 3".into());
    }
    Ok(Arguments {
        cache_directory: cache_directory.ok_or("--cache-directory is required")?,
        output_directory: output_directory.ok_or("--output-directory is required")?,
        query,
        iterations,
        run_id: run_id.unwrap_or_else(default_run_id),
        opt_level_label,
    })
}

fn validate_output_directory(output: &Path, cache: &Path) -> Result<(), String> {
    let output = canonicalize_allow_missing(&absolute_path(output)?)?;
    let cache = fs::canonicalize(cache)
        .map_err(|error| format!("canonicalize cache directory: {error}"))?;
    if same_or_descendant(&output, &cache) {
        return Err("output directory must not be inside the product cache directory".into());
    }
    Ok(())
}

fn validate_opt_level_label(label: &str) -> Result<String, String> {
    match COMPILE_TIME_OPT_LEVEL_OVERRIDE {
        Some(value) if value == label => Ok(format!("CARGO_PROFILE_RELEASE_OPT_LEVEL={value}")),
        Some(value) => Err(format!(
            "--opt-level-label {label} does not match compile-time CARGO_PROFILE_RELEASE_OPT_LEVEL={value}"
        )),
        None if label == "z" => Ok("committed scan-floor release profile opt-level=z".into()),
        None => Err(
            "--opt-level-label 3 requires building with CARGO_PROFILE_RELEASE_OPT_LEVEL=3"
                .into(),
        ),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("resolve path: {error}"))
    }
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return fs::canonicalize(path).map_err(|error| format!("canonicalize path: {error}"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| "output directory must have a final path component".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "output directory must have a parent".to_string())?;
    Ok(canonicalize_allow_missing(parent)?.join(name))
}

fn same_or_descendant(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy().to_lowercase();
        let root = root.to_string_lossy().to_lowercase();
        path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|suffix| suffix.starts_with(['\\', '/']))
    }
    #[cfg(not(windows))]
    {
        path == root || path.starts_with(root)
    }
}

fn write_summary_atomically(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("create {}: {error}", temporary.display()))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| format!("publish {}: {error}", path.display()))
}

fn scan_all_nodes(state: &IndexState, query_lower: &str) -> ScanCounts {
    let mut counts = ScanCounts {
        scanned_nodes: 0,
        name_candidates: 0,
        matches: 0,
    };
    for volume in &state.volumes {
        for slot in &volume.nodes {
            counts.scanned_nodes += 1;
            if slot.flags & FLAG_PRESENT == 0
                || slot.flags & FLAG_EXCLUDED != 0
                || slot.name_off == u32::MAX
            {
                continue;
            }
            let Some(name) = name_at(&volume.names, slot.name_off as usize) else {
                continue;
            };
            counts.name_candidates += 1;
            if contains_case_insensitive(name, query_lower) {
                counts.matches += 1;
            }
        }
    }
    black_box(counts)
}

fn name_at(pool: &[u8], offset: usize) -> Option<&str> {
    let tail = pool.get(offset..)?;
    let end = tail.iter().position(|byte| *byte == 0)?;
    std::str::from_utf8(&tail[..end]).ok()
}

// Mirrors the product matcher without widening its API for a benchmark-only
// consumer. In particular, ASCII names must keep the allocation-free path.
fn contains_case_insensitive(name: &str, query_lower: &str) -> bool {
    if name.is_ascii() && query_lower.is_ascii() {
        name.as_bytes().windows(query_lower.len()).any(|window| {
            window
                .iter()
                .zip(query_lower.as_bytes())
                .all(|(left, right)| left.to_ascii_lowercase() == *right)
        })
    } else {
        name.to_lowercase().contains(query_lower)
    }
}

fn nearest_rank(values: &[f64], percentile: f64) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let rank = ((percentile * ordered.len() as f64).ceil() as usize).max(1);
    ordered[rank - 1]
}

fn round_millis(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

fn default_run_id() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("g0-scan-{seconds}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_keeps_tail_samples() {
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(nearest_rank(&values, 0.50), 10.0);
        assert_eq!(nearest_rank(&values, 0.95), 19.0);
    }

    #[test]
    fn name_pool_offsets_are_bounded() {
        let pool = b"alpha\0beta\0";
        assert_eq!(name_at(pool, 0), Some("alpha"));
        assert_eq!(name_at(pool, 6), Some("beta"));
        assert_eq!(name_at(pool, 99), None);
    }

    #[test]
    fn matching_tracks_product_ascii_and_unicode_paths() {
        assert!(contains_case_insensitive("Prism.EXE", "prism"));
        assert!(contains_case_insensitive("工具箱", "工具"));
        assert!(!contains_case_insensitive("Listary", "prism"));
    }

    #[test]
    fn descendant_check_is_component_aware() {
        let root = Path::new(r"C:\ProgramData\Prism");
        assert!(same_or_descendant(
            Path::new(r"C:\ProgramData\Prism\bench"),
            root
        ));
        assert!(!same_or_descendant(
            Path::new(r"C:\ProgramData\Prism-bench"),
            root
        ));
    }

    #[test]
    fn opt_level_label_requires_matching_build_evidence() {
        let expected = COMPILE_TIME_OPT_LEVEL_OVERRIDE.unwrap_or("z");
        assert!(validate_opt_level_label(expected).is_ok());
        let mismatch = if expected == "z" { "3" } else { "z" };
        assert!(validate_opt_level_label(mismatch).is_err());
    }
}
