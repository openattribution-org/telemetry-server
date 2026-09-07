//! Content Telemetry v1 conformance rules.
//!
//! Application-layer rules from the specification that JSON Schema cannot
//! express (spec section 5.7.5), plus the value sets the standard defines.
//! Pure functions only — callers decide how to log or surface the flags
//! these return.

use serde_json::Value;

/// Schema versions this consumer accepts. `"1.0"` is the version this
/// implementation targets. `"0.1"` remains accepted for the transition:
/// live member edge workers still declare it, and spec 12.1 gives a
/// consumer explicit migration rules for reading preview documents
/// (`bot_category` as `purpose`, defaulted `scope` and `citation_type`).
/// Under the spec's own rule the two lines do not interoperate — a strict
/// v1 consumer rejects `"0.1"` — so accepting both is a deliberate,
/// temporary deployment choice, not the 5.7.4 default. Documents declaring
/// `"1.0"` get full v1 strictness; documents declaring `"0.1"` are
/// normalised per 12.1 where a rule exists and tolerated otherwise.
pub const ACCEPTED_SCHEMA_VERSIONS: &[&str] = &["0.1", "1.0"];

/// The two schema lines this consumer reads. Which line a document declares
/// decides how much strictness applies at ingest: `V1_0` documents are held
/// to the v1 structural rules, `V0_1` documents are migrated per spec 12.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaLine {
    /// The v0.1 preview line, read under the migration rules of spec 12.1.
    V0_1,
    /// The v1.0 line, held to the full v1 structural rules.
    V1_0,
}

/// Which schema line a declared `schema_version` selects, or None when the
/// version is not accepted at all. Absent versions read as the current line:
/// the field postdates the earliest envelopes, and every live v0.1 emitter
/// declares its version explicitly, so absence means a current emitter.
pub fn schema_line(version: Option<&str>) -> Option<SchemaLine> {
    match version {
        None | Some("1.0") => Some(SchemaLine::V1_0),
        Some("0.1") => Some(SchemaLine::V0_1),
        Some(_) => None,
    }
}

/// Conformance levels the standard defines (spec 5.7).
pub const STANDARD_CONFORMANCE_LEVELS: &[&str] = &["retrieval", "grounding", "citation"];

/// Event types that carry content and therefore require an identifier
/// (spec 5.7.5). Turn events and extension events are exempt.
pub const CONTENT_EVENT_TYPES: &[&str] = &[
    "content_retrieved",
    "content_grounded",
    "content_cited",
    "content_presented",
    "content_engaged",
];

/// Closed enums on event `data` members (spec Annex A). Unlike the open
/// string types (media_type, presentation_type, engagement_type,
/// query_intent), the schema closes these: an out-of-set value makes the
/// materialised session document schema-invalid.
pub const CITATION_TYPES: &[&str] = &[
    "direct_quote",
    "paraphrase",
    "reference",
    "contradiction",
    "unclassified",
];
pub const CITATION_POSITIONS: &[&str] = &["primary", "supporting", "mentioned", "unclassified"];
pub const GROUNDING_SCOPES: &[&str] = &["session", "turn"];
pub const PRESENTATION_KINDS: &[&str] = &["content", "source_reference"];

/// The event type v1 withdrew (spec 12.1). Emitters MUST NOT send it on the
/// v1 integration line; stored v0.1 rows keep it and are quarantined under
/// `extensions.events` at materialisation. `content_reproduced`, the other
/// type 12.1 excludes, existed only on the pre-release v1-draft line: it is
/// simply not in the core sets here, so stored draft rows self-quarantine
/// the same way without a named constant.
pub const WITHDRAWN_EVENT_TYPE_DISPLAYED: &str = "content_displayed";

/// V0.1 event `data` fields prohibited by the v1 migration rule (spec 9.1).
/// The schemas cannot catch these — event `data` accepts additional
/// properties by design — so the prohibition is enforced here. This is the
/// v1 transition rule, not a general registry of withdrawn extension names.
pub const WITHDRAWN_EVENT_DATA_FIELDS: &[&str] = &["ip_hash"];

/// The one container core defines inside the session-level `data` object
/// (spec 5.1.3): the context from which the session's access rights derive,
/// an institution and never an individual. COUNTER usage reporting needs it,
/// which is why it is in core rather than in an extension.
pub const SESSION_ACCESS_CONTEXT: &str = "access_context";

/// Identifier schemes named in core (spec 5.1.3). Informative only: the
/// vocabulary is open, emitters MAY use others, and consumers MUST tolerate
/// unknown ones, so nothing validates against this list.
pub const CORE_ACCESS_CONTEXT_SCHEMES: &[&str] = &["ror", "saml_entity_id", "isni"];

/// Conversation-turn privacy levels (spec 5.4). A closed enum: the schema
/// rejects a turn whose `privacy_level` is outside this set.
pub const PRIVACY_LEVELS: &[&str] = &["full", "summary", "intent", "minimal"];

