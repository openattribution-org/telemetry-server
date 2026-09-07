use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::event::EventRow;
use crate::models::session::{
    SessionCreateRequest, SessionEndRequest, SessionRow, SessionSummary, SessionWithEvents,
};

/// Create a new telemetry session stamped with the owning entity.
///
/// The `owner_id` is stored in the `organization_id` column. Callers resolve
/// this from their own auth model (platform API key, gateway header, etc.).
pub async fn create_session(
    pool: &PgPool,
    owner_id: Uuid,
    req: &SessionCreateRequest,
) -> Result<SessionRow, sqlx::Error> {
    // Parse prior_session_ids, skipping invalid UUIDs
    let prior: Vec<Uuid> = req
        .prior_session_ids
        .iter()
        .filter_map(|s| s.parse::<Uuid>().ok())
        .collect();

    // Normalise the informational conformance level (spec 5.7): legacy
    // 'attribution' becomes 'citation'; non-standard values are stored
    // as supplied but flagged, never rejected.
    let conformance =
        crate::conformance::normalise_conformance_level(req.conformance_level.as_deref());
    if conformance.non_standard {
        tracing::warn!(
            conformance_level = ?req.conformance_level,
            "Non-standard conformance_level on session (informational, stored as supplied)"
        );
    }

    // The session-level `data` container and any top-level members this
    // server does not define are stored as given (spec 5.1.3): nothing
    // inside either is interpreted, normalised or dropped.
    let unrecognised = if req.unrecognised_fields.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(req.unrecognised_fields.clone()))
    };

    sqlx::query_as::<_, SessionRow>(
        r"INSERT INTO sessions (
            organization_id, parent_session_id, initiator_type, initiator,
            content_scope, manifest_ref, conformance_level,
            agent_id, external_session_id, prior_session_ids,
            session_data, unrecognised_fields,
            user_context, platform_id, client_type, client_info,
            started_at, ended_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                $15, $16, COALESCE($17, NOW()), $18)
        RETURNING *",
    )
    .bind(owner_id)
    .bind(req.parent_session_id)
    .bind(&req.initiator_type)
    .bind(&req.initiator)
    .bind(&req.content_scope)
    .bind(&req.manifest_ref)
    .bind(&conformance.value)
    .bind(&req.agent_id)
    .bind(&req.external_session_id)
    .bind(&prior)
    .bind(&req.data)
    .bind(&unrecognised)
    .bind(&req.user_context)
    .bind(&req.platform_id)
    .bind(&req.client_type)
    .bind(&req.client_info)
    .bind(req.started_at)
    .bind(req.ended_at)
    .fetch_one(pool)
    .await
}

/// Get a session by its ID, creating a placeholder if it does not exist.
///
/// A conforming telemetry consumer reconstructs sessions from standalone
/// event streams (spec 5.7.4): an emitter at Grounding conformance may
/// deliver events carrying a `session_id` the server has never been told
/// about, mirroring `agent_id` and `started_at` on the event envelope
/// (spec 7.1). The reconstructed session is owned by the submitting
/// organisation. Returns the existing row untouched when the session is
/// already known.
///
/// Callers must pass an org-scoped `session_id` they minted themselves,
/// never an emitter-chosen value: inserting a client-supplied UUID here
/// would let any org squat an id another emitter presents later. The
/// emitter's own id travels in `external_session_id`.
pub async fn ensure_session(
    pool: &PgPool,
    session_id: Uuid,
    owner_id: Uuid,
    agent_id: Option<&str>,
    started_at: Option<DateTime<Utc>>,
    external_session_id: Option<&str>,
    parent_session_id: Option<Uuid>,
) -> Result<SessionRow, sqlx::Error> {
    sqlx::query(
        r"INSERT INTO sessions (id, organization_id, initiator_type, agent_id, started_at,
                                external_session_id, parent_session_id)
        VALUES ($1, $2, 'user', $3, COALESCE($4, NOW()), $5, $6)
        ON CONFLICT (id) DO NOTHING",
    )
    .bind(session_id)
    .bind(owner_id)
    .bind(agent_id)
    .bind(started_at)
    .bind(external_session_id)
    .bind(parent_session_id)
    .execute(pool)
    .await?;

    sqlx::query_as::<_, SessionRow>("SELECT * FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_one(pool)
        .await
}

/// The org-scoped session id for an emitter-chosen session id.
///
/// Emitter-generated ids from standalone event streams are reconstructed
/// under this deterministic UUIDv5 rather than as primary ids, so two
/// organisations presenting the same id get distinct sessions and no
/// organisation can claim an id another emitter presents later. The
/// presented id is kept on the row as `external_session_id`.
pub fn scoped_session_id(organization_id: Uuid, presented: Uuid) -> Uuid {
    Uuid::new_v5(&organization_id, presented.as_bytes())
}

