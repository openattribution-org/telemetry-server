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
    /// Session-level extension container (spec 5.1.3), carrying
    /// `access_context` and any namespaced members alongside it. Stored and
    /// served verbatim: consumers MUST tolerate unknown fields within it and
    /// unknown identifier schemes inside `access_context`, so nothing here
    /// is normalised or dropped.
    pub data: Option<serde_json::Value>,
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
    /// Top-level members this server does not define. The session root is
    /// not an extension point (spec 5.1.3), so nothing here is interpreted,
    /// but a conforming consumer MUST tolerate unknown fields without error
    /// (spec 5.7.4). Capturing them is what stops a member the specification
    /// adds later from being accepted and silently discarded.
    #[serde(flatten)]
    pub unrecognised_fields: serde_json::Map<String, serde_json::Value>,
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
    /// Session-level extension container (spec 5.1.3), carrying
    /// `access_context` and any namespaced members alongside it. Stored and
    /// served verbatim.
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub user_context: serde_json::Value,
    #[serde(default)]
    pub events: Vec<super::event::TelemetryEventInput>,
    pub outcome: Option<SessionOutcome>,
    // SPUR extensions
    pub platform_id: Option<String>,
    pub client_type: Option<String>,
    pub client_info: Option<serde_json::Value>,
    /// Top-level members this server does not define (spec 5.1.3, 5.7.4).
    /// Recorded rather than dropped; see `SessionCreateRequest`.
    #[serde(flatten)]
    pub unrecognised_fields: serde_json::Map<String, serde_json::Value>,
}

impl BulkSessionRequest {
    /// The session this document opens, as the bulk ingest path creates it.
    ///
    /// One mapping from the document format onto the stored session, rather
    /// than a hand-written copy in each handler: a member the format gains
    /// is either carried here or visibly absent here, never dropped in a
    /// place nobody looks.
    ///
    /// `conformance_level` passes through unnormalised — `create_session`
    /// normalises it (spec 5.7) — and `ended_at` is withheld when the
    /// document carries an outcome, because ending a session only updates a
    /// row whose `ended_at` is still NULL and stamping it here would drop
    /// the outcome. The caller hands that timestamp to `end_session`.
    pub fn session_create(&self) -> SessionCreateRequest {
        SessionCreateRequest {
            initiator_type: self.initiator_type.clone(),
            initiator: self.initiator.clone(),
            parent_session_id: self.parent_session_id,
            content_scope: self.content_scope.clone(),
            manifest_ref: self.manifest_ref.clone(),
            conformance_level: self.conformance_level.clone(),
            agent_id: self.agent_id.clone(),
            // The presented session id is the emitter's, not ours: it is
            // stored as the external id under a server-minted primary key.
            external_session_id: Some(
                self.external_session_id
                    .clone()
                    .unwrap_or_else(|| self.session_id.to_string()),
            ),
            data: self.data.clone(),
            user_context: self.user_context.clone(),
            prior_session_ids: self.prior_session_ids.iter().map(Uuid::to_string).collect(),
            platform_id: self.platform_id.clone(),
            client_type: self.client_type.clone(),
            client_info: self.client_info.clone(),
            started_at: self.started_at,
            ended_at: if self.outcome.is_some() {
                None
            } else {
                self.ended_at
            },
            unrecognised_fields: self.unrecognised_fields.clone(),
        }
    }
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
    /// The session-level `data` container as the emitter sent it
    /// (spec 5.1.3).
    pub session_data: Option<serde_json::Value>,
    /// Top-level members the specification does not define, kept so nothing
    /// a conformant document carries is lost without trace (spec 5.7.4).
    pub unrecognised_fields: Option<serde_json::Value>,
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
