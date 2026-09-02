use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Publisher query responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct PublisherSummary {
    pub organization_id: Uuid,
    pub domains: Vec<String>,
    /// True when the organisation is a demo fixture: every event under it
    /// is fabricated, and no consumer may present it as real telemetry.
    pub synthetic: bool,
    pub total_events: i64,
    pub total_sessions: i64,
    pub events_by_type: Vec<EventTypeCount>,
    pub events_by_source: Vec<SourceRoleCount>,
    pub events_by_status: Vec<StatusCodeCount>,
    pub agents: Vec<AgentBreakdown>,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRoleCount {
    pub source_role: Option<String>,
    pub count: i64,
    pub sessions: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusCodeCount {
    /// HTTP status observed at the edge (event_data.response_status). None
    /// bucket counts events whose source recorded no status — only the edge
    /// enrichment profile stamps one, so self-reported events land there.
    pub status: Option<i32>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentBreakdown {
    pub platform_id: Option<String>,
    pub agent_id: Option<String>,
    /// Bot name from edge detection (event_data.bot_name), e.g. "ClaudeBot".
    /// Set when agent_id is null and the edge worker identified a known bot UA.
    pub bot_name: Option<String>,
    /// Access purpose as classified by the reporting party (spec 6.2):
    /// "training" | "inference" | "search" | "advertising", open enum.
    /// Read from event_data.purpose, falling back to the v0.1 name
    /// bot_category (spec 12.1).
    pub purpose: Option<String>,
    /// Deprecated alias for `purpose` (the v0.1 field name). Carries the
    /// same value during the transition so existing dashboard readers keep
    /// working; new readers use `purpose`.
    pub bot_category: Option<String>,
    pub event_count: i64,
    pub session_count: i64,
    pub by_source: Vec<SourceRoleCount>,
    pub by_event_type: Vec<EventTypeCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventTypeCount {
    pub event_type: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublisherEvent {
    pub event_id: Uuid,
    pub session_id: Option<Uuid>,
    pub event_type: String,
    pub source_role: Option<String>,
    pub content_telemetry_id: Option<Uuid>,
    pub content_url: Option<String>,
    pub event_timestamp: DateTime<Utc>,
    pub event_data: serde_json::Value,
    pub platform_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublisherUrlMetric {
    pub content_url: String,
    pub total_events: i64,
    pub unique_sessions: i64,
    pub event_types: Vec<EventTypeCount>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DailyMetricRow {
    pub organization_id: Uuid,
    pub metric_date: NaiveDate,
    pub domain: String,
    pub event_type: String,
    pub event_count: i64,
    pub unique_sessions: i64,
}

/// One day on the publisher/agent funnel time-series.
///
/// Built server-side via `date_trunc('day', event_timestamp)::date` so the
/// dashboard does not have to bucket a sampled events list (which silently
/// collapsed to "all events on today" when the window held more rows than
/// the events endpoint returned at limit=500).
#[derive(Debug, Clone, Serialize)]
pub struct DayFunnelCount {
    pub date: NaiveDate,
    pub retrieved: i64,
    pub grounded: i64,
    pub cited: i64,
    /// Includes stored v0.1 `content_displayed` events, so charts stay
    /// continuous across the v1 rename.
    pub presented: i64,
    /// Deprecated alias for `presented` (the v0.1 stage name). Carries the
    /// same value during the transition - the website dashboard still reads
    /// `displayed`; a coordinated rename retires it.
    pub displayed: i64,
    pub engaged: i64,
}

/// Per-agent edge-vs-self-report reconciliation.
///
/// Edge counts come from the publisher's own emitter under its own key —
/// zero trust in the agent required. Everything past retrieval is
/// agent-attested: correlation operates at the retrieval level only (spec
/// s7.3), so grounded/cited/presented/engaged counts are what the agent
/// *says*, deterrence-audited, never corroborated. The field names say so.
#[derive(Debug, Clone, Serialize)]
pub struct AgentReconciliation {
    /// The identity both sides are joined on: the edge worker's detected
    /// `bot_name` and/or the session's self-declared `agent_id`. `None`
    /// groups edge events the worker could not attribute.
    pub identity: Option<String>,
    /// Retrievals the publisher's edge observed for this identity.
    pub edge_retrievals: i64,
    /// Retrievals the agent reported about this publisher's content.
    pub self_reported_retrievals: i64,
    /// self_reported / edge. `None` when the edge saw nothing (self-only
    /// identity: an edge coverage gap, a naming mismatch, or reports about
    /// fetches the edge never served). Above 1.0 the agent reports more
    /// than the edge saw — same three explanations, reversed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_report_ratio: Option<f64>,
    /// Sessions this identity reported in.
    pub sessions_reporting: i64,
    /// Agent-attested funnel stages past retrieval — uncorroborated by
    /// construction.
    pub agent_attested: AgentAttestedCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentAttestedCounts {
    pub grounded: i64,
    pub cited: i64,
    /// Presented count; stored v0.1 `content_displayed` rows are counted
    /// inside this stage so history stays continuous across the v1 rename.
    pub presented: i64,
    /// Deprecated alias for `presented` (the v0.1 stage name). Same value
    /// during the transition; the website dashboard still reads it.
    pub displayed: i64,
    pub engaged: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReconciliationReport {
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
    /// True when the organisation is a demo fixture.
    pub synthetic: bool,
    pub agents: Vec<AgentReconciliation>,
    /// Fixed honesty note: what the edge corroborates and what stays
    /// agent-attested.
    pub corroboration: &'static str,
}

/// The R3 honesty note carried on every reconciliation report.
pub const CORROBORATION_NOTE: &str = "Edge counts corroborate retrieval only. Grounding, \
     citation, display and engagement are agent-attested self-reports (spec s7.3: correlation \
     operates at the retrieval level); reconciliation deters under-reporting, it does not \
     verify compensation events.";

// ---------------------------------------------------------------------------
// Agent query responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AgentSummary {
    pub organization_id: Uuid,
    pub total_events: i64,
    pub total_sessions: i64,
    pub events_by_type: Vec<EventTypeCount>,
    pub domains: Vec<AgentDomainBreakdown>,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentDomainBreakdown {
    pub domain: String,
    pub event_count: i64,
    pub session_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentDomainMetric {
    pub domain: String,
    pub total_events: i64,
    pub unique_sessions: i64,
    pub event_types: Vec<EventTypeCount>,
    pub last_seen: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

/// Serde adapter that accepts either an RFC3339 timestamp
/// (`2026-05-07T00:00:00Z`) or a bare `YYYY-MM-DD` date (treated as
/// midnight UTC). Without this, every caller who copies the API docs
/// example (`?since=2026-03-01`) gets a hostile
/// "since: premature end of input" 400 from axum's query parser.
mod flexible_datetime {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Option<String> = Option::deserialize(deserializer)?;
        let Some(s) = raw else {
            return Ok(None);
        };
        if s.is_empty() {
            return Ok(None);
        }
        if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
            return Ok(Some(dt.with_timezone(&Utc)));
        }
        if let Ok(date) = NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
            let ndt = NaiveDateTime::new(date, NaiveTime::MIN);
            return Ok(Some(DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc)));
        }
        Err(serde::de::Error::custom(format!(
            "expected RFC3339 timestamp or YYYY-MM-DD date, got: {s}"
        )))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PublisherQueryParams {
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub until: Option<DateTime<Utc>>,
    pub domain: Option<String>,
    /// Filter to a specific bot name from edge detection
    /// (event_data.bot_name), e.g. "ClaudeBot".
    pub bot: Option<String>,
    /// Filter to an access purpose ("training" | "inference" | "search" |
    /// "advertising", spec 6.2). `bot_category` is accepted as a deprecated
    /// alias for the v0.1 field name (spec 12.1).
    #[serde(alias = "bot_category")]
    pub purpose: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConformanceQueryParams {
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub until: Option<DateTime<Utc>>,
    pub domain: Option<String>,
    /// Filter to one agent (sessions.agent_id).
    pub agent: Option<String>,
    /// How many recent sessions to evaluate (bounded server-side).
    #[serde(default)]
    pub session_limit: Option<i64>,
}

impl ConformanceQueryParams {
    pub const DEFAULT_SESSION_LIMIT: i64 = 500;
    pub const MAX_SESSION_LIMIT: i64 = 2000;

    /// Clamp the requested session limit to the server-side bounds.
    #[must_use]
    pub fn effective_session_limit(&self) -> i64 {
        self.session_limit
            .unwrap_or(Self::DEFAULT_SESSION_LIMIT)
            .clamp(1, Self::MAX_SESSION_LIMIT)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PaginatedQueryParams {
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub until: Option<DateTime<Utc>>,
    pub domain: Option<String>,
    pub bot: Option<String>,
    /// Access-purpose filter; `bot_category` accepted as a deprecated alias.
    #[serde(alias = "bot_category")]
    pub purpose: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentQueryParams {
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentPaginatedQueryParams {
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub since: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize")]
    pub until: Option<DateTime<Utc>>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

impl AgentPaginatedQueryParams {
    pub fn effective_limit(&self) -> i64 {
        self.limit.clamp(1, MAX_LIMIT)
    }

    /// A negative offset is a Postgres error; treat it as zero.
    pub fn effective_offset(&self) -> i64 {
        self.offset.max(0)
    }
}

const MAX_LIMIT: i64 = 1000;

fn default_limit() -> i64 {
    100
}

impl PaginatedQueryParams {
    /// Clamp limit to server-side maximum.
    pub fn effective_limit(&self) -> i64 {
        self.limit.clamp(1, MAX_LIMIT)
    }

    /// A negative offset is a Postgres error; treat it as zero.
    pub fn effective_offset(&self) -> i64 {
        self.offset.max(0)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    /// Set to `Some(true)` on responses whose organisation is a demo
    /// fixture: the listed items are fabricated. Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
}

// ---------------------------------------------------------------------------
// Admin query responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DomainPreview {
    pub domain: String,
    pub registered: bool,
    pub organization_id: Option<Uuid>,
    pub total_events: i64,
    pub events_by_type: Vec<EventTypeCount>,
    pub top_urls: Vec<DomainPreviewUrl>,
    pub agents: Vec<AgentBreakdown>,
    pub period_start: Option<DateTime<Utc>>,
    pub period_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DomainPreviewUrl {
    pub content_url: String,
    pub event_count: i64,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnclaimedDomain {
    pub domain: String,
    pub total_events: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub event_types: Vec<EventTypeCount>,
}

// ---------------------------------------------------------------------------
// Admin query params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct DomainPreviewParams {
    pub domain: String,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnclaimedDomainsParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "default_min_events")]
    pub min_events: i64,
}

fn default_min_events() -> i64 {
    10
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ResolveResponse {
    pub domain: String,
    pub handled: bool,
    pub organization: Option<ResolvedOrganization>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedOrganization {
    pub id: Uuid,
    pub name: String,
}
