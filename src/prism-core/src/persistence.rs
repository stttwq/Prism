//! Version contracts shared by user-data persistence features.

use serde::{Deserialize, Serialize};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const HISTORY_SCHEMA_VERSION: u32 = 2;
pub const FAVICON_METADATA_SCHEMA_VERSION: u32 = 1;

pub trait VersionedData {
    const SCHEMA_VERSION: u32;
    fn validate(&self) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VersionedEnvelope<T> {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub data: T,
}

impl<T: VersionedData + Default> VersionedEnvelope<T> {
    pub fn new(data: T) -> Result<Self, String> {
        data.validate()?;
        Ok(Self {
            schema_version: T::SCHEMA_VERSION,
            data,
        })
    }

    pub fn into_compatible(self) -> Result<T, String> {
        if self.schema_version > T::SCHEMA_VERSION {
            return Err(format!(
                "schema version {} is newer than supported version {}",
                self.schema_version,
                T::SCHEMA_VERSION
            ));
        }
        self.data.validate()?;
        Ok(self.data)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryData {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

/// v2：记录某个规范化查询串选中过该 target（写入侧在 history.rs 里做键归一化）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryStat {
    pub query: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub last_used_utc: u64,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub kind: String,
    pub target: String,
    #[serde(default)]
    pub execute_count: u32,
    #[serde(default)]
    pub reveal_count: u32,
    #[serde(default)]
    pub destination_count: u32,
    #[serde(default)]
    pub last_used_utc: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub first_used_utc: u64,
    /// 衰减加权分 ×1000 定点存储：事件时更新、读取时按 last_utc 惰性衰减。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub frecency_milli: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queries: Vec<QueryStat>,
}

impl VersionedData for HistoryData {
    const SCHEMA_VERSION: u32 = HISTORY_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 5000 {
            return Err("history contains more than 5000 entries".into());
        }
        for entry in &self.entries {
            if !matches!(
                entry.kind.as_str(),
                "file" | "directory" | "application" | "window"
            ) {
                return Err("history contains an unsupported target kind".into());
            }
            if entry.target.is_empty()
                || entry.target.contains('\0')
                || entry.target.len() > 32 * 1024
            {
                return Err("history contains an invalid target".into());
            }
            if entry.queries.len() > 8 {
                return Err("history entry contains more than 8 query stats".into());
            }
            for stat in &entry.queries {
                if stat.query.is_empty() || stat.query.contains('\0') || stat.query.len() > 128 {
                    return Err("history entry contains an invalid query stat".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaviconMetadata {
    #[serde(default)]
    pub entries: Vec<serde_json::Value>,
}

impl VersionedData for FaviconMetadata {
    const SCHEMA_VERSION: u32 = FAVICON_METADATA_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 1_000 {
            return Err("favicon metadata contains more than 1000 entries".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_version_and_fields_use_safe_defaults() {
        let envelope: VersionedEnvelope<HistoryData> =
            serde_json::from_str(r#"{"data":{}}"#).unwrap();
        assert_eq!(envelope.schema_version, 0);
        assert!(envelope.into_compatible().unwrap().entries.is_empty());
    }

    #[test]
    fn future_schema_is_rejected() {
        let envelope: VersionedEnvelope<FaviconMetadata> =
            serde_json::from_str(r#"{"schema_version":999,"data":{"entries":[]}}"#).unwrap();
        assert!(envelope.into_compatible().is_err());
    }

    #[test]
    fn writer_validates_before_serializing_new_shape() {
        let data = HistoryData {
            entries: vec![HistoryEntry::default(); 5001],
        };
        assert!(VersionedEnvelope::new(data).is_err());
    }

    #[test]
    fn query_stats_are_bounded_and_nonempty() {
        let entry = |queries: Vec<QueryStat>| HistoryData {
            entries: vec![HistoryEntry {
                kind: "file".into(),
                target: r"C:\x".into(),
                queries,
                ..HistoryEntry::default()
            }],
        };
        let nine = (0..9)
            .map(|index| QueryStat {
                query: format!("q{index}"),
                count: 1,
                last_used_utc: 0,
            })
            .collect();
        assert!(VersionedEnvelope::new(entry(nine)).is_err());

        let empty = vec![QueryStat::default()];
        assert!(VersionedEnvelope::new(entry(empty)).is_err());

        let overlong = vec![QueryStat {
            query: "x".repeat(129),
            ..QueryStat::default()
        }];
        assert!(VersionedEnvelope::new(entry(overlong)).is_err());

        let valid = vec![QueryStat {
            query: "conf".into(),
            count: 2,
            last_used_utc: 42,
        }];
        assert!(VersionedEnvelope::new(entry(valid)).is_ok());
    }

    #[test]
    fn v1_entries_decode_into_the_v2_shape_with_defaults() {
        let envelope: VersionedEnvelope<HistoryData> = serde_json::from_str(
            r#"{"schema_version":1,"data":{"entries":[{"kind":"file","target":"C:\\a","execute_count":2,"last_used_utc":9}]}}"#,
        )
        .unwrap();
        let data = envelope.into_compatible().unwrap();
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].frecency_milli, 0);
        assert!(data.entries[0].queries.is_empty());
        assert_eq!(data.entries[0].first_used_utc, 0);
    }
}
