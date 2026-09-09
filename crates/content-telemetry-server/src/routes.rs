use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use content_telemetry_core::models::click_token::{
    ClickTokenCreateRequest, ClickTokenCreateResponse,
};
use content_telemetry_core::models::event::{
    EventsCreateRequest, EventsCreatedResponse, TelemetryEventInput,
};
use content_telemetry_core::models::session::{
    BulkSessionRequest, BulkSessionResponse, SessionCreateRequest, SessionEndRequest,
    SessionStartResponse,
};
use content_telemetry_core::services::events::EventInsert;
use content_telemetry_core::services::{click_tokens, events, sessions};
use content_telemetry_core::{conformance, standard};

use crate::AppState;
use crate::auth::OrgContext;
use crate::error::ApiError;
use crate::validate;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/sessions/start", post(start_session))
        .route("/sessions/end", post(end_session))
        .route("/sessions/bulk", post(bulk_session))
        .route("/sessions/{id}/document", get(session_document))
        .route("/events", post(record_events))
        .route("/click-tokens", post(create_click_token))
        .route("/ctx/{token}", get(lookup_ctx))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(json!({ "status": "ready" })))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

async fn start_session(
    State(state): State<AppState>,
    OrgContext(org): OrgContext,
    Json(mut req): Json<SessionCreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    check_initiator_type(&req.initiator_type)?;
    check_session_data(req.data.as_ref())?;
    log_unrecognised_fields(&req.unrecognised_fields);

    // Informational only: the spec forbids rejecting a document over its
    // conformance level, so a value we do not recognise is logged and stored,
    // never refused.
    let level = conformance::normalise_conformance_level(req.conformance_level.as_deref());
    if level.non_standard {
        tracing::info!(
            level = ?req.conformance_level,
            "session declares a non-standard conformance level"
        );
    }
    req.conformance_level = level.value;

    let session = sessions::create_session(&state.pool, org, &req).await?;

    Ok((
        StatusCode::CREATED,
        Json(SessionStartResponse {
            session_id: session.id.to_string(),
        }),
    ))
}

async fn end_session(
    State(state): State<AppState>,
    OrgContext(org): OrgContext,
    Json(req): Json<SessionEndRequest>,
) -> Result<impl IntoResponse, ApiError> {
    check_outcome_type(&req.outcome.outcome_type)?;

    let presented = parse_uuid(&req.session_id, "session_id")?;
    let session = sessions::find_owned_session(&state.pool, org, presented)
        .await?
        .ok_or(ApiError::NotFound)?;

    // Re-key onto the id we actually store, so a caller that presented its
    // own session id still ends the right row.
    let owned = SessionEndRequest {
        session_id: session.id.to_string(),
        outcome: req.outcome,
    };

    // No emitter-supplied end timestamp on this path: the session ends when
    // the caller says so, which is now.
    let ended = sessions::end_session(&state.pool, org, &owned, None)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(json!({
        "status": "ok",
        "session_id": ended.id.to_string(),
    })))
}

/// Ingest a complete session document in one request: the session, its
/// events, and its outcome.
async fn bulk_session(
    State(state): State<AppState>,
    OrgContext(org): OrgContext,
    Json(req): Json<BulkSessionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(document_type) = req.document_type.as_deref()
        && document_type != "session"
    {
        return Err(ApiError::bad_request(
            "document_type must be 'session' on this endpoint",
        ));
    }

    let Some(line) = conformance::schema_line(req.schema_version.as_deref()) else {
        return Err(unsupported_schema_version(req.schema_version.as_deref()));
    };

    check_initiator_type(&req.initiator_type)?;
    check_session_data(req.data.as_ref())?;
    log_unrecognised_fields(&req.unrecognised_fields);
    if let Some(outcome) = req.outcome.as_ref() {
        check_outcome_type(&outcome.outcome_type)?;
    }

    if req.events.len() > validate::MAX_BATCH_SIZE {
        return Err(too_large(req.events.len()));
    }

    // The presented session id is the emitter's, not ours. It is stored as
    // the external id under a server-minted primary key, so two emitters
    // cannot collide on a chosen id and neither can address the other's row.
    // The mapping lives on the document type: the session-level `data`
    // container and any top-level members the server does not define travel
    // to storage with everything else (spec 5.1.3).
    let mut create = req.session_create();
    create.conformance_level =
        conformance::normalise_conformance_level(req.conformance_level.as_deref()).value;

    let mut events_in = req.events;
    for (index, event) in events_in.iter_mut().enumerate() {
        // Normalise before checking: a "0.1" document's migration defaults
        // (spec 12.1) must land before the structural rules judge it.
        log_notes(validate::normalise(event, line));
        validate::check_event(event, line, state.max_event_age_days, index)?;
    }

    let session = sessions::create_session(&state.pool, org, &create).await?;

    let created = if events_in.is_empty() {
        0
    } else {
        events::create_events(&state.pool, session.id, org, &events_in)
            .await?
            .len()
    };

    let mut outcome_recorded = false;
    if let Some(outcome) = req.outcome {
        let end = SessionEndRequest {
            session_id: session.id.to_string(),
            outcome,
        };
        // The document carries its own end timestamp; preserve it rather than
        // stamping the moment we happened to receive the upload.
        outcome_recorded = sessions::end_session(&state.pool, org, &end, req.ended_at)
            .await?
            .is_some();
    }

    Ok((
        StatusCode::CREATED,
        Json(BulkSessionResponse {
            session_id: session.id.to_string(),
            events_created: created,
            outcome_recorded,
        }),
    ))
}

