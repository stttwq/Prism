//! Versioned, read-only protocol exposed by the privileged indexer service.

use serde::{Deserialize, Serialize};

use crate::hierarchy::MatchMetadata;
use crate::root_scope::{RootRejection, MAX_ROOT_PATH_BYTES};

pub const MAX_SEARCH_RESULTS: usize = 1000;
pub const MAX_FILTERS: usize = 32;
pub const MAX_FILTER_FIELD_BYTES: usize = 32;
pub const MAX_FILTER_VALUE_BYTES: usize = 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IndexerRequest {
    Hello {
        protocol: u32,
    },
    Status,
    Search {
        query: String,
        #[serde(default = "default_max")]
        max: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filters: Option<Vec<SearchFilter>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pinyin_enabled: Option<bool>,
        /// Optional current-directory scope. Absent (older clients) or blank means an
        /// unrestricted global search, so a reader that ignores the field stays correct.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
    },
    WaitGeneration {
        after: u64,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    /// Management command: flip the pinyin enabled flag and load/release the
    /// sidecar. Search requests are read-only w.r.t. this flag (audit M4+M6):
    /// the `pinyin_enabled` field on `Search` is still accepted (forward
    /// compatibility) but no longer mutates global state.
    SetPinyinEnabled {
        enabled: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IndexerResponse {
    Hello {
        protocol: u32,
    },
    Status(IndexerStatus),
    Results {
        generation: u64,
        items: Vec<IndexerItem>,
        #[serde(default)]
        is_truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        matched_count: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scanned_nodes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name_candidates: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entered_top_k: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path_constructions: Option<u64>,
        /// 内存收口 II：本次搜索消耗的现场 stat 次数（诊断，供调 STAT_CALL_BUDGET）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stat_calls: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pinyin_status: Option<PinyinStatus>,
    },
    Generation {
        generation: u64,
    },
    /// A requested root could not be used. Only ever sent in reply to a request that
    /// carried `root`, so clients that never send one cannot receive an unknown variant.
    RootUnavailable {
        reason: RootRejection,
        message: String,
    },
    Error {
        message: String,
    },
}

/// Index availability snapshot.
///
/// `ready` and `building` are independent: during a first build they are both true
/// once at least one volume has been published, meaning "searchable, still filling in".
/// A partially built index is **not** expressed through `Results::is_truncated`, which
/// only ever means "more matches existed than `max` allowed".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerStatus {
    pub ready: bool,
    pub building: bool,
    pub degraded: bool,
    pub generation: u64,
    pub volumes: usize,
    pub memory_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// First-build progress. Absent when no build is running, and absent field-by-field
    /// when a figure is unknown, so older readers keep their previous behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_progress: Option<BuildProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinyin_status: Option<PinyinStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PinyinStatus {
    Disabled,
    Ready,
    Building,
    Missing,
    Corrupt,
    VersionMismatch,
    IndexMismatch,
}

/// Per-volume first-build progress. Every field beyond the volume counts is optional:
/// a first install has no previous cache to estimate a record total from.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildProgress {
    pub volumes_total: usize,
    pub volumes_done: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_volume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_scanned: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records_estimate: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerItem {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_metadata: Option<MatchMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_spans: Option<Vec<i32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchFilter {
    pub field: String,
    pub value: String,
}

/// Recognised filter field names on the `filters` channel.
const FIELD_EXCLUDE_PATH: &str = "exclude_path";
const FIELD_EXT: &str = "ext";
const FIELD_PATH: &str = "path";
/// G7c：stat 类过滤字段——判定在索引器扫描内用每记录元数据执行。
const FIELD_SIZE: &str = "size";
const FIELD_MODIFIED: &str = "dm";
const FIELD_CREATED: &str = "dc";
const FIELD_FILE: &str = "file";
const FIELD_FOLDER: &str = "folder";

/// H1（复审 2026-08-21）：查询长度上限。索引器管道对 Authenticated Users
/// 开放，无界查询即使经 NameTerms 去重/封顶，归一化与分词本身也是每击键
/// 的线性成本——4KB 覆盖真实输入（文件名搜索的最长意图），超出直接拒绝。
pub const MAX_QUERY_BYTES: usize = 4 * 1024;

pub fn validate_search_request(max: usize, filters: Option<&[SearchFilter]>) -> Result<(), String> {
    if max == 0 || max > MAX_SEARCH_RESULTS {
        return Err(format!("max must be between 1 and {MAX_SEARCH_RESULTS}"));
    }
    let filters = filters.unwrap_or_default();
    if filters.len() > MAX_FILTERS {
        return Err(format!("filters may contain at most {MAX_FILTERS} entries"));
    }
    for filter in filters {
        if filter.field.is_empty() || filter.field.len() > MAX_FILTER_FIELD_BYTES {
            return Err(format!(
                "filter field must contain 1 to {MAX_FILTER_FIELD_BYTES} bytes"
            ));
        }
        if filter.value.len() > MAX_FILTER_VALUE_BYTES {
            return Err(format!(
                "filter value may contain at most {MAX_FILTER_VALUE_BYTES} bytes"
            ));
        }
        match filter.field.as_str() {
            FIELD_EXCLUDE_PATH => {
                if filter.value.trim().is_empty()
                    || filter.value.contains('\0')
                    || filter.value.chars().any(char::is_control)
                    || !std::path::Path::new(filter.value.trim()).is_absolute()
                {
                    return Err("exclude_path filters require an absolute path".into());
                }
            }
            FIELD_EXT | FIELD_PATH => {
                if filter.value.trim().is_empty()
                    || filter.value.contains('\0')
                    || filter.value.chars().any(char::is_control)
                {
                    return Err(format!(
                        "{} filter value must be a non-empty, control-free string",
                        filter.field
                    ));
                }
            }
            // G7c：stat 类过滤下沉到索引器扫描内——值格式按与 broker parse_query
            // 同一解析器校验（解析失败的请求整体拒绝，不再静默降级）。
            FIELD_SIZE => {
                if !crate::filters::is_valid_size_value(filter.value.trim()) {
                    return Err(format!("size filter value is invalid: {}", filter.value));
                }
            }
            FIELD_MODIFIED | FIELD_CREATED => {
                if !crate::filters::is_valid_date_value(
                    filter.value.trim(),
                    &crate::filters::local_now(),
                ) {
                    return Err(format!(
                        "{} filter value is invalid: {}",
                        filter.field, filter.value
                    ));
                }
            }
            FIELD_FILE | FIELD_FOLDER => {
                if !filter.value.is_empty() {
                    return Err(format!("{} filter takes no value", filter.field));
                }
            }
            _ => {
                return Err(format!("unsupported filter field: {}", filter.field));
            }
        }
    }
    Ok(())
}

/// Bounds a requested root before any index work happens.
///
/// A missing *or* blank root means "search globally": a client that clears its scope must
/// not get an error, and an older client that never sends the field behaves as before.
pub fn requested_root(root: Option<&str>) -> Result<Option<&str>, RootRejection> {
    let Some(value) = root.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > MAX_ROOT_PATH_BYTES {
        return Err(RootRejection::TooLong);
    }
    Ok(Some(value))
}

pub fn exclusion_paths(filters: Option<&[SearchFilter]>) -> Vec<String> {
    filters
        .unwrap_or_default()
        .iter()
        .filter(|filter| filter.field == FIELD_EXCLUDE_PATH)
        .map(|filter| {
            filter
                .value
                .trim()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_owned()
        })
        .collect()
}

/// Returns `ext:` filter values (G7). Each value is already normalized by the broker parser:
/// leading dot stripped, lowercased. Multiple values within one `ext:` token are split into
/// separate filters (OR semantics); multiple `ext:` tokens are AND-ed at the filter level but
/// collapse to a single OR set at execution time (any ext match passes).
pub fn ext_filters(filters: Option<&[SearchFilter]>) -> Vec<String> {
    filters
        .unwrap_or_default()
        .iter()
        .filter(|filter| filter.field == FIELD_EXT)
        // L4（复审 2026-08-21）：剥一个前导点并降幂——「已由 broker 归一化」的
        // 假设对直连索引器管道的客户端不成立，ext:.pdf 此前静默匹配不到任何东西。
        .map(|filter| {
            let trimmed = filter.value.trim().to_lowercase();
            trimmed.strip_prefix('.').unwrap_or(&trimmed).to_owned()
        })
        .collect()
}

/// Returns `path:` filter values (G7). Each value is a case-insensitive substring matched
/// against the candidate's full path. Multiple `path:` filters are AND-ed: a candidate's path
/// must contain every path substring.
pub fn path_filters(filters: Option<&[SearchFilter]>) -> Vec<String> {
    filters
        .unwrap_or_default()
        .iter()
        .filter(|filter| filter.field == FIELD_PATH)
        .map(|filter| filter.value.trim().to_owned())
        .collect()
}

fn default_max() -> usize {
    100
}

fn default_timeout_ms() -> u64 {
    30_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_write_command_is_rejected_by_decoder() {
        let request = serde_json::from_str::<IndexerRequest>(r#"{"type":"rebuild"}"#);
        assert!(request.is_err());
    }

    #[test]
    fn protocol_version_is_explicit() {
        let request: IndexerRequest =
            serde_json::from_str(r#"{"type":"hello","protocol":2}"#).unwrap();
        assert!(matches!(request, IndexerRequest::Hello { protocol: 2 }));
    }

    /// G7c：stat 类过滤字段进索引器通道，值格式与 broker parse_query 同解析器。
    #[test]
    fn g7c_stat_filter_fields_validate() {
        let ok = |field: &str, value: &str| {
            validate_search_request(
                10,
                Some(&[SearchFilter {
                    field: field.into(),
                    value: value.into(),
                }]),
            )
            .is_ok()
        };
        assert!(ok("size", ">10mb"));
        assert!(ok("size", "1mb..100mb"));
        assert!(ok("size", "large"));
        assert!(ok("dm", "today"));
        assert!(ok("dc", "2024"));
        assert!(ok("file", ""));
        assert!(ok("folder", ""));
        assert!(!ok("size", "abc"));
        assert!(!ok("dm", "2026/8/26"), "斜杠日期非法，须 20260826");
        assert!(!ok("folder", "x"), "旗标不取值");
    }

    #[test]
    fn missing_and_empty_filters_decode_to_equivalent_requests() {
        let missing: IndexerRequest =
            serde_json::from_str(r#"{"type":"search","query":"x","max":8}"#).unwrap();
        let empty: IndexerRequest =
            serde_json::from_str(r#"{"type":"search","query":"x","max":8,"filters":[]}"#).unwrap();
        assert!(matches!(
            missing,
            IndexerRequest::Search { filters: None, .. }
        ));
        assert!(matches!(
            empty,
            IndexerRequest::Search {
                filters: Some(values),
                ..
            } if values.is_empty()
        ));
    }

    #[test]
    fn status_without_build_progress_still_decodes() {
        let status: IndexerStatus = serde_json::from_str(
            r#"{"ready":true,"building":false,"degraded":false,"generation":7,"volumes":3,"memory_bytes":10}"#,
        )
        .unwrap();
        assert!(status.build_progress.is_none());
    }

    #[test]
    fn user_exclusions_are_bounded_absolute_paths() {
        let valid = [SearchFilter {
            field: "exclude_path".into(),
            value: r"C:\Users\me\build".into(),
        }];
        validate_search_request(8, Some(&valid)).unwrap();
        assert_eq!(exclusion_paths(Some(&valid)), vec![r"C:\Users\me\build"]);

        let unknown = [SearchFilter {
            field: "future".into(),
            value: "x".into(),
        }];
        assert!(validate_search_request(8, Some(&unknown)).is_err());
        let relative = [SearchFilter {
            field: "exclude_path".into(),
            value: "relative".into(),
        }];
        assert!(validate_search_request(8, Some(&relative)).is_err());
    }

    #[test]
    fn ext_and_path_filters_are_accepted_and_extracted() {
        let filters = [
            SearchFilter {
                field: "ext".into(),
                value: "pdf".into(),
            },
            SearchFilter {
                field: "ext".into(),
                value: "md".into(),
            },
            SearchFilter {
                field: "path".into(),
                value: r"Project Docs".into(),
            },
        ];
        validate_search_request(8, Some(&filters)).unwrap();
        assert_eq!(ext_filters(Some(&filters)), vec!["pdf", "md"]);
        assert_eq!(path_filters(Some(&filters)), vec!["Project Docs"]);
    }

    #[test]
    fn empty_ext_or_path_value_is_rejected() {
        let empty_ext = [SearchFilter {
            field: "ext".into(),
            value: "  ".into(),
        }];
        assert!(validate_search_request(8, Some(&empty_ext)).is_err());

        let empty_path = [SearchFilter {
            field: "path".into(),
            value: "".into(),
        }];
        assert!(validate_search_request(8, Some(&empty_path)).is_err());
    }

    #[test]
    fn unknown_filter_field_is_still_rejected() {
        let unknown = [SearchFilter {
            field: "weird_field".into(),
            value: "100".into(),
        }];
        assert!(validate_search_request(8, Some(&unknown)).is_err());
    }

    #[test]
    fn absent_build_progress_is_not_serialized() {
        let status = IndexerStatus {
            ready: false,
            building: true,
            degraded: false,
            generation: 0,
            volumes: 0,
            memory_bytes: 0,
            message: None,
            build_progress: None,
            pinyin_status: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("build_progress"), "{json}");
    }

    #[test]
    fn partial_build_progress_round_trips_without_optional_figures() {
        let status = IndexerStatus {
            ready: true,
            building: true,
            degraded: false,
            generation: 2,
            volumes: 1,
            memory_bytes: 4,
            message: None,
            build_progress: Some(BuildProgress {
                volumes_total: 3,
                volumes_done: 1,
                current_volume: Some("D:\\".into()),
                records_scanned: Some(1_234),
                records_estimate: None,
            }),
            pinyin_status: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("records_estimate"), "{json}");
        let decoded: IndexerStatus = serde_json::from_str(&json).unwrap();
        let progress = decoded.build_progress.unwrap();
        assert_eq!(progress.volumes_done, 1);
        assert_eq!(progress.volumes_total, 3);
        assert_eq!(progress.current_volume.as_deref(), Some("D:\\"));
        assert_eq!(progress.records_scanned, Some(1_234));
        assert_eq!(progress.records_estimate, None);
        // ready && building is the partial-index combination, and it is not truncation.
        assert!(decoded.ready && decoded.building);
    }

    #[test]
    fn old_results_without_optional_g1_fields_still_decode() {
        let response: IndexerResponse =
            serde_json::from_str(r#"{"type":"results","generation":3,"items":[]}"#).unwrap();
        assert!(matches!(
            response,
            IndexerResponse::Results {
                generation: 3,
                is_truncated: false,
                matched_count: None,
                path_constructions: None,
                ..
            }
        ));
    }

    #[test]
    fn search_without_root_field_decodes_and_serializes_as_global() {
        let request: IndexerRequest =
            serde_json::from_str(r#"{"type":"search","query":"x","max":8}"#).unwrap();
        assert!(matches!(request, IndexerRequest::Search { root: None, .. }));
        // An old reader must not see a field it cannot interpret.
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("root"), "{json}");

        let scoped: IndexerRequest =
            serde_json::from_str(r#"{"type":"search","query":"x","max":8,"root":"C:\\Users\\me"}"#)
                .unwrap();
        assert!(matches!(
            scoped,
            IndexerRequest::Search { root: Some(ref value), .. } if value == r"C:\Users\me"
        ));
    }

    #[test]
    fn requested_root_treats_blank_as_global_and_bounds_length() {
        assert_eq!(requested_root(None).unwrap(), None);
        assert_eq!(requested_root(Some("   ")).unwrap(), None);
        assert_eq!(
            requested_root(Some("  C:\\dir  ")).unwrap(),
            Some(r"C:\dir")
        );
        let too_long = "C:\\".to_owned() + &"a".repeat(MAX_ROOT_PATH_BYTES);
        assert_eq!(
            requested_root(Some(&too_long)).unwrap_err(),
            RootRejection::TooLong
        );
    }

    #[test]
    fn root_unavailable_round_trips_with_a_machine_readable_reason() {
        let response = IndexerResponse::RootUnavailable {
            reason: RootRejection::NotFound,
            message: RootRejection::NotFound.message().to_owned(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains(r#""type":"root_unavailable""#), "{json}");
        assert!(json.contains(r#""reason":"not_found""#), "{json}");
        let decoded: IndexerResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            IndexerResponse::RootUnavailable {
                reason: RootRejection::NotFound,
                ..
            }
        ));
    }
}
