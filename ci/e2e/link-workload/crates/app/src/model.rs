use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Artifact {
    pub id: Uuid,
    pub path: String,
    pub kind: String,
    pub bytes: Vec<u8>,
    pub dependencies: Vec<String>,
    pub labels: IndexMap<String, String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct Policy {
    pub include: Vec<String>,
    pub deny: Vec<String>,
    pub max_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct AuditResult {
    pub fingerprint: String,
    pub accepted: usize,
    pub rejected: usize,
    pub graph_edges: usize,
    pub sqlite_rows: usize,
    pub compressed_bytes: usize,
    pub rendered_report: String,
}