/// Normalise the closed-enum members of an event's `data` object so the
/// materialised document validates against the schema (spec Annex A).
/// `citation_type` and `position` carry `unclassified` for exactly this
/// case, so unknown values map there; `scope` has no such member, so
/// unknown values are dropped (the field is optional). `media_type` is an
/// open vocabulary (core values plus emitter-defined ones, e.g. `3d`,
/// `dataset`), so it is not normalised here. Returns the names of the
/// fields changed.
pub fn normalise_event_data_enums(event_type: &str, data: &mut Value) -> Vec<String> {
    type Rule = (&'static str, &'static [&'static str], Option<&'static str>);
    let rules: &[Rule] = match event_type {
        "content_grounded" => &[("scope", GROUNDING_SCOPES, None)],
        "content_cited" => &[
            ("citation_type", CITATION_TYPES, Some("unclassified")),
            ("position", CITATION_POSITIONS, Some("unclassified")),
        ],
        _ => return Vec::new(),
    };

    let Some(obj) = data.as_object_mut() else {
        return Vec::new();
    };

    let mut changed = Vec::new();
    for (field, allowed, fallback) in rules {
        let valid = match obj.get(*field) {
            None | Some(Value::Null) => true,
            Some(Value::String(v)) => allowed.contains(&v.as_str()),
            Some(_) => false,
        };
        if !valid {
            match fallback {
                Some(f) => {
                    obj.insert((*field).to_string(), Value::String((*f).to_string()));
                }
                None => {
                    obj.remove(*field);
                }
            }
            changed.push((*field).to_string());
        }
    }
    changed
}

/// Whether a document-level `schema_version` is acceptable. Absent versions
/// are accepted and treated as the current version: the field postdates the
/// earliest envelopes, so its absence carries no information.
pub fn schema_version_accepted(version: Option<&str>) -> bool {
    schema_line(version).is_some()
}

/// Apply the spec 12.1 migration rules for reading a v0.1 preview event as
/// v1, in place, returning the names of the fields written:
///
/// - `content_cited` without `data.citation_type` reads as `unclassified`;
/// - `content_grounded` without `data.scope` reads as `turn` when the event
///   carries a `turn_id` and `session` otherwise;
/// - a `data.bot_category` value is read as `purpose` (the v1 name for the
///   field). The original member is kept so pre-rename readers still see
///   it during the transition; the read layer coalesces the two.
///
/// Everything else the preview line tolerated stays tolerated: 12.1 defines
/// no other defaulting rule, and inventing one would manufacture claims the
/// emitter never made. Used both at ingest for documents declaring `"0.1"`
/// and at materialisation for stored rows written before versioned ingest.
pub fn apply_v0_migration(
    event_type: &str,
    turn_id: Option<&str>,
    data: &mut Value,
) -> Vec<String> {
    let needs_default = matches!(event_type, "content_grounded" | "content_cited");
    if data.is_null() && needs_default {
        *data = Value::Object(serde_json::Map::new());
    }
    let Some(obj) = data.as_object_mut() else {
        return Vec::new();
    };

    let mut changed = Vec::new();
    let absent = |obj: &serde_json::Map<String, Value>, field: &str| {
        matches!(obj.get(field), None | Some(Value::Null))
    };

    match event_type {
        "content_cited" if absent(obj, "citation_type") => {
            obj.insert(
                "citation_type".to_string(),
                Value::String("unclassified".to_string()),
            );
            changed.push("citation_type".to_string());
        }
        "content_grounded" if absent(obj, "scope") => {
            let scope = if turn_id.is_some() { "turn" } else { "session" };
            obj.insert("scope".to_string(), Value::String(scope.to_string()));
            changed.push("scope".to_string());
        }
        _ => {}
    }

    if let Some(Value::String(category)) = obj.get("bot_category")
        && absent(obj, "purpose")
    {
        let category = category.clone();
        obj.insert("purpose".to_string(), Value::String(category));
        changed.push("purpose".to_string());
    }

    changed
}

/// Result of normalising an emitter-supplied conformance level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalisedConformanceLevel {
    pub value: Option<String>,
    /// True when the supplied value is outside the standard's set. The field
    /// is informational (spec 5.7), so this is a flag, never a rejection.
    pub non_standard: bool,
}

/// Normalise a conformance level: the legacy pre-publication value
/// `attribution` maps to `citation`; standard values pass through; anything
/// else passes through flagged.
pub fn normalise_conformance_level(value: Option<&str>) -> NormalisedConformanceLevel {
    match value {
        None => NormalisedConformanceLevel {
            value: None,
            non_standard: false,
        },
        Some("attribution") => NormalisedConformanceLevel {
            value: Some("citation".to_string()),
            non_standard: false,
        },
        Some(v) => NormalisedConformanceLevel {
            value: Some(v.to_string()),
            non_standard: !STANDARD_CONFORMANCE_LEVELS.contains(&v),
        },
    }
}

/// Whether a content event satisfies the identifier rule: at least one of
/// `content_url` or `content_id` on every content event (spec 5.7.5).
/// Non-content event types always pass.
pub fn content_identifier_present(
    event_type: &str,
    content_url: Option<&str>,
    content_id: Option<&str>,
) -> bool {
    if !CONTENT_EVENT_TYPES.contains(&event_type) {
        return true;
    }
    content_url.is_some_and(|v| !v.is_empty()) || content_id.is_some_and(|v| !v.is_empty())
}

/// Strip the event `data` fields v1 withdrew (spec 9.1, 12.1) in place,
/// returning the names of the fields removed. A hashed IP address is a
/// pseudonym, not an anonymous value, so `ip_hash` is treated as personal
/// data: the consumer drops it rather than storing it, on ingest and again
/// on read for rows written before this rule existed.
///
/// `content_fingerprint.preserved_in_output` is also withdrawn (spec 6.4,
/// 12.1): v1 defines no output-side reuse reporting, and a grounding
/// fingerprint is a grounding-time claim only. It lives one level down, so
/// the top-level sweep cannot catch it.
pub fn strip_withdrawn_data_fields(data: &mut Value) -> Vec<String> {
    let Some(obj) = data.as_object_mut() else {
        return Vec::new();
    };
    let mut stripped = Vec::new();
    for field in WITHDRAWN_EVENT_DATA_FIELDS {
        if obj.remove(*field).is_some() {
            stripped.push((*field).to_string());
        }
    }
    if let Some(fingerprint) = obj
        .get_mut("content_fingerprint")
        .and_then(Value::as_object_mut)
        && fingerprint.remove("preserved_in_output").is_some()
    {
        stripped.push("content_fingerprint.preserved_in_output".to_string());
    }
    stripped
}

