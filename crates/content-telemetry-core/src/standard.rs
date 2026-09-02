//! Materialise a stored session as a Content Telemetry v1 session
//! document.
//!
//! This server's own API has its own envelope shapes; the document this
//! module produces is the standard one (spec section 5.1, validated by
//! `telemetry-session.json`). Implementation extensions - session outcomes,
//! prior_session_ids, initiator_type, the platform/client fields, and
//! extension event types such as `checkout_completed` - never appear as
//! top-level standard fields. They are namespaced under an
//! `extensions` member, which conforming consumers tolerate as an unknown
//! field (spec 5.7.4).

use serde_json::{Map, Value, json};

use crate::models::event::EventRow;
use crate::models::session::SessionWithEvents;

/// Event types the core schema validates (spec 5.3). Anything else is an
/// extension event and moves to the document's `extensions` member -
/// including `content_displayed`, which v1 withdrew (spec 12.1): stored
/// v0.1 rows keep their type and are quarantined rather than rewritten
/// into presentation claims the emitter never made.
const CORE_EVENT_TYPES: &[&str] = &[
    "content_retrieved",
    "content_grounded",
    "content_reproduced",
    "content_cited",
    "content_presented",
    "content_engaged",
    "turn_started",
    "turn_completed",
];

/// Prepare a stored row's `data` for the v1 document: normalise closed
/// enums (spec Annex A), strip the fields v1 withdrew (spec 9.1), and apply
/// the spec 12.1 migration defaults for rows written before versioned
/// ingest. Rows ingested on the v1 line come out unchanged; pre-v1 rows
/// come out reading as the v1 document the migration rules define.
fn prepared_event_data(row: &EventRow) -> Value {
    let mut data = row.event_data.clone();
    if !data.is_null() {
        crate::conformance::normalise_event_data_enums(&row.event_type, &mut data);
        crate::conformance::strip_withdrawn_data_fields(&mut data);
    }
    crate::conformance::apply_v0_migration(&row.event_type, row.turn_id.as_deref(), &mut data);
    data
}

/// Whether a stored row satisfies the v1 structural requirements for its
/// event type, judged against its prepared (migration-normalised) `data`.
/// Rows ingested on the v1 line always do (ingest rejects violations);
/// pre-v1 rows that still do not after normalisation - an engagement
/// without a `presentation_id`, a citation without an `output_id` - are
/// quarantined under `extensions.events` so the materialised document stays
/// valid against the v1 schema.
fn meets_v1_structure(row: &EventRow, prepared_data: &Value) -> bool {
    crate::conformance::v1_structural_violation(
        &row.event_type,
        true,
        row.output_id.as_deref(),
        row.presentation_id.is_some(),
        row.content_url.as_deref(),
        row.content_id.as_deref(),
        prepared_data,
    )
    .is_none()
}

fn insert_if_some(obj: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value
        && !v.is_null()
    {
        obj.insert(key.to_string(), v);
    }
}

