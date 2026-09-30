//! Workspace catalog: incident, custom field, connector, customer event vocabularies.

use serde::{Deserialize, Serialize};

/// One of the 5 incident statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Investigating,
    Identified,
    FixInProgress,
    Monitoring,
    Resolved,
}

impl IncidentStatus {
    pub const ALL: [Self; 5] = [
        Self::Investigating,
        Self::Identified,
        Self::FixInProgress,
        Self::Monitoring,
        Self::Resolved,
    ];
}

/// One of the 4 incident severities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentSeverity {
    Sev1,
    Sev2,
    Sev3,
    Sev4,
}

impl IncidentSeverity {
    pub const ALL: [Self; 4] = [Self::Sev1, Self::Sev2, Self::Sev3, Self::Sev4];
}

/// One of the 3 incident sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentSource {
    Manual,
    Cluster,
    KnownIssue,
}

impl IncidentSource {
    pub const ALL: [Self; 3] = [Self::Manual, Self::Cluster, Self::KnownIssue];
}

/// One of the 6 custom field types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomFieldType {
    Text,
    LongText,
    Number,
    Date,
    Boolean,
    Select,
}

impl CustomFieldType {
    pub const ALL: [Self; 6] = [
        Self::Text,
        Self::LongText,
        Self::Number,
        Self::Date,
        Self::Boolean,
        Self::Select,
    ];
}

/// One of the 4 connector kinds (M10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    LocalJson,
    Csv,
    Sqlite,
    Http,
}

impl ConnectorKind {
    pub const ALL: [Self; 4] = [Self::LocalJson, Self::Csv, Self::Sqlite, Self::Http];
}

/// One of the 3 connector auth modes (M10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAuthMode {
    None,
    Header,
    Bearer,
}

impl ConnectorAuthMode {
    pub const ALL: [Self; 3] = [Self::None, Self::Header, Self::Bearer];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_spec() {
        assert_eq!(IncidentStatus::ALL.len(), 5);
        assert_eq!(IncidentSeverity::ALL.len(), 4);
        assert_eq!(IncidentSource::ALL.len(), 3);
        assert_eq!(CustomFieldType::ALL.len(), 6);
        assert_eq!(ConnectorKind::ALL.len(), 4);
        assert_eq!(ConnectorAuthMode::ALL.len(), 3);
    }
}
