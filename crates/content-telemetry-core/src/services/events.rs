use sqlx::PgPool;
use uuid::Uuid;

use crate::models::event::{EdgeEventInput, EventRow, TelemetryEventInput};

/// Normalise the closed-enum members of an event's `data` before storage
/// (spec Annex A), so the materialised session document stays schema-valid,
/// and drop the fields v1 withdrew (spec 9.1) so they are never stored.
fn normalised_event_data(event_type: &str, mut data: serde_json::Value) -> serde_json::Value {
    let changed = crate::conformance::normalise_event_data_enums(event_type, &mut data);
    if !changed.is_empty() {
        tracing::warn!(
            event_type,
            fields = ?changed,
            "Normalised non-standard closed-enum values in event data"
        );
    }
    let stripped = crate::conformance::strip_withdrawn_data_fields(&mut data);
    if !stripped.is_empty() {
        tracing::warn!(
            event_type,
            fields = ?stripped,
            "Stripped v1-withdrawn event data fields"
        );
    }
    data
}

pub struct EventInsert<'a> {
    pub session_id: Option<Uuid>,
    pub organization_id: Uuid,
    pub event: &'a TelemetryEventInput,
}

/// Batch-insert session events using UNNEST for single round-trip performance.
pub async fn create_events(
    pool: &PgPool,
    session_id: Uuid,
    organization_id: Uuid,
    events: &[TelemetryEventInput],
) -> Result<Vec<EventRow>, sqlx::Error> {
    let inserts = events
        .iter()
        .map(|event| EventInsert {
            session_id: Some(session_id),
            organization_id,
            event,
        })
        .collect::<Vec<_>>();

    create_events_with_context(pool, &inserts).await
}

/// Batch-insert events with per-row session and organisation context.
///
/// Idempotent on client-supplied event ids: a duplicate id (a retried
/// delivery) is skipped rather than failing the whole batch, and the
/// returned rows cover only the events actually inserted.
pub async fn create_events_with_context(
    pool: &PgPool,
    events: &[EventInsert<'_>],
) -> Result<Vec<EventRow>, sqlx::Error> {
    if events.is_empty() {
        return Ok(Vec::new());
    }

    let len = events.len();
    let mut ids = Vec::with_capacity(len);
    let mut session_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut org_ids = Vec::with_capacity(len);
    let mut event_types = Vec::with_capacity(len);
    let mut source_roles: Vec<Option<String>> = Vec::with_capacity(len);
    let mut content_telemetry_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut content_urls: Vec<Option<String>> = Vec::with_capacity(len);
    let mut content_ids: Vec<Option<String>> = Vec::with_capacity(len);
    let mut turn_ids: Vec<Option<String>> = Vec::with_capacity(len);
    let mut output_ids: Vec<Option<String>> = Vec::with_capacity(len);
    let mut output_element_ids: Vec<Option<String>> = Vec::with_capacity(len);
    let mut citation_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut presentation_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut ctx_tokens: Vec<Option<String>> = Vec::with_capacity(len);
    let mut license_refs: Vec<Option<String>> = Vec::with_capacity(len);
    let mut terms_refs: Vec<Option<String>> = Vec::with_capacity(len);
    let mut product_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut turn_datas: Vec<Option<serde_json::Value>> = Vec::with_capacity(len);
    let mut event_datas = Vec::with_capacity(len);
    let mut timestamps = Vec::with_capacity(len);

    for event in events {
        ids.push(event.event.id.unwrap_or_else(Uuid::new_v4));
        session_ids.push(event.session_id);
        org_ids.push(event.organization_id);
        event_types.push(event.event.event_type.clone());
        source_roles.push(event.event.source_role.clone());
        content_telemetry_ids.push(event.event.content_telemetry_id);
        content_urls.push(event.event.content_url.clone());
        content_ids.push(event.event.content_id.clone());
        turn_ids.push(event.event.turn_id.clone());
        output_ids.push(event.event.output_id.clone());
        output_element_ids.push(event.event.output_element_id.clone());
        citation_ids.push(event.event.citation_id);
        presentation_ids.push(event.event.presentation_id);
        ctx_tokens.push(event.event.ctx_token.clone());
        license_refs.push(event.event.license_ref.clone());
        // terms_ref is preserved byte-for-byte (spec 5.2.4): a processor
        // MUST NOT rewrite it, so it is cloned and bound with no
        // normalisation of any kind.
        terms_refs.push(event.event.terms_ref.clone());
        product_ids.push(event.event.product_id);
        // A consumer that receives a privacy-violating turn strips the
        // offending fields rather than rejecting the document (spec 5.7.5).
        turn_datas.push(event.event.turn.clone().map(|mut turn| {
            let stripped = crate::conformance::strip_turn_privacy_violations(&mut turn);
            if !stripped.is_empty() {
                tracing::warn!(
                    event_type = %event.event.event_type,
                    stripped = ?stripped,
                    "Stripped privacy-violating turn fields"
                );
            }
            turn
        }));
        event_datas.push(normalised_event_data(
            &event.event.event_type,
            event.event.data.clone(),
        ));
        timestamps.push(event.event.timestamp);
    }

    sqlx::query_as::<_, EventRow>(
        r"INSERT INTO events (
            id, session_id, organization_id, event_type, source_role, content_telemetry_id,
            content_url, content_id, turn_id,
            output_id, output_element_id, citation_id, presentation_id, ctx_token, license_ref,
            terms_ref, product_id, turn_data, event_data, event_timestamp
        )
        SELECT * FROM UNNEST(
            $1::uuid[], $2::uuid[], $3::uuid[], $4::text[], $5::text[], $6::uuid[],
            $7::text[], $8::text[], $9::text[],
            $10::text[], $11::text[], $12::uuid[], $13::uuid[], $14::text[], $15::text[],
            $16::text[], $17::uuid[], $18::jsonb[], $19::jsonb[], $20::timestamptz[]
        )
        ON CONFLICT (id) DO NOTHING
        RETURNING *",
    )
    .bind(&ids)
    .bind(&session_ids)
    .bind(&org_ids)
    .bind(&event_types)
    .bind(&source_roles)
    .bind(&content_telemetry_ids)
    .bind(&content_urls)
    .bind(&content_ids)
    .bind(&turn_ids)
    .bind(&output_ids)
    .bind(&output_element_ids)
    .bind(&citation_ids)
    .bind(&presentation_ids)
    .bind(&ctx_tokens)
    .bind(&license_refs)
    .bind(&terms_refs)
    .bind(&product_ids)
    .bind(&turn_datas)
    .bind(&event_datas)
    .bind(&timestamps)
    .fetch_all(pool)
    .await
}

