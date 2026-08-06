use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// API input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ClickTokenCreateRequest {
    pub session_id: Uuid,
    pub content_url: String,
    /// Optional token. If not provided, the server generates one.
    pub token: Option<String>,
}

// ---------------------------------------------------------------------------
// API responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ClickTokenCreateResponse {
    pub token: String,
    pub session_id: Uuid,
    pub content_url: String,
    pub expires_at: DateTime<Utc>,
}

/// One event in a click manifest: a `content_grounded`, `content_cited` or
/// `content_presented` event from the resolved session (spec section 7.1,
/// commerce profile 5.7.2), trimmed to the spec event fields. The event
/// `id` is included so a destination reporting a corroborating engagement
/// can reference the exact presentation occurrence as `presentation_id`
/// (spec 6.8). Legacy sessions can still surface stored v0.1
/// `content_displayed` events here.
#[derive(Debug, Clone, Serialize)]
pub struct ClickManifestEvent {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_element_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    pub data: serde_json::Value,
}

/// Click manifest returned when a landing page looks up a click token.
///
/// Identifies every source that informed the response that produced the
/// click: the session's grounded, cited and presented events, gated by
/// two-sided consent. The raw session UUID never crosses the click-out
/// boundary (spec 7.1, commerce profile 5.5): the token is the only
/// session reference a downstream party holds.
#[derive(Debug, Clone, Serialize)]
pub struct ClickManifestResponse {
    pub started_at: DateTime<Utc>,
    /// The specific URL that was clicked out to.
    pub click_content_url: String,
    /// The click manifest: grounded, cited and presented events for
    /// consenting content owners, in timestamp order.
    pub events: Vec<ClickManifestEvent>,
    /// Distinct content URLs by stage, derived from `events`. URLs from
    /// stored v0.1 `content_displayed` events count as presented.
    pub content_urls_grounded: Vec<String>,
    pub content_urls_cited: Vec<String>,
    pub content_urls_presented: Vec<String>,
}

/// A resolved click token: the owning session plus the disclosure-safe
/// manifest. `session_id` is for server-side use only (event binding,
/// ownership checks) and must not be serialised into any response that
/// crosses the click-out boundary.
#[derive(Debug, Clone)]
pub struct ResolvedClickToken {
    pub session_id: Uuid,
    pub manifest: ClickManifestResponse,
}

// ---------------------------------------------------------------------------
// Database row
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ClickTokenRow {
    pub id: Uuid,
    pub token: String,
    pub session_id: Uuid,
    pub content_url: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
