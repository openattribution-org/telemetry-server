use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::EventRow;

// ---------------------------------------------------------------------------
// API input
// ---------------------------------------------------------------------------

fn default_initiator_type() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCreateRequest {
    #[serde(default = "default_initiator_type")]
    pub initiator_type: String,
    pub initiator: Option<serde_json::Value>,
    /// Immediate parent session that delegated work to this session
    /// (spec 5.1). Optional; consumers never infer that an unlinked
    /// session had no parent.
    pub parent_session_id: Option<Uuid>,
    pub content_scope: Option<String>,
    pub manifest_ref: Option<String>,
    pub conformance_level: Option<String>,
    pub agent_id: Option<String>,
    pub external_session_id: Option<String>,
    #[serde(default)]
    pub user_context: serde_json::Value,
    #[serde(default)]
    pub prior_session_ids: Vec<String>,
    // SPUR extensions
    pub platform_id: Option<String>,
    pub client_type: Option<String>,
    pub client_info: Option<serde_json::Value>,
    /// Emitter-supplied timestamps, preserved on bulk session-document
    /// ingest. Defaults to NOW() when absent (the /sessions/start flow).
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionEndRequest {
    pub session_id: String,
    pub outcome: SessionOutcome,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionOutcome {
    #[serde(rename = "type")]
    pub outcome_type: String,
    #[serde(default)]
    pub value_amount: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default)]
    pub products: Vec<Uuid>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

fn default_currency() -> String {
    "USD".to_string()
}

/// Complete session for bulk upload (matches SDK `TelemetrySession` and the
/// spec session-document format)
#[derive(Debug, Clone, Deserialize)]
pub struct BulkSessionRequest {
    /// Spec document discriminator. When present it must be "session";
    /// absent is treated as a session (spec 7.1).
    pub document_type: Option<String>,
    pub schema_version: Option<String>,
    pub session_id: Uuid,
    /// Immediate parent session that delegated work to this session
    /// (spec 5.1).
    pub parent_session_id: Option<Uuid>,
    #[serde(default = "default_initiator_type")]
    pub initiator_type: String,
    pub initiator: Option<serde_json::Value>,
    pub agent_id: Option<String>,
    pub content_scope: Option<String>,
    pub manifest_ref: Option<String>,
    pub conformance_level: Option<String>,
    /// Emitter's own correlation id. When absent, the presented
    /// `session_id` is stored as the external id.
    pub external_session_id: Option<String>,
    #[serde(default)]
    pub prior_session_ids: Vec<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub user_context: serde_json::Value,
    #[serde(default)]
    pub events: Vec<super::event::TelemetryEventInput>,
    pub outcome: Option<SessionOutcome>,
    // SPUR extensions
    pub platform_id: Option<String>,
    pub client_type: Option<String>,
    pub client_info: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Database row
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SessionRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub parent_session_id: Option<Uuid>,
    pub initiator_type: String,
    pub initiator: Option<serde_json::Value>,
    pub content_scope: Option<String>,
    pub manifest_ref: Option<String>,
    pub conformance_level: Option<String>,
    pub config_snapshot_hash: Option<String>,
    pub agent_id: Option<String>,
    pub external_session_id: Option<String>,
    pub prior_session_ids: Option<Vec<Uuid>>,
    pub user_context: serde_json::Value,
    pub platform_id: Option<String>,
    pub client_type: Option<String>,
    pub client_info: Option<serde_json::Value>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub outcome_type: Option<String>,
    pub outcome_value: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// API responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SessionStartResponse {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionEndResponse {
    pub status: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulkSessionResponse {
    pub session_id: String,
    pub events_created: usize,
    pub outcome_recorded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionWithEvents {
    #[serde(flatten)]
    pub session: SessionRow,
    pub events: Vec<EventRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SessionSummary {
    pub id: Uuid,
    pub content_scope: Option<String>,
    pub external_session_id: Option<String>,
    pub outcome_type: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}