/// The v1 structural requirements the standard's JSON Schema enforces per
/// event type (spec 5.2, 6.4-6.8): grounding carries `data.scope` (closed)
/// with `provenance` and `cached` kept consistent; citation and
/// presentation events carry an emitter-assigned `id` and an `output_id`;
/// citation carries a resolvable source reference and
/// `data.citation_type`; presentation carries `data.presentation_kind`
/// (closed) and `data.presentation_type`; engagement carries the
/// `presentation_id` of the exact presentation occurrence acted upon.
///
/// Returns a description of the first violation, or None when the event
/// satisfies the v1 shape. Ingest rejects violations; materialisation uses
/// the same rule to quarantine stored pre-v1 rows under `extensions.events`
/// so the session document stays schema-valid.
#[allow(clippy::too_many_arguments)]
pub fn v1_structural_violation(
    event_type: &str,
    id_present: bool,
    output_id: Option<&str>,
    presentation_id_present: bool,
    content_url: Option<&str>,
    content_id: Option<&str>,
    data: &Value,
) -> Option<String> {
    let data_str = |field: &str| -> Option<&str> { data.get(field).and_then(Value::as_str) };

    match event_type {
        "content_grounded" => {
            // scope is required and schema-closed (spec 6.4): the occurrence
            // boundary and every counting model depend on it.
            if !data_str("scope").is_some_and(|v| GROUNDING_SCOPES.contains(&v)) {
                return Some(format!(
                    "content_grounded events must carry data.scope of: {}",
                    GROUNDING_SCOPES.join(", ")
                ));
            }
            // provenance and cached must agree (spec 6.4): agent_fetched
            // asserts a live fetch this session, agent_cached asserts reuse.
            // third_party_sourced leaves cached unconstrained.
            let cached = data.get("cached").and_then(Value::as_bool);
            match data_str("provenance") {
                Some("agent_fetched") if cached != Some(false) => Some(
                    "content_grounded provenance 'agent_fetched' requires data.cached: false"
                        .to_string(),
                ),
                Some("agent_cached") if cached != Some(true) => Some(
                    "content_grounded provenance 'agent_cached' requires data.cached: true"
                        .to_string(),
                ),
                _ => None,
            }
        }
        "content_cited" | "content_presented" => {
            if !id_present {
                return Some(format!("{event_type} events must carry an event id"));
            }
            if !output_id.is_some_and(|v| !v.is_empty()) {
                return Some(format!("{event_type} events must carry output_id"));
            }
            if event_type == "content_cited" {
                // Schema-enforced for citations (spec 6.5), over and above
                // the application-layer identifier rule.
                if !(content_url.is_some_and(|v| !v.is_empty())
                    || content_id.is_some_and(|v| !v.is_empty()))
                {
                    return Some(format!(
                        "{event_type} events must carry a resolvable content_url or content_id"
                    ));
                }
                // citation_type is required and schema-enforced (spec 6.5);
                // an emitter that cannot classify uses 'unclassified'.
                if !data_str("citation_type").is_some_and(|v| !v.is_empty()) {
                    return Some(
                        "content_cited events must carry data.citation_type \
                         ('unclassified' when the agent cannot classify)"
                            .to_string(),
                    );
                }
            } else {
                let kind = data_str("presentation_kind");
                if !kind.is_some_and(|v| PRESENTATION_KINDS.contains(&v)) {
                    return Some(format!(
                        "content_presented events must carry data.presentation_kind of: {}",
                        PRESENTATION_KINDS.join(", ")
                    ));
                }
                if !data_str("presentation_type").is_some_and(|v| !v.is_empty()) {
                    return Some(
                        "content_presented events must carry data.presentation_type".to_string(),
                    );
                }
            }
            None
        }
        "content_engaged" => {
            if presentation_id_present {
                None
            } else {
                Some(
                    "content_engaged events must carry presentation_id, referencing the \
                     content_presented event acted upon"
                        .to_string(),
                )
            }
        }
        _ => None,
    }
}

/// The shape the schema gives the session-level `data` container's one core
/// member, `access_context` (spec 5.1.3): `identifiers` is an array, and
/// every entry is an object carrying a `scheme` and a `value`, both strings.
///
/// Only the shape is checked. Scheme values are open — `ror`,
/// `saml_entity_id` and `isni` are the core ones and a consumer MUST
/// tolerate any other — and additional members inside `data`, inside
/// `access_context` and inside an identifier are tolerated unchanged,
/// because consumers MUST tolerate unknown fields within the container.
///
/// Returns a description of the first violation, or None.
pub fn session_data_violation(data: Option<&Value>) -> Option<String> {
    let data = data?;
    if data.is_null() {
        return None;
    }
    let Some(data) = data.as_object() else {
        return Some("session data must be an object".to_string());
    };

    let access_context = data.get(SESSION_ACCESS_CONTEXT)?;
    if access_context.is_null() {
        return None;
    }
    let Some(access_context) = access_context.as_object() else {
        return Some("data.access_context must be an object".to_string());
    };

    let identifiers = access_context.get("identifiers")?;
    let Some(identifiers) = identifiers.as_array() else {
        return Some(
            "data.access_context.identifiers must be an array of {scheme, value} objects"
                .to_string(),
        );
    };

    for (index, identifier) in identifiers.iter().enumerate() {
        let Some(identifier) = identifier.as_object() else {
            return Some(format!(
                "data.access_context.identifiers[{index}] must be an object"
            ));
        };
        for member in ["scheme", "value"] {
            match identifier.get(member) {
                Some(v) if v.is_string() => {}
                Some(_) => {
                    return Some(format!(
                        "data.access_context.identifiers[{index}].{member} must be a string"
                    ));
                }
                None => {
                    return Some(format!(
                        "data.access_context.identifiers[{index}] requires {member}"
                    ));
                }
            }
        }
    }

    None
}