/// Resolve a presented session id to a session the caller owns, in
/// deterministic precedence order: the raw id for server-minted sessions
/// (/sessions/start, pre-scoping reconstructions), then the org-scoped id
/// for sessions reconstructed from an emitter-chosen id, then an
/// org-scoped `external_session_id` lookup for sessions uploaded via
/// /sessions/bulk (which store the emitter's presented id as the external
/// id under a server-minted primary id).
pub async fn find_owned_session(
    pool: &PgPool,
    organization_id: Uuid,
    presented: Uuid,
) -> Result<Option<SessionRow>, sqlx::Error> {
    if let Some(session) = get_session(pool, presented).await?
        && session.organization_id == organization_id
    {
        return Ok(Some(session));
    }
    if let Some(session) = get_session(pool, scoped_session_id(organization_id, presented))
        .await?
        .filter(|s| s.organization_id == organization_id)
    {
        return Ok(Some(session));
    }
    get_session_by_external_id(pool, organization_id, &presented.to_string()).await
}

/// Get a session by ID.
pub async fn get_session(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionRow>("SELECT * FROM sessions WHERE id = $1")
        .bind(session_id)
        .fetch_optional(pool)
        .await
}

/// Get a session by external session ID (most recent), scoped to the
/// owning organisation. External IDs are emitter-chosen, so two orgs can
/// legitimately use the same value; the org predicate keeps the lookup
/// from resolving to another tenant's session.
pub async fn get_session_by_external_id(
    pool: &PgPool,
    organization_id: Uuid,
    external_id: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionRow>(
        "SELECT * FROM sessions
         WHERE external_session_id = $1 AND organization_id = $2
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(external_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
}

/// Get a session with all its events.
pub async fn get_session_with_events(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionWithEvents>, sqlx::Error> {
    let Some(session) = get_session(pool, session_id).await? else {
        return Ok(None);
    };

    let events = sqlx::query_as::<_, EventRow>(
        "SELECT * FROM events WHERE session_id = $1 ORDER BY event_timestamp ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    Ok(Some(SessionWithEvents { session, events }))
}

/// End a session with outcome, scoped to the owning organisation.
///
/// `ended_at` preserves an emitter-supplied end timestamp (the bulk
/// session-document flow); when `None`, the session is stamped with NOW()
/// (the /sessions/end flow).
pub async fn end_session(
    pool: &PgPool,
    organization_id: Uuid,
    req: &SessionEndRequest,
    ended_at: Option<DateTime<Utc>>,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let session_id: Uuid = req
        .session_id
        .parse()
        .map_err(|_| sqlx::Error::Protocol("Invalid session_id UUID".to_string()))?;

    let outcome_value = serde_json::to_value(&req.outcome).unwrap_or_default();

    sqlx::query_as::<_, SessionRow>(
        r"UPDATE sessions
        SET ended_at = COALESCE($5, NOW()),
            outcome_type = $1,
            outcome_value = $2
        WHERE id = $3 AND organization_id = $4 AND ended_at IS NULL
        RETURNING *",
    )
    .bind(&req.outcome.outcome_type)
    .bind(&outcome_value)
    .bind(session_id)
    .bind(organization_id)
    .bind(ended_at)
    .fetch_optional(pool)
    .await
}

/// List sessions with filters (for attribution systems), scoped to the
/// owning organisation.
#[allow(clippy::too_many_arguments)]
pub async fn list_sessions(
    pool: &PgPool,
    organization_id: Uuid,
    outcome_type: Option<&str>,
    content_scope: Option<&str>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: i64,
    offset: i64,
) -> Result<Vec<SessionSummary>, sqlx::Error> {
    let mut query = String::from(
        "SELECT id, content_scope, external_session_id, outcome_type, started_at, ended_at
         FROM sessions WHERE organization_id = $1",
    );
    let mut param_idx = 2u32;

    struct Params {
        outcome_type: Option<String>,
        content_scope: Option<String>,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        limit: i64,
        offset: i64,
    }

    let params = Params {
        outcome_type: outcome_type.map(String::from),
        content_scope: content_scope.map(String::from),
        since,
        until,
        limit,
        offset,
    };

    if params.outcome_type.is_some() {
        query.push_str(&format!(" AND outcome_type = ${param_idx}"));
        param_idx += 1;
    }
    if params.content_scope.is_some() {
        query.push_str(&format!(" AND content_scope = ${param_idx}"));
        param_idx += 1;
    }
    if params.since.is_some() {
        query.push_str(&format!(" AND ended_at >= ${param_idx}"));
        param_idx += 1;
    }
    if params.until.is_some() {
        query.push_str(&format!(" AND ended_at <= ${param_idx}"));
        param_idx += 1;
    }

    query.push_str(&format!(
        " ORDER BY started_at DESC LIMIT ${param_idx} OFFSET ${}",
        param_idx + 1
    ));

    let mut q = sqlx::query_as::<_, SessionSummary>(&query).bind(organization_id);
    if let Some(ref v) = params.outcome_type {
        q = q.bind(v);
    }
    if let Some(ref v) = params.content_scope {
        q = q.bind(v);
    }
    if let Some(ref v) = params.since {
        q = q.bind(v);
    }
    if let Some(ref v) = params.until {
        q = q.bind(v);
    }
    q = q.bind(params.limit).bind(params.offset);

    q.fetch_all(pool).await
}
