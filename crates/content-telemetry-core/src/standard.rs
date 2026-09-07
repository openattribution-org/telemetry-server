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
/// into presentation claims the emitter never made. The same applies to
/// `content_reproduced`, which existed only on the pre-release v1-draft
/// line and is not part of v1 (spec 12.1): stored draft rows self-
/// quarantine here rather than being rewritten.
const CORE_EVENT_TYPES: &[&str] = &[
    "content_retrieved",
    "content_grounded",
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
        && crate::conformance::source_role_violation(&row.event_type, row.source_role.as_deref())
            .is_none()
        && crate::conformance::field_placement_violation(
            &row.event_type,
            row.presentation_id.is_some(),
            row.ctx_token.is_some(),
            row.citation_id.is_some(),
            row.turn_data.is_some(),
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
    // The event-level ctx_token belongs on content_engaged only (spec 5.2,
    // 5.7.5): the agent records the token minted for the engaged
    // presentation so destination reports join to it.
    if row.event_type == "content_engaged" {
        insert_if_some(
            &mut event,
            "ctx_token",
            row.ctx_token.clone().map(Value::from),
        );
    }
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
    // terms_ref passes through byte-for-byte (spec 5.2.4): a processor MUST
    // preserve it unchanged and MUST NOT remove or rewrite it.
    insert_if_some(
        &mut event,
        "terms_ref",
        row.terms_ref.clone().map(Value::from),
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
    // The session-level data container (spec 5.1.3), served back exactly as
    // it arrived. Consumers MUST tolerate unknown fields within it and
    // unknown `access_context` identifier schemes, so nothing inside is
    // normalised, defaulted or dropped: the round trip is lossless.
    insert_if_some(&mut doc, "data", session.session_data.clone());

    let mut extensions = Map::new();

    // Top-level members this server does not define. The session root is not
    // an extension point (spec 5.1.3) and they are not interpreted, but a
    // consumer MUST tolerate unknown fields without error (spec 5.7.4), so
    // they are returned rather than dropped — a document cannot lose data
    // here without the loss being visible.
    if let Some(Value::Object(unrecognised)) = session.unrecognised_fields.as_ref()
        && !unrecognised.is_empty()
    {
        extensions.insert(
            "unrecognised_fields".to_string(),
            Value::Object(unrecognised.clone()),
        );
    }

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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::models::session::SessionRow;

    fn session_row() -> SessionRow {
        SessionRow {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            parent_session_id: None,
            initiator_type: "user".to_string(),
            initiator: None,
            content_scope: None,
            manifest_ref: None,
            conformance_level: Some("citation".to_string()),
            config_snapshot_hash: None,
            agent_id: Some("test-agent".to_string()),
            external_session_id: None,
            prior_session_ids: None,
            session_data: None,
            unrecognised_fields: None,
            user_context: json!({}),
            platform_id: None,
            client_type: None,
            client_info: None,
            started_at: Utc::now(),
            ended_at: None,
            outcome_type: None,
            outcome_value: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn event_row(event_type: &str, data: Value) -> EventRow {
        EventRow {
            id: Uuid::new_v4(),
            session_id: None,
            organization_id: Uuid::new_v4(),
            event_type: event_type.to_string(),
            source_role: Some("agent".to_string()),
            content_telemetry_id: None,
            content_url: Some("https://example.com/a".to_string()),
            content_id: None,
            turn_id: None,
            output_id: None,
            output_element_id: None,
            citation_id: None,
            presentation_id: None,
            ctx_token: None,
            license_ref: None,
            terms_ref: None,
            product_id: None,
            turn_data: None,
            event_data: data,
            event_timestamp: Utc::now(),
            created_at: Utc::now(),
        }
    }

    fn event_types(doc: &Value, pointer: &str) -> Vec<String> {
        doc.pointer(pointer)
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|e| e.get("type").and_then(Value::as_str))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn withdrawn_types_self_quarantine_into_extensions() {
        // A stored pre-release content_reproduced row and a stored v0.1
        // content_displayed row both fall outside CORE_EVENT_TYPES, so the
        // partition moves them under extensions.events untouched (spec 12.1)
        // while conforming core rows stay in the document body.
        let swe = SessionWithEvents {
            session: session_row(),
            events: vec![
                event_row("content_grounded", json!({ "scope": "session" })),
                event_row(
                    "content_reproduced",
                    json!({ "reproduction_type": "verbatim", "output_id": "out-1" }),
                ),
                event_row("content_displayed", json!({ "display_type": "link" })),
                event_row("checkout_completed", json!({})),
            ],
        };

        let doc = standard_document(&swe);
        assert_eq!(doc["schema_version"], "1.0");
        assert_eq!(event_types(&doc, "/events"), vec!["content_grounded"]);

        let quarantined = event_types(&doc, "/extensions/events");
        assert_eq!(
            quarantined,
            vec![
                "content_reproduced",
                "content_displayed",
                "checkout_completed"
            ]
        );

        // Quarantined rows keep the claims their emitters made.
        let reproduced = &doc["extensions"]["events"][0];
        assert_eq!(reproduced["data"]["reproduction_type"], "verbatim");
    }

    #[test]
    fn ctx_token_and_terms_ref_materialise_where_the_spec_places_them() {
        let mut engaged = event_row(
            "content_engaged",
            json!({ "engagement_type": "link_click" }),
        );
        engaged.presentation_id = Some(Uuid::new_v4());
        engaged.ctx_token = Some("ct_dGVzdHRva2VudmFsdWU".to_string());
        engaged.terms_ref = Some("https://example.com/terms/2026-01".to_string());

        let mut grounded = event_row("content_grounded", json!({ "scope": "session" }));
        grounded.terms_ref = Some("opaque:terms-77".to_string());

        // A pre-v1-tolerated row with a ctx_token on the wrong type is
        // quarantined by the field-placement rule, never emitted as core.
        let mut misplaced = event_row("content_grounded", json!({ "scope": "session" }));
        misplaced.ctx_token = Some("ct_bWlzcGxhY2VkdG9rZW4".to_string());

        let swe = SessionWithEvents {
            session: session_row(),
            events: vec![engaged, grounded, misplaced],
        };
        let doc = standard_document(&swe);

        assert_eq!(
            event_types(&doc, "/events"),
            vec!["content_engaged", "content_grounded"]
        );
        assert_eq!(doc["events"][0]["ctx_token"], "ct_dGVzdHRva2VudmFsdWU");
        // terms_ref passes through byte-for-byte on any event (spec 5.2.4).
        assert_eq!(
            doc["events"][0]["terms_ref"],
            "https://example.com/terms/2026-01"
        );
        assert_eq!(doc["events"][1]["terms_ref"], "opaque:terms-77");
        assert_eq!(
            event_types(&doc, "/extensions/events"),
            vec!["content_grounded"]
        );
    }

    #[test]
    fn session_data_materialises_at_the_document_root() {
        // The container is served back byte-for-byte, unknown identifier
        // schemes and namespaced neighbours included (spec 5.1.3).
        let data = json!({
            "access_context": {
                "identifiers": [
                    { "scheme": "ror", "value": "https://ror.org/013meh722" },
                    { "scheme": "example_local", "value": "lib-4471", "note": "consortium seat" }
                ]
            },
            "com.example.reporting_period": "2026-08"
        });

        let mut session = session_row();
        session.session_data = Some(data.clone());

        let doc = standard_document(&SessionWithEvents {
            session,
            events: vec![],
        });

        assert_eq!(doc["data"], data);
        assert_eq!(
            doc["data"]["access_context"]["identifiers"][1]["scheme"],
            "example_local"
        );
    }

    #[test]
    fn undefined_top_level_fields_come_back_under_extensions() {
        // The session root is not an extension point (spec 5.1.3), but a
        // consumer MUST tolerate unknown fields without error (spec 5.7.4).
        // Recording them is what makes the loss visible: a member this
        // server does not implement is returned rather than dropped.
        let mut session = session_row();
        session.unrecognised_fields = Some(json!({ "access_summary": { "institutions": 3 } }));

        let doc = standard_document(&SessionWithEvents {
            session,
            events: vec![],
        });

        assert!(
            doc.get("access_summary").is_none(),
            "an undefined member must not be promoted to a standard field"
        );
        assert_eq!(
            doc["extensions"]["unrecognised_fields"]["access_summary"]["institutions"],
            3
        );
    }

    #[test]
    fn stored_v0_rows_materialise_under_the_migration_rules() {
        // Rows written before versioned ingest lack the members v1 requires;
        // the spec 12.1 defaults are applied on read so the document body
        // keeps them rather than quarantining history (and the "1.0" stamp
        // stays truthful).
        let mut cited = event_row("content_cited", json!({}));
        cited.output_id = Some("response:1".to_string());
        let mut grounded_turn = event_row("content_grounded", json!({}));
        grounded_turn.turn_id = Some("turn-1".to_string());

        let swe = SessionWithEvents {
            session: session_row(),
            events: vec![
                event_row("content_grounded", json!({})),
                grounded_turn,
                cited,
            ],
        };

        let doc = standard_document(&swe);
        assert_eq!(
            event_types(&doc, "/events"),
            vec!["content_grounded", "content_grounded", "content_cited"]
        );
        assert_eq!(doc["events"][0]["data"]["scope"], "session");
        assert_eq!(doc["events"][1]["data"]["scope"], "turn");
        assert_eq!(doc["events"][2]["data"]["citation_type"], "unclassified");
    }
}
