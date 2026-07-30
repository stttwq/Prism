//! Versioned, read-only protocol exposed by the privileged indexer service.

use serde::{Deserialize, Serialize};

use crate::hierarchy::MatchMetadata;

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
    },
    WaitGeneration {
        after: u64,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
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
    },
    Generation {
        generation: u64,
    },
    Error {
        message: String,
    },
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexerItem {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_metadata: Option<MatchMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchFilter {
    pub field: String,
    pub value: String,
}

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
    }
    Ok(())
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
            serde_json::from_str(r#"{"type":"hello","protocol":1}"#).unwrap();
        assert!(matches!(request, IndexerRequest::Hello { protocol: 1 }));
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
}