/// The session as a Content Telemetry session document — the standard shape,
/// not this server's storage shape.
async fn session_document(
    State(state): State<AppState>,
    OrgContext(org): OrgContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let session = sessions::find_owned_session(&state.pool, org, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let with_events = sessions::get_session_with_events(&state.pool, session.id)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(standard::standard_document(&with_events)))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// How one event found its session, if it has one.
enum Binding {
    /// Bound to a session owned by the calling organisation.
    Owned(Uuid),
    /// Bound through a ctx token, which is a bearer capability: whoever holds
    /// it may report an engagement against the session that issued it, and
    /// that session belongs to a different organisation. The event is
    /// therefore stored under the *issuing* organisation, so the manifest and
    /// the session's own reads stay coherent.
    Delegated { session_id: Uuid, owner: Uuid },
    /// A retrieval-level observation with no interaction behind it.
    Sessionless,
}

async fn record_events(
    State(state): State<AppState>,
    OrgContext(org): OrgContext,
    Json(req): Json<EventsCreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(document_type) = req.document_type.as_deref()
        && document_type != "event"
        && document_type != "event_batch"
    {
        return Err(ApiError::bad_request(
            "document_type must be 'event' or 'event_batch'; session documents go to /sessions/bulk",
        ));
    }

    let Some(line) = conformance::schema_line(req.schema_version.as_deref()) else {
        return Err(unsupported_schema_version(req.schema_version.as_deref()));
    };

    // Both envelopes are accepted: `event` for a single standalone event,
    // `events` for a batch (specification section 7.1).
    let mut incoming = req.events;
    if let Some(single) = req.event {
        incoming.push(single);
    }

    if incoming.is_empty() {
        return Err(ApiError::bad_request(
            "supply at least one event via 'event' or 'events'",
        ));
    }

    if incoming.len() > validate::MAX_BATCH_SIZE {
        return Err(too_large(incoming.len()));
    }

    // An envelope ctx_token accompanies content_engaged events only (spec
    // 5.7.5). Checked here, not during binding: a batch that also presents
    // a session_id binds through the session, but presenting the token
    // alongside non-engagement claims is still malformed.
    if req.ctx_token.is_some()
        && let Some(index) = incoming
            .iter()
            .position(|e| e.event_type != "content_engaged")
    {
        return Err(ApiError::bad_request(format!(
            "event {index}: an envelope ctx_token may only accompany content_engaged events; \
             supply a session_id instead"
        )));
    }

    let batch_session = req
        .session_id
        .as_deref()
        .map(|v| parse_uuid(v, "session_id"));
    let batch_session = batch_session.transpose()?;

    for (index, event) in incoming.iter_mut().enumerate() {
        // Normalise before checking: a "0.1" document's migration defaults
        // (spec 12.1) must land before the structural rules judge it.
        log_notes(validate::normalise(event, line));
        validate::check_event(event, line, state.max_event_age_days, index)?;
    }

    let defaults = BatchDefaults {
        session_id: batch_session,
        ctx_token: req.ctx_token.as_deref(),
        agent_id: req.agent_id.as_deref(),
        started_at: req.started_at,
    };

    let mut bindings = Vec::with_capacity(incoming.len());
    for (index, event) in incoming.iter().enumerate() {
        bindings.push(resolve_binding(&state, org, event, index, &defaults).await?);
    }

    let inserts: Vec<EventInsert<'_>> = incoming
        .iter()
        .zip(bindings.iter())
        .map(|(event, binding)| match binding {
            Binding::Owned(session_id) => EventInsert {
                session_id: Some(*session_id),
                organization_id: org,
                event,
            },
            Binding::Delegated { session_id, owner } => EventInsert {
                session_id: Some(*session_id),
                organization_id: *owner,
                event,
            },
            Binding::Sessionless => EventInsert {
                session_id: None,
                organization_id: org,
                event,
            },
        })
        .collect();

    let rows = events::create_events_with_context(&state.pool, &inserts).await?;

    Ok((
        StatusCode::CREATED,
        Json(EventsCreatedResponse {
            status: "ok".to_string(),
            // Ingest is idempotent on client-supplied event ids, so a replayed
            // batch reports fewer created than submitted rather than
            // duplicating rows.
            events_created: rows.len(),
        }),
    ))
}

/// Request-level values an individual event falls back to when it does not
/// carry its own.
struct BatchDefaults<'a> {
    session_id: Option<Uuid>,
    ctx_token: Option<&'a str>,
    agent_id: Option<&'a str>,
    started_at: Option<DateTime<Utc>>,
}

