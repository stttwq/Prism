//! Versioned, read-only protocol exposed by the privileged indexer service.

use serde::{Deserialize, Serialize};

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
}