fn standard_event(row: &EventRow, prepared_data: &Value) -> Value {
    let mut event = Map::new();
    event.insert("id".to_string(), json!(row.id));
    event.insert("type".to_string(), json!(row.event_type));
    event.insert("timestamp".to_string(), json!(row.event_timestamp));
    insert_if_some(&mut event, "turn_id", row.turn_id.clone().map(Value::from));
    insert_if_some(
        &mut event,
        "output_id",
        row.output_id.clone().map(Value::from),
    );
    insert_if_some(
        &mut event,
        "output_element_id",
        row.output_element_id.clone().map(Value::from),
    );
    insert_if_some(&mut event, "citation_id", row.citation_id.map(|v| json!(v)));
    insert_if_some(
        &mut event,
        "presentation_id",
        row.presentation_id.map(|v| json!(v)),
    );
    insert_if_some(
        &mut event,
        "source_role",
        row.source_role.clone().map(Value::from),
    );
    insert_if_some(
        &mut event,
        "content_telemetry_id",
        row.content_telemetry_id.map(|v| json!(v)),
    );
    insert_if_some(
        &mut event,
        "content_url",
        row.content_url.clone().map(Value::from),
    );
    insert_if_some(
        &mut event,
        "content_id",
        row.content_id.clone().map(Value::from),
    );
    insert_if_some(
        &mut event,
        "license_ref",
        row.license_ref.clone().map(Value::from),
    );
    insert_if_some(&mut event, "turn", row.turn_data.clone());
    if !prepared_data.is_null() {
        // Rows stored before ingest normalised closed enums can still carry
        // out-of-set values, and rows stored before versioned ingest can
        // lack the members v1 requires; `prepared_event_data` normalised,
        // migrated (spec 12.1) and stripped (spec 9.1) them on read so the
        // materialised document always validates.
        event.insert("data".to_string(), prepared_data.clone());
    }
    // product_id is an extension field; carry it inside data where the
    // schema permits custom members, not as a top-level event field.
    if let Some(product_id) = row.product_id
        && let Some(data) = event
            .entry("data".to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
    {
        data.insert("product_id".to_string(), json!(product_id));
    }
    Value::Object(event)
}

/// Build the standard session document for a stored session.
///
/// The result validates against `telemetry-session.json`: core event types
/// only in `events`, extensions namespaced under `extensions`, and
/// `conformance_level` included only when it is a standard value
/// (non-standard values travel in `extensions` instead, since the schema
/// closes that enum).
pub fn standard_document(swe: &SessionWithEvents) -> Value {
    let session = &swe.session;

    let mut doc = Map::new();
    doc.insert("document_type".to_string(), json!("session"));
    // The document this module builds is a v1 session document: stored rows
    // are read under the spec 12.1 migration rules (scope and citation_type
    // defaults, bot_category as purpose) and rows that still do not satisfy
    // the v1 shape are quarantined under `extensions.events`, so the stamp
    // is truthful after normalisation.
    doc.insert("schema_version".to_string(), json!("1.0"));
    doc.insert("session_id".to_string(), json!(session.id));
    insert_if_some(
        &mut doc,
        "parent_session_id",
        session.parent_session_id.map(|v| json!(v)),
    );
    insert_if_some(
        &mut doc,
        "agent_id",
        session.agent_id.clone().map(Value::from),
    );
    insert_if_some(
        &mut doc,
        "content_scope",
        session.content_scope.clone().map(Value::from),
    );
    insert_if_some(
        &mut doc,
        "manifest_ref",
        session.manifest_ref.clone().map(Value::from),
    );
    doc.insert("started_at".to_string(), json!(session.started_at));
    insert_if_some(&mut doc, "ended_at", session.ended_at.map(|v| json!(v)));

    let mut extensions = Map::new();

    // Normalise on read as well as ingest: rows written by a pre-rename
    // binary during the 0011 deploy window can still carry the legacy
    // 'attribution' value.
    let conformance =
        crate::conformance::normalise_conformance_level(session.conformance_level.as_deref());
    match conformance.value {
        Some(level) if !conformance.non_standard => {
            doc.insert("conformance_level".to_string(), json!(level));
        }
        Some(level) => {
            extensions.insert("conformance_level".to_string(), json!(level));
        }
        None => {}
    }

    type Prepared<'a> = Vec<(&'a EventRow, Value)>;
    let (core_events, extension_events): (Prepared<'_>, Prepared<'_>) = swe
        .events
        .iter()
        .map(|e| (e, prepared_event_data(e)))
        .partition(|(e, data)| {
            CORE_EVENT_TYPES.contains(&e.event_type.as_str()) && meets_v1_structure(e, data)
        });

    doc.insert(
        "events".to_string(),
        Value::Array(
            core_events
                .iter()
                .map(|(e, data)| standard_event(e, data))
                .collect(),
        ),
    );

    if !extension_events.is_empty() {
        extensions.insert(
            "events".to_string(),
            Value::Array(
                extension_events
                    .iter()
                    .map(|(e, data)| standard_event(e, data))
                    .collect(),
            ),
        );
    }

    // Implementation extensions, never top-level standard fields.
    extensions.insert("initiator_type".to_string(), json!(session.initiator_type));
    insert_if_some(&mut extensions, "initiator", session.initiator.clone());
    insert_if_some(
        &mut extensions,
        "external_session_id",
        session.external_session_id.clone().map(Value::from),
    );
    if let Some(prior) = &session.prior_session_ids
        && !prior.is_empty()
    {
        extensions.insert("prior_session_ids".to_string(), json!(prior));
    }
    insert_if_some(
        &mut extensions,
        "outcome_type",
        session.outcome_type.clone().map(Value::from),
    );
    insert_if_some(
        &mut extensions,
        "outcome_value",
        session.outcome_value.clone(),
    );
    insert_if_some(
        &mut extensions,
        "platform_id",
        session.platform_id.clone().map(Value::from),
    );
    insert_if_some(
        &mut extensions,
        "client_type",
        session.client_type.clone().map(Value::from),
    );
    insert_if_some(&mut extensions, "client_info", session.client_info.clone());

    doc.insert("extensions".to_string(), Value::Object(extensions));

    Value::Object(doc)
}