/// The v1 rule that `source_role` MUST be present on every
/// `content_retrieved` event (spec 5.2.2, 5.7.5): without it a consumer
/// cannot tell an agent-reported fetch from an origin- or edge-reported
/// one. Kept separate from `v1_structural_violation` because `source_role`
/// is an event-level field, not a `data` member.
pub fn source_role_violation(event_type: &str, source_role: Option<&str>) -> Option<String> {
    if event_type == "content_retrieved" && !source_role.is_some_and(|v| !v.is_empty()) {
        Some("content_retrieved events must carry source_role".to_string())
    } else {
        None
    }
}

/// The v1 field-placement rules (spec 5.7.5): fields scoped to one event
/// type MUST NOT appear on others. `presentation_id` and the event-level
/// `ctx_token` belong only on `content_engaged`; `citation_id` only on
/// `content_presented`; `turn` only on `turn_started` and `turn_completed`.
/// Returns a description of the first misplaced field, or None.
pub fn field_placement_violation(
    event_type: &str,
    presentation_id_present: bool,
    ctx_token_present: bool,
    citation_id_present: bool,
    turn_present: bool,
) -> Option<String> {
    if event_type != "content_engaged" {
        if presentation_id_present {
            return Some(format!(
                "presentation_id may only appear on content_engaged events, not {event_type}"
            ));
        }
        if ctx_token_present {
            return Some(format!(
                "an event-level ctx_token may only appear on content_engaged events, not \
                 {event_type}"
            ));
        }
    }
    if citation_id_present && event_type != "content_presented" {
        return Some(format!(
            "citation_id may only appear on content_presented events, not {event_type}"
        ));
    }
    if turn_present && !matches!(event_type, "turn_started" | "turn_completed") {
        return Some(format!(
            "turn may only appear on turn_started and turn_completed events, not {event_type}"
        ));
    }
    None
}

/// Conversation-turn fields that MUST NOT be present at each privacy level
/// (spec 5.5). `minimal` keeps only token counts and content URL arrays;
/// `intent` strips the raw query/response text. PrivacyLevel is a closed
/// enum, so anything outside it fails closed to `minimal`: an
/// unrecognised value must never grant more visibility than the most
/// restrictive level.
fn forbidden_turn_fields(privacy_level: &str) -> &'static [&'static str] {
    match privacy_level {
        "full" | "summary" => &[],
        "intent" => &["query_text", "response_text"],
        _ => &[
            "query_text",
            "response_text",
            "query_intent",
            "topics",
            "response_type",
            "response_mode",
            "model_id",
            "ad_rendered",
        ],
    }
}