/// Batch-insert sessionless edge/origin events.
pub async fn create_edge_events(
    pool: &PgPool,
    organization_id: Uuid,
    events: &[EdgeEventInput],
) -> Result<Vec<EventRow>, sqlx::Error> {
    if events.is_empty() {
        return Ok(Vec::new());
    }

    let len = events.len();
    let mut ids = Vec::with_capacity(len);
    let mut org_ids = Vec::with_capacity(len);
    let mut event_types = Vec::with_capacity(len);
    let mut source_roles: Vec<Option<String>> = Vec::with_capacity(len);
    let mut content_telemetry_ids: Vec<Option<Uuid>> = Vec::with_capacity(len);
    let mut content_urls: Vec<Option<String>> = Vec::with_capacity(len);
    let mut content_ids: Vec<Option<String>> = Vec::with_capacity(len);
    let mut license_refs: Vec<Option<String>> = Vec::with_capacity(len);
    let mut event_datas = Vec::with_capacity(len);
    let mut timestamps = Vec::with_capacity(len);

    for event in events {
        ids.push(event.id.unwrap_or_else(Uuid::new_v4));
        org_ids.push(organization_id);
        event_types.push(event.event_type.clone());
        source_roles.push(event.source_role.clone());
        content_telemetry_ids.push(event.content_telemetry_id);
        content_urls.push(event.content_url.clone());
        content_ids.push(event.content_id.clone());
        license_refs.push(event.license_ref.clone());
        event_datas.push(normalised_event_data(&event.event_type, event.data.clone()));
        timestamps.push(event.timestamp);
    }

    sqlx::query_as::<_, EventRow>(
        r"INSERT INTO events (
            id, organization_id, event_type, source_role, content_telemetry_id,
            content_url, content_id, license_ref,
            event_data, event_timestamp
        )
        SELECT * FROM UNNEST(
            $1::uuid[], $2::uuid[], $3::text[], $4::text[], $5::uuid[],
            $6::text[], $7::text[], $8::text[],
            $9::jsonb[], $10::timestamptz[]
        )
        ON CONFLICT (id) DO NOTHING
        RETURNING *",
    )
    .bind(&ids)
    .bind(&org_ids)
    .bind(&event_types)
    .bind(&source_roles)
    .bind(&content_telemetry_ids)
    .bind(&content_urls)
    .bind(&content_ids)
    .bind(&license_refs)
    .bind(&event_datas)
    .bind(&timestamps)
    .fetch_all(pool)
    .await
}

/// Get all events for a session, ordered by timestamp.
pub async fn get_events_for_session(
    pool: &PgPool,
    session_id: Uuid,
) -> Result<Vec<EventRow>, sqlx::Error> {
    sqlx::query_as::<_, EventRow>(
        "SELECT * FROM events WHERE session_id = $1 ORDER BY event_timestamp ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
}