/// Binding precedence: the event's own session, then the batch's, then the
/// event's ctx token, then the batch's, then sessionless.
async fn resolve_binding(
    state: &AppState,
    org: Uuid,
    event: &TelemetryEventInput,
    index: usize,
    defaults: &BatchDefaults<'_>,
) -> Result<Binding, ApiError> {
    if let Some(presented) = event.session_id.or(defaults.session_id) {
        return resolve_owned_session(
            state,
            org,
            presented,
            defaults.agent_id,
            defaults.started_at,
        )
        .await
        .map(Binding::Owned);
    }

    if let Some(token) = event.ctx_token.as_deref().or(defaults.ctx_token) {
        // A ctx token authorises reporting engagement with content the
        // session surfaced. It does not authorise writing arbitrary claims
        // into someone else's session, so only engagement crosses this
        // boundary.
        if event.event_type != "content_engaged" {
            return Err(ApiError::bad_request(format!(
                "event {index}: a ctx_token may only carry content_engaged; supply a session_id instead"
            )));
        }

        // Well-formedness before lookup (spec 7.4.1): every token this
        // server mints matches the pattern, so anything else can only be
        // noise or probing and never reaches the database.
        if !click_tokens::ctx_token_well_formed(token) {
            return Err(ApiError::bad_request(format!(
                "event {index}: malformed ctx_token; token values match \
                 ^ct_[A-Za-z0-9_-]{{16,240}}$ (spec 7.4.1)"
            )));
        }

        let session_id = click_tokens::resolve_session_id(&state.pool, token)
            .await?
            .ok_or_else(|| {
                ApiError::bad_request(format!("event {index}: unknown or expired ctx_token"))
            })?;

        let session = sessions::get_session(&state.pool, session_id)
            .await?
            .ok_or_else(|| {
                ApiError::bad_request(format!("event {index}: ctx_token resolves to no session"))
            })?;

        return Ok(Binding::Delegated {
            session_id: session.id,
            owner: session.organization_id,
        });
    }

    validate::check_sessionless(event, index)?;
    Ok(Binding::Sessionless)
}

/// Find the caller's session, or reconstruct it.
///
/// Events can outrun the session that explains them — a batch flushed after a
/// crash, an emitter that never called `/sessions/start`. Rather than drop
/// them, an unknown id is created under a UUIDv5 derived from the
/// organisation and the presented id, keeping the emitter's value as the
/// external id. The derivation is what makes it safe: two organisations
/// presenting the same id get two different rows, so neither can reach the
/// other's session by guessing.
async fn resolve_owned_session(
    state: &AppState,
    org: Uuid,
    presented: Uuid,
    agent_id: Option<&str>,
    started_at: Option<DateTime<Utc>>,
) -> Result<Uuid, ApiError> {
    if let Some(session) = sessions::find_owned_session(&state.pool, org, presented).await? {
        if session.ended_at.is_some() {
            return Err(ApiError::bad_request(
                "session has ended; start a new session for further events",
            ));
        }
        return Ok(session.id);
    }

    tracing::info!(
        %presented,
        agent_id,
        "reconstructing a session from an event batch"
    );

    let scoped = sessions::scoped_session_id(org, presented);
    let session = sessions::ensure_session(
        &state.pool,
        scoped,
        org,
        agent_id,
        started_at,
        Some(&presented.to_string()),
        None,
    )
    .await?;

    Ok(session.id)
}