/// Strip privacy-violating fields from a conversation turn in place,
/// returning the names of the fields removed. A consumer that receives a
/// privacy-violating turn strips the offending fields rather than rejecting
/// the document carrying them (spec 5.7.5). Null-valued fields are not
/// violations; only populated fields are stripped.
///
/// `privacy_level` is required on every turn (spec 5.4) and is a closed
/// enum. A turn without one, or whose value is outside the enum (wrong
/// case, stray whitespace, non-string), is stripped as `minimal` and its
/// `privacy_level` rewritten to `minimal`: leaving the offending value in
/// place would make the materialised session document schema-invalid.
pub fn strip_turn_privacy_violations(turn: &mut Value) -> Vec<String> {
    let Some(obj) = turn.as_object_mut() else {
        return Vec::new();
    };
    let level = obj
        .get("privacy_level")
        .and_then(Value::as_str)
        .filter(|v| PRIVACY_LEVELS.contains(v))
        .map(ToString::to_string);

    let forbidden = forbidden_turn_fields(level.as_deref().unwrap_or("minimal"));
    let mut stripped = Vec::new();
    for field in forbidden {
        if obj.get(*field).is_some_and(|v| !v.is_null()) {
            obj.remove(*field);
            stripped.push((*field).to_string());
        }
    }
    if level.is_none() {
        obj.insert(
            "privacy_level".to_string(),
            Value::String("minimal".to_string()),
        );
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn closed_enums_with_unclassified_normalise_to_it() {
        let mut data = json!({
            "citation_type": "weird_unknown_type",
            "position": "primary",
            "excerpt_tokens": 12
        });
        let changed = normalise_event_data_enums("content_cited", &mut data);
        assert_eq!(changed, vec!["citation_type"]);
        assert_eq!(data["citation_type"], "unclassified");
        assert_eq!(data["position"], "primary");
        assert_eq!(data["excerpt_tokens"], 12);
    }

    #[test]
    fn closed_enums_without_unclassified_drop_unknown_values() {
        let mut data = json!({
            "scope": "paragraph",
            "cached": true
        });
        let changed = normalise_event_data_enums("content_grounded", &mut data);
        assert_eq!(changed, vec!["scope"]);
        assert!(data.get("scope").is_none());
        assert_eq!(data["cached"], true);
    }

    #[test]
    fn media_type_is_an_open_vocabulary() {
        // media_type tolerates emitter-defined values beyond the core set
        // (spec Annex A): unknown values pass through untouched.
        for event_type in ["content_retrieved", "content_grounded", "content_cited"] {
            let mut data = json!({ "media_type": "dataset" });
            assert!(
                normalise_event_data_enums(event_type, &mut data).is_empty(),
                "{event_type} normalised an open media_type value"
            );
            assert_eq!(data["media_type"], "dataset");
        }
    }

    #[test]
    fn non_string_closed_enum_values_are_normalised_too() {
        let mut data = json!({ "scope": 3, "citation_type": ["a"] });
        let changed = normalise_event_data_enums("content_grounded", &mut data);
        assert_eq!(changed, vec!["scope"]);
        assert!(data.get("scope").is_none());

        let mut data = json!({ "citation_type": ["a"] });
        let changed = normalise_event_data_enums("content_cited", &mut data);
        assert_eq!(changed, vec!["citation_type"]);
        assert_eq!(data["citation_type"], "unclassified");
    }

    #[test]
    fn standard_values_and_other_event_types_left_alone() {
        let mut data = json!({ "scope": "turn", "media_type": "text" });
        assert!(normalise_event_data_enums("content_grounded", &mut data).is_empty());
        assert_eq!(data["scope"], "turn");

        // turn events and extension events carry no closed data enums.
        let mut data = json!({ "citation_type": "nonsense" });
        assert!(normalise_event_data_enums("turn_completed", &mut data).is_empty());
        assert_eq!(data["citation_type"], "nonsense");

        // null and absent members are not violations.
        let mut data = json!({ "scope": null });
        assert!(normalise_event_data_enums("content_grounded", &mut data).is_empty());
    }

    #[test]
    fn schema_version_accepts_v1_and_transitional_v0() {
        // "1.0" is the implemented version; absent reads as current.
        assert!(schema_version_accepted(None));
        assert!(schema_version_accepted(Some("1.0")));
        // "0.1" stays accepted during the transition: live member edge
        // workers still declare it, and spec 12.1 defines how a consumer
        // reads preview documents. This is deliberately more lenient than
        // the spec's non-interoperation rule and is removed once every
        // emitter declares "1.0".
        assert!(schema_version_accepted(Some("0.1")));
        assert!(!schema_version_accepted(Some("0.2")));
        assert!(!schema_version_accepted(Some("1.1")));
        assert!(!schema_version_accepted(Some("2.0")));
    }

    #[test]
    fn schema_line_maps_versions_to_strictness() {
        assert_eq!(schema_line(None), Some(SchemaLine::V1_0));
        assert_eq!(schema_line(Some("1.0")), Some(SchemaLine::V1_0));
        assert_eq!(schema_line(Some("0.1")), Some(SchemaLine::V0_1));
        assert_eq!(schema_line(Some("9.9")), None);
    }

    #[test]
    fn v0_migration_defaults_citation_type_to_unclassified() {
        let mut data = json!({ "excerpt_chars": 90 });
        let changed = apply_v0_migration("content_cited", None, &mut data);
        assert_eq!(changed, vec!["citation_type"]);
        assert_eq!(data["citation_type"], "unclassified");
        assert_eq!(data["excerpt_chars"], 90);

        // A supplied value is never overwritten.
        let mut data = json!({ "citation_type": "direct_quote" });
        assert!(apply_v0_migration("content_cited", None, &mut data).is_empty());
        assert_eq!(data["citation_type"], "direct_quote");
    }

    #[test]
    fn v0_migration_defaults_grounding_scope_by_turn_presence() {
        let mut data = json!({});
        apply_v0_migration("content_grounded", Some("turn-1"), &mut data);
        assert_eq!(data["scope"], "turn");

        let mut data = json!({});
        apply_v0_migration("content_grounded", None, &mut data);
        assert_eq!(data["scope"], "session");

        // A supplied scope is never overwritten.
        let mut data = json!({ "scope": "session" });
        assert!(apply_v0_migration("content_grounded", Some("turn-1"), &mut data).is_empty());
        assert_eq!(data["scope"], "session");

        // Null data still gains the required member.
        let mut data = Value::Null;
        apply_v0_migration("content_grounded", None, &mut data);
        assert_eq!(data["scope"], "session");
    }

    #[test]
    fn v0_migration_reads_bot_category_as_purpose() {
        let mut data = json!({ "bot_category": "inference", "bot_name": "Claude-User" });
        let changed = apply_v0_migration("content_retrieved", None, &mut data);
        assert_eq!(changed, vec!["purpose"]);
        assert_eq!(data["purpose"], "inference");
        // The original member is kept for pre-rename readers.
        assert_eq!(data["bot_category"], "inference");

        // An explicit purpose wins over the legacy field.
        let mut data = json!({ "bot_category": "training", "purpose": "search" });
        assert!(apply_v0_migration("content_retrieved", None, &mut data).is_empty());
        assert_eq!(data["purpose"], "search");
    }

    #[test]
    fn conformance_level_maps_legacy_attribution_to_citation() {
        let n = normalise_conformance_level(Some("attribution"));
        assert_eq!(n.value.as_deref(), Some("citation"));
        assert!(!n.non_standard);
    }

    #[test]
    fn conformance_level_passes_standard_values() {
        for v in STANDARD_CONFORMANCE_LEVELS {
            let n = normalise_conformance_level(Some(v));
            assert_eq!(n.value.as_deref(), Some(*v));
            assert!(!n.non_standard);
        }
    }

    #[test]
    fn conformance_level_flags_unknown_values_without_rejecting() {
        let n = normalise_conformance_level(Some("platinum"));
        assert_eq!(n.value.as_deref(), Some("platinum"));
        assert!(n.non_standard);
    }

    #[test]
    fn withdrawn_reproduced_type_carries_no_rules() {
        // content_reproduced existed only on the pre-release v1-draft line
        // (spec 12.1). It is an extension type now: no enum normalisation
        // applies, and no v1 structural requirement recognises it.
        let mut data = json!({ "reproduction_type": "loose_paraphrase" });
        assert!(normalise_event_data_enums("content_reproduced", &mut data).is_empty());
        assert_eq!(data["reproduction_type"], "loose_paraphrase");
        assert!(!CONTENT_EVENT_TYPES.contains(&"content_reproduced"));
        assert!(
            v1_structural_violation("content_reproduced", false, None, false, None, None, &data)
                .is_none()
        );
    }

    #[test]
    fn withdrawn_ip_hash_is_stripped() {
        let mut data = json!({
            "ip_hash": "sha256:d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5",
            "country": "US"
        });
        assert_eq!(strip_withdrawn_data_fields(&mut data), vec!["ip_hash"]);
        assert!(data.get("ip_hash").is_none());
        assert_eq!(data["country"], "US");

        let mut clean = json!({ "country": "US" });
        assert!(strip_withdrawn_data_fields(&mut clean).is_empty());
    }

    #[test]
    fn v1_structure_requires_output_identity_on_response_layer_events() {
        for event_type in ["content_cited", "content_presented"] {
            let data = match event_type {
                "content_presented" => {
                    json!({ "presentation_kind": "source_reference", "presentation_type": "link" })
                }
                _ => json!({ "citation_type": "direct_quote" }),
            };
            assert!(
                v1_structural_violation(
                    event_type,
                    true,
                    Some("response:1"),
                    false,
                    Some("https://example.com/a"),
                    None,
                    &data
                )
                .is_none(),
                "{event_type} rejected a conforming event"
            );
            assert!(
                v1_structural_violation(
                    event_type,
                    true,
                    None,
                    false,
                    Some("https://example.com/a"),
                    None,
                    &data
                )
                .is_some(),
                "{event_type} accepted a missing output_id"
            );
            assert!(
                v1_structural_violation(
                    event_type,
                    false,
                    Some("response:1"),
                    false,
                    Some("https://example.com/a"),
                    None,
                    &data
                )
                .is_some(),
                "{event_type} accepted a missing event id"
            );
        }
    }

    #[test]
    fn v1_structure_requires_source_reference_on_cited() {
        let data = json!({ "citation_type": "direct_quote" });
        assert!(
            v1_structural_violation(
                "content_cited",
                true,
                Some("response:1"),
                false,
                None,
                None,
                &data
            )
            .is_some(),
            "content_cited accepted an event with no source reference"
        );
    }

    #[test]
    fn v1_structure_requires_typed_presentation_data() {
        // presentation_kind is closed with no fallback member, so an
        // out-of-set value is a violation, not a normalisation case.
        assert!(
            v1_structural_violation(
                "content_presented",
                true,
                Some("response:1"),
                false,
                Some("https://example.com/a"),
                None,
                &json!({ "presentation_kind": "hologram", "presentation_type": "link" })
            )
            .is_some()
        );
        // presentation_type is an open vocabulary: custom values pass.
        assert!(
            v1_structural_violation(
                "content_presented",
                true,
                Some("response:1"),
                false,
                Some("https://example.com/a"),
                None,
                &json!({ "presentation_kind": "content", "presentation_type": "hologram" })
            )
            .is_none()
        );
    }

    #[test]
    fn v1_structure_requires_presentation_id_on_engagement() {
        assert!(
            v1_structural_violation(
                "content_engaged",
                false,
                None,
                true,
                Some("https://example.com/a"),
                None,
                &json!({ "engagement_type": "link_click" })
            )
            .is_none()
        );
        assert!(
            v1_structural_violation(
                "content_engaged",
                false,
                None,
                false,
                Some("https://example.com/a"),
                None,
                &json!({ "engagement_type": "link_click" })
            )
            .is_some()
        );
        // Other event types carry no v1 structural requirements.
        assert!(
            v1_structural_violation(
                "content_retrieved",
                false,
                None,
                false,
                None,
                None,
                &json!({})
            )
            .is_none()
        );
    }

    #[test]
    fn v1_structure_requires_grounding_scope() {
        let ok = |data: &Value| {
            v1_structural_violation(
                "content_grounded",
                false,
                None,
                false,
                Some("https://example.com/a"),
                None,
                data,
            )
        };
        assert!(ok(&json!({ "scope": "session" })).is_none());
        assert!(ok(&json!({ "scope": "turn" })).is_none());
        assert!(ok(&json!({})).is_some(), "missing scope must be rejected");
        assert!(
            ok(&json!({ "scope": "paragraph" })).is_some(),
            "out-of-set scope must be rejected"
        );
    }

    #[test]
    fn v1_structure_requires_provenance_cached_consistency() {
        let check = |data: Value| {
            v1_structural_violation(
                "content_grounded",
                false,
                None,
                false,
                Some("https://example.com/a"),
                None,
                &data,
            )
        };
        // agent_fetched asserts a live fetch: cached must be false.
        assert!(
            check(json!({ "scope": "turn", "provenance": "agent_fetched", "cached": false }))
                .is_none()
        );
        assert!(
            check(json!({ "scope": "turn", "provenance": "agent_fetched", "cached": true }))
                .is_some()
        );
        assert!(check(json!({ "scope": "turn", "provenance": "agent_fetched" })).is_some());
        // agent_cached asserts reuse: cached must be true.
        assert!(
            check(json!({ "scope": "turn", "provenance": "agent_cached", "cached": true }))
                .is_none()
        );
        assert!(
            check(json!({ "scope": "turn", "provenance": "agent_cached", "cached": false }))
                .is_some()
        );
        assert!(check(json!({ "scope": "turn", "provenance": "agent_cached" })).is_some());
        // third_party_sourced leaves cached unconstrained (spec 6.4).
        assert!(
            check(json!({ "scope": "turn", "provenance": "third_party_sourced", "cached": true }))
                .is_none()
        );
        assert!(check(json!({ "scope": "turn", "provenance": "third_party_sourced" })).is_none());
        // Absent provenance constrains nothing.
        assert!(check(json!({ "scope": "turn", "cached": true })).is_none());
    }

    #[test]
    fn v1_structure_requires_citation_type() {
        let check = |data: Value| {
            v1_structural_violation(
                "content_cited",
                true,
                Some("response:1"),
                false,
                Some("https://example.com/a"),
                None,
                &data,
            )
        };
        assert!(check(json!({ "citation_type": "unclassified" })).is_none());
        assert!(
            check(json!({})).is_some(),
            "missing citation_type must be rejected"
        );
    }

    #[test]
    fn source_role_required_on_retrieval_only() {
        assert!(source_role_violation("content_retrieved", None).is_some());
        assert!(source_role_violation("content_retrieved", Some("")).is_some());
        assert!(source_role_violation("content_retrieved", Some("edge")).is_none());
        assert!(source_role_violation("content_grounded", None).is_none());
        assert!(source_role_violation("turn_started", None).is_none());
    }

    #[test]
    fn scoped_fields_may_not_appear_on_other_types() {
        // Conforming placements pass.
        assert!(field_placement_violation("content_engaged", true, true, false, false).is_none());
        assert!(
            field_placement_violation("content_presented", false, false, true, false).is_none()
        );
        assert!(field_placement_violation("turn_started", false, false, false, true).is_none());
        assert!(field_placement_violation("turn_completed", false, false, false, true).is_none());

        // Misplacements are violations (spec 5.7.5).
        assert!(
            field_placement_violation("content_presented", true, false, false, false).is_some()
        );
        assert!(
            field_placement_violation("content_retrieved", false, true, false, false).is_some()
        );
        assert!(field_placement_violation("content_cited", false, false, true, false).is_some());
        assert!(field_placement_violation("content_engaged", true, false, true, false).is_some());
        assert!(field_placement_violation("content_grounded", false, false, false, true).is_some());
        assert!(
            field_placement_violation("checkout_completed", false, false, false, true).is_some()
        );
    }

    #[test]
    fn withdrawn_preserved_in_output_is_stripped_from_fingerprints() {
        let mut data = json!({
            "scope": "turn",
            "content_fingerprint": {
                "scheme": "example:watermark",
                "detected": true,
                "preserved_in_output": true
            }
        });
        assert_eq!(
            strip_withdrawn_data_fields(&mut data),
            vec!["content_fingerprint.preserved_in_output"]
        );
        assert_eq!(data["content_fingerprint"]["detected"], true);
        assert!(
            data["content_fingerprint"]
                .get("preserved_in_output")
                .is_none()
        );

        // A conforming fingerprint is left alone.
        let mut clean = json!({
            "content_fingerprint": { "scheme": "example:watermark", "detected": false }
        });
        assert!(strip_withdrawn_data_fields(&mut clean).is_empty());
    }

    #[test]
    fn content_identifier_required_on_content_events_only() {
        assert!(!content_identifier_present("content_grounded", None, None));
        assert!(content_identifier_present(
            "content_grounded",
            Some("https://example.com/a"),
            None
        ));
        assert!(content_identifier_present(
            "content_cited",
            None,
            Some("cms:123")
        ));
        assert!(content_identifier_present("turn_started", None, None));
        assert!(content_identifier_present("checkout_completed", None, None));
    }

    #[test]
    fn minimal_turn_strips_everything_but_tokens_and_urls() {
        let mut turn = json!({
            "privacy_level": "minimal",
            "query_text": "secret question",
            "query_intent": "comparison",
            "topics": ["a"],
            "response_type": "recommendation",
            "response_mode": "standard",
            "model_id": "m",
            "ad_rendered": true,
            "response_tokens": 280,
            "content_urls_cited": ["https://example.com/a"]
        });
        let mut stripped = strip_turn_privacy_violations(&mut turn);
        stripped.sort();
        assert_eq!(
            stripped,
            vec![
                "ad_rendered",
                "model_id",
                "query_intent",
                "query_text",
                "response_mode",
                "response_type",
                "topics"
            ]
        );
        assert_eq!(turn["response_tokens"], 280);
        assert_eq!(turn["content_urls_cited"][0], "https://example.com/a");
        assert!(turn.get("query_text").is_none());
    }

    #[test]
    fn intent_turn_strips_text_only() {
        let mut turn = json!({
            "privacy_level": "intent",
            "query_text": "secret",
            "query_intent": "comparison",
            "topics": ["headphones"]
        });
        let stripped = strip_turn_privacy_violations(&mut turn);
        assert_eq!(stripped, vec!["query_text"]);
        assert_eq!(turn["query_intent"], "comparison");
    }

    #[test]
    fn full_and_summary_turns_untouched() {
        for level in ["full", "summary"] {
            let mut turn = json!({
                "privacy_level": level,
                "query_text": "q",
                "response_text": "r"
            });
            assert!(strip_turn_privacy_violations(&mut turn).is_empty());
            assert_eq!(turn["query_text"], "q");
        }
    }

    #[test]
    fn unknown_privacy_levels_fail_closed_to_minimal() {
        // Capitalisation, stray whitespace, and made-up values are all
        // outside the PrivacyLevel enum and must strip like `minimal`.
        for level in ["Minimal", "minimal ", "FULL", "platinum", ""] {
            let mut turn = json!({
                "privacy_level": level,
                "query_text": "secret question",
                "query_intent": "comparison",
                "response_tokens": 280
            });
            let stripped = strip_turn_privacy_violations(&mut turn);
            assert!(
                stripped.contains(&"query_text".to_string()),
                "level {level:?} did not strip query_text"
            );
            assert!(turn.get("query_text").is_none());
            assert!(turn.get("query_intent").is_none());
            assert_eq!(turn["response_tokens"], 280);
        }
    }

    #[test]
    fn invalid_privacy_levels_rewritten_to_minimal() {
        // Stripping alone is not enough: privacy_level is a closed enum,
        // so an out-of-enum value left in place would fail the standard
        // schema on the materialised document.
        let invalid = [
            json!("Minimal"),
            json!("minimal "),
            json!("ultra"),
            json!(5),
            Value::Null,
        ];
        for level in invalid {
            let mut turn = json!({
                "query_text": "secret question",
                "query_intent": "comparison",
                "response_tokens": 280
            });
            if !level.is_null() {
                turn["privacy_level"] = level.clone();
            }
            strip_turn_privacy_violations(&mut turn);
            assert_eq!(
                turn["privacy_level"], "minimal",
                "level {level:?} was not rewritten to minimal"
            );
            assert!(turn.get("query_text").is_none());
            assert!(turn.get("query_intent").is_none());
            assert_eq!(turn["response_tokens"], 280);
        }

        // Valid levels are never rewritten.
        for level in PRIVACY_LEVELS {
            let mut turn = json!({ "privacy_level": level, "response_tokens": 1 });
            strip_turn_privacy_violations(&mut turn);
            assert_eq!(turn["privacy_level"], *level);
        }
    }

    #[test]
    fn missing_privacy_level_strips_as_minimal() {
        let mut turn = json!({
            "query_text": "secret question",
            "topics": ["a"],
            "response_tokens": 12
        });
        let mut stripped = strip_turn_privacy_violations(&mut turn);
        stripped.sort();
        assert_eq!(stripped, vec!["query_text", "topics"]);
        assert_eq!(turn["response_tokens"], 12);

        let mut non_string = json!({
            "privacy_level": 3,
            "query_text": "secret question"
        });
        let stripped = strip_turn_privacy_violations(&mut non_string);
        assert_eq!(stripped, vec!["query_text"]);
    }

    #[test]
    fn null_fields_are_not_violations() {
        let mut turn = json!({
            "privacy_level": "minimal",
            "query_text": null,
            "response_tokens": 12
        });
        assert!(strip_turn_privacy_violations(&mut turn).is_empty());
    }

    #[test]
    fn access_context_shape_is_checked_and_its_vocabulary_is_not() {
        // The spec's own example (5.1.3), plus a scheme core does not name.
        // Consumers MUST tolerate unknown schemes, so the unknown one is not
        // a violation.
        let spec_example = json!({
            "access_context": {
                "identifiers": [
                    { "scheme": "ror", "value": "https://ror.org/013meh722" },
                    { "scheme": "saml_entity_id", "value": "https://idp.example.ac.uk/shibboleth" },
                    { "scheme": "example_local", "value": "lib-4471" }
                ]
            }
        });
        assert!(session_data_violation(Some(&spec_example)).is_none());

        // Unknown fields inside the container, inside access_context and
        // inside an identifier are all tolerated (5.1.3).
        let extras = json!({
            "com.example.reporting_period": "2026-08",
            "access_context": {
                "asserted_by": "agent",
                "identifiers": [
                    { "scheme": "isni", "value": "0000000121032683", "note": "consortium seat" }
                ]
            }
        });
        assert!(session_data_violation(Some(&extras)).is_none());

        // A container with no access_context, and no container at all.
        assert!(session_data_violation(Some(&json!({ "x": 1 }))).is_none());
        assert!(session_data_violation(None).is_none());
        assert!(session_data_violation(Some(&Value::Null)).is_none());
    }

    #[test]
    fn malformed_access_context_is_a_violation() {
        // Both cases are the standard's own invalid fixtures.
        let missing_value = json!({
            "access_context": { "identifiers": [{ "scheme": "ror" }] }
        });
        assert_eq!(
            session_data_violation(Some(&missing_value)).as_deref(),
            Some("data.access_context.identifiers[0] requires value")
        );

        let not_an_array = json!({
            "access_context": { "identifiers": "https://ror.org/013meh722" }
        });
        assert!(
            session_data_violation(Some(&not_an_array))
                .is_some_and(|v| v.contains("must be an array"))
        );

        let non_string_scheme = json!({
            "access_context": { "identifiers": [{ "scheme": 7, "value": "x" }] }
        });
        assert_eq!(
            session_data_violation(Some(&non_string_scheme)).as_deref(),
            Some("data.access_context.identifiers[0].scheme must be a string")
        );

        assert!(session_data_violation(Some(&json!("not an object"))).is_some());
    }
}
