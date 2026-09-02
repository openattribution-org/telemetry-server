//! Ingest policy.
//!
//! `content-telemetry-core::conformance` decides what the *standard* says
//! about an event: whether a schema version is acceptable, whether a v1
//! event carries the identifiers its type requires, which enum values are
//! recognised. This module decides what this *deployment* will accept:
//! batch sizes, how stale a timestamp may be, which emitters may skip a
//! session. Those are operator choices, and the split is deliberate — the
//! spec's rules are in the library, ours are here.

use chrono::{DateTime, Duration, Utc};
use content_telemetry_core::conformance;
use content_telemetry_core::models::event::TelemetryEventInput;

use crate::error::ApiError;

/// Events accepted in a single request.
pub const MAX_BATCH_SIZE: usize = 500;

/// How far into the future a timestamp may sit before we treat it as a
/// broken clock rather than clock drift.
pub const MAX_EVENT_SKEW_MINUTES: i64 = 5;

pub const VALID_SOURCE_ROLES: &[&str] = &["origin", "index", "edge", "agent"];

/// Roles permitted to emit without a session. Retrieval-level observers see
/// a fetch and nothing else — there is no interaction for them to open a
/// session over, and they correlate via `content_telemetry_id` instead
/// (specification section 5.7).
pub const SESSIONLESS_ROLES: &[&str] = &["origin", "index", "edge"];

/// What a sessionless emitter may report. An origin can observe a retrieval,
/// and a landing page can observe an engagement; anything about how content
/// was used inside a response requires the agent's session.
pub const SESSIONLESS_EVENT_TYPES: &[&str] = &["content_retrieved", "content_engaged"];