// ---------------------------------------------------------------------------
// ctx tokens
// ---------------------------------------------------------------------------

async fn create_click_token(
    State(state): State<AppState>,
    OrgContext(org): OrgContext,
    Json(req): Json<ClickTokenCreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // A caller-supplied token is held to the same spec 7.4.1 shape as a
    // minted one; the unguessability of its suffix is the issuer's burden.
    if let Some(token) = req.token.as_deref()
        && !click_tokens::ctx_token_well_formed(token)
    {
        return Err(ApiError::bad_request(
            "token must match ^ct_[A-Za-z0-9_-]{16,240}$ (spec 7.4.1)",
        ));
    }

    let session = sessions::find_owned_session(&state.pool, org, req.session_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let row = click_tokens::create_click_token(
        &state.pool,
        session.id,
        &req.content_url,
        req.token.as_deref(),
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(ClickTokenCreateResponse {
            token: row.token,
            session_id: row.session_id,
            content_url: row.content_url,
            expires_at: row.expires_at,
        }),
    ))
}

/// Resolve a ctx token to its click context.
///
/// Deliberately unauthenticated: the destination of a click-out has no
/// account here, and requiring one would defeat the mechanism. The token is
/// the credential, and what it discloses is bounded twice over — by
/// two-sided consent inside the core lookup, and by the response shape, which
/// never contains the session id. A missing token, an expired one and a
/// non-consenting one are all 404, so the endpoint cannot be used to probe
/// which sessions exist.
///
/// TODO(spec 7.4.4): the response is still the v0.1 click-manifest shape;
/// the v1 four-component click context (engagement, clicked-content
/// lineage, turn-scoped contributing sources, count-based session summary)
/// is a separate design change — see the note on
/// `click_tokens::lookup_by_token`.
async fn lookup_ctx(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let resolved = click_tokens::lookup_by_token(&state.pool, &token)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(resolved.manifest))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn parse_uuid(value: &str, field: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value)
        .map_err(|_| ApiError::bad_request(format!("{field} must be a UUID, got '{value}'")))
}

fn check_initiator_type(value: &str) -> Result<(), ApiError> {
    if matches!(value, "user" | "agent") {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "initiator_type must be 'user' or 'agent'",
        ))
    }
}

/// The session-level `data` container is an extension point, so almost
/// nothing in it is checked — but `access_context` is defined in core (spec
/// 5.1.3) and the schema gives it a shape, so a malformed one is refused
/// rather than stored as a claim nobody can read.
fn check_session_data(data: Option<&Value>) -> Result<(), ApiError> {
    match conformance::session_data_violation(data) {
        Some(violation) => Err(ApiError::bad_request(violation)),
        None => Ok(()),
    }
}

/// Record top-level members the server does not define.
///
/// The session root is not an extension point (spec 5.1.3) and a consumer
/// MUST tolerate unknown fields without error (spec 5.7.4), so the document
/// is accepted either way. What must not happen is accepting it silently:
/// the members are logged here, stored on the session, and returned under
/// the document's `extensions`. A field the specification adds that this
/// server does not implement yet shows up in all three places instead of
/// disappearing into a 201.
fn log_unrecognised_fields(fields: &serde_json::Map<String, Value>) {
    if fields.is_empty() {
        return;
    }

    let names = fields
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    tracing::warn!(
        fields = names,
        "session document carries top-level fields this server does not define; stored and \
         returned under extensions.unrecognised_fields (spec 5.1.3, 5.7.4)"
    );
}

fn check_outcome_type(value: &str) -> Result<(), ApiError> {
    if matches!(value, "conversion" | "abandonment" | "browse") {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "outcome type must be 'conversion', 'abandonment' or 'browse'",
        ))
    }
}

fn too_large(count: usize) -> ApiError {
    ApiError::bad_request(format!(
        "batch of {count} exceeds the limit of {} events",
        validate::MAX_BATCH_SIZE
    ))
}

/// The spec's exact-minor rule (section 5.7.4): a consumer accepts the minor
/// versions it implements and refuses the rest, rather than guessing.
fn unsupported_schema_version(presented: Option<&str>) -> ApiError {
    ApiError::bad_request(format!(
        "unsupported schema_version {:?}; this server implements {}",
        presented.unwrap_or("(absent)"),
        conformance::ACCEPTED_SCHEMA_VERSIONS.join(", ")
    ))
}

fn log_notes(notes: Vec<String>) {
    for note in notes {
        tracing::info!(note, "normalised event on ingest");
    }
}
