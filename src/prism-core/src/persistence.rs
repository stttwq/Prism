//! Version contracts shared by user-data persistence features.

use serde::{Deserialize, Serialize};

pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const HISTORY_SCHEMA_VERSION: u32 = 1;
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
    pub entries: Vec<serde_json::Value>,
}

impl VersionedData for HistoryData {
    const SCHEMA_VERSION: u32 = HISTORY_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 10_000 {
            return Err("history contains more than 10000 entries".into());
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
            entries: vec![serde_json::Value::Null; 10_001],
        };
        assert!(VersionedEnvelope::new(data).is_err());
    }
}