/// Extension event types are permitted (specification section 5.3), so this
/// is a token sanity check rather than a vocabulary check. Rejecting an
/// unknown type here would break the spec's extension mechanism.
pub fn event_type_acceptable(event_type: &str) -> bool {
    (1..=64).contains(&event_type.len())
        && event_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn check_timestamp(
    timestamp: DateTime<Utc>,
    max_event_age_days: i64,
    index: usize,
) -> Result<(), ApiError> {
    let now = Utc::now();

    if timestamp > now + Duration::minutes(MAX_EVENT_SKEW_MINUTES) {
        return Err(ApiError::bad_request(format!(
            "event {index}: timestamp is more than {MAX_EVENT_SKEW_MINUTES} minutes in the future"
        )));
    }

    if timestamp < now - Duration::days(max_event_age_days) {
        return Err(ApiError::bad_request(format!(
            "event {index}: timestamp is older than the {max_event_age_days}-day ingest window"
        )));
    }

    Ok(())
}

/// Structural checks that apply to every event regardless of how it binds to
/// a session. `line` is the schema line the enclosing document declared:
/// the full v1 structural rules bind documents on the `"1.0"` line (and
/// undeclared documents, read as current), while `"0.1"` documents are
/// normalised per spec 12.1 by [`normalise`] and tolerated where no
/// migration rule exists — the transition posture for live v0.1 emitters.
pub fn check_event(
    event: &TelemetryEventInput,
    line: conformance::SchemaLine,
    max_event_age_days: i64,
    index: usize,
) -> Result<(), ApiError> {
    if !event_type_acceptable(&event.event_type) {
        return Err(ApiError::bad_request(format!(
            "event {index}: type must be 1-64 characters of letters, digits, '_', '.' or '-'"
        )));
    }

    // v1 withdrew content_displayed (specification section 12.1). Stored v0.1
    // rows keep their type, but nothing new is accepted under it: the
    // replacement type distinguishes presenting content from presenting a
    // reference to it via data.presentation_kind, and rewriting one into the
    // other would manufacture a claim the emitter never made.
    if event.event_type == conformance::WITHDRAWN_EVENT_TYPE_DISPLAYED {
        return Err(ApiError::bad_request(format!(
            "event {index}: '{}' was withdrawn in v1 — use content_presented",
            conformance::WITHDRAWN_EVENT_TYPE_DISPLAYED
        )));
    }

    if let Some(role) = event.source_role.as_deref()
        && !VALID_SOURCE_ROLES.contains(&role)
    {
        return Err(ApiError::bad_request(format!(
            "event {index}: source_role must be one of {}",
            VALID_SOURCE_ROLES.join(", ")
        )));
    }

    check_timestamp(event.timestamp, max_event_age_days, index)?;

    if !conformance::content_identifier_present(
        &event.event_type,
        event.content_url.as_deref(),
        event.content_id.as_deref(),
    ) {
        return Err(ApiError::bad_request(format!(
            "event {index}: {} events require content_url or content_id",
            event.event_type
        )));
    }

    // The v1 structural rules bind the "1.0" line only. A "0.1" document
    // has already been normalised per spec 12.1 where a rule exists; what
    // the preview line tolerated beyond that stays tolerated at ingest and
    // is quarantined at materialisation instead.
    if line == conformance::SchemaLine::V1_0 {
        if let Some(violation) = conformance::v1_structural_violation(
            &event.event_type,
            event.id.is_some(),
            event.output_id.as_deref(),
            event.presentation_id.is_some(),
            event.content_url.as_deref(),
            event.content_id.as_deref(),
            &event.data,
        ) {
            return Err(ApiError::bad_request(format!("event {index}: {violation}")));
        }

        if let Some(violation) =
            conformance::source_role_violation(&event.event_type, event.source_role.as_deref())
        {
            return Err(ApiError::bad_request(format!("event {index}: {violation}")));
        }

        if let Some(violation) = conformance::field_placement_violation(
            &event.event_type,
            event.presentation_id.is_some(),
            event.ctx_token.is_some(),
            event.citation_id.is_some(),
            event.turn.is_some(),
        ) {
            return Err(ApiError::bad_request(format!("event {index}: {violation}")));
        }
    }

    Ok(())
}

/// Extra conditions on an event that arrives with no session to bind to.
pub fn check_sessionless(event: &TelemetryEventInput, index: usize) -> Result<(), ApiError> {
    let Some(role) = event.source_role.as_deref() else {
        return Err(ApiError::bad_request(format!(
            "event {index}: an event without a session must declare a source_role"
        )));
    };

    if !SESSIONLESS_ROLES.contains(&role) {
        return Err(ApiError::bad_request(format!(
            "event {index}: source_role '{role}' must supply a session_id or ctx_token"
        )));
    }

    if !SESSIONLESS_EVENT_TYPES.contains(&event.event_type.as_str()) {
        return Err(ApiError::bad_request(format!(
            "event {index}: sessionless emitters may only report {}",
            SESSIONLESS_EVENT_TYPES.join(" or ")
        )));
    }

    Ok(())
}

/// Apply the spec's normalisations in place: fold recognised enum synonyms,
/// drop fields v1 withdrew on privacy grounds, and — for documents on the
/// `"0.1"` line — apply the spec 12.1 migration defaults so preview events
/// read as the v1 events the migration rules define. Runs before
/// [`check_event`], so a migrated `"0.1"` event passes the checks its
/// defaults satisfy. Returns the notes worth logging so an emitter can be
/// told what was changed.
pub fn normalise(event: &mut TelemetryEventInput, line: conformance::SchemaLine) -> Vec<String> {
    let mut notes = conformance::normalise_event_data_enums(&event.event_type, &mut event.data);

    if line == conformance::SchemaLine::V0_1 {
        notes.extend(conformance::apply_v0_migration(
            &event.event_type,
            event.turn_id.as_deref(),
            &mut event.data,
        ));
    }

    notes.extend(conformance::strip_withdrawn_data_fields(&mut event.data));

    if let Some(turn) = event.turn.as_mut() {
        notes.extend(conformance::strip_turn_privacy_violations(turn));
    }

    notes
}
