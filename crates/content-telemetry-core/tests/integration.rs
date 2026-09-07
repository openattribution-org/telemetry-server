use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use content_telemetry_core::models::event::{EdgeEventInput, TelemetryEventInput};
use content_telemetry_core::models::session::{
    BulkSessionRequest, SessionCreateRequest, SessionEndRequest, SessionOutcome,
};
use content_telemetry_core::services::{click_tokens, events, queries, sessions};
use content_telemetry_core::{conformance, standard};

// ---------------------------------------------------------------------------
// Test constants
// ---------------------------------------------------------------------------

const AGENT_ORG: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const PUBLISHER_ORG: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const PRIVATE_PUB_ORG: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

fn agent_org_id() -> Uuid {
    AGENT_ORG.parse().unwrap()
}

fn publisher_org_id() -> Uuid {
    PUBLISHER_ORG.parse().unwrap()
}

fn private_pub_org_id() -> Uuid {
    PRIVATE_PUB_ORG.parse().unwrap()
}

fn minimal_session_request() -> SessionCreateRequest {
    serde_json::from_value(serde_json::json!({
        "initiator_type": "agent",
        "agent_id": "test-agent-1",
    }))
    .unwrap()
}

// ===================================================================
// Session lifecycle
// ===================================================================

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn session_create_returns_active_session(pool: PgPool) {
    let org = agent_org_id();
    let req = minimal_session_request();

    let session = sessions::create_session(&pool, org, &req).await.unwrap();

    assert_eq!(session.organization_id, org);
    assert_eq!(session.initiator_type, "agent");
    assert!(
        session.ended_at.is_none(),
        "new session should not be ended"
    );
    assert!(
        (Utc::now() - session.started_at).num_seconds() < 5,
        "started_at should be recent"
    );
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn session_end_sets_outcome_and_ended_at(pool: PgPool) {
    let org = agent_org_id();
    let session = sessions::create_session(&pool, org, &minimal_session_request())
        .await
        .unwrap();

    let end_req = SessionEndRequest {
        session_id: session.id.to_string(),
        outcome: SessionOutcome {
            outcome_type: "conversion".to_string(),
            value_amount: 100,
            currency: "GBP".to_string(),
            products: vec![],
            metadata: serde_json::Value::Null,
        },
    };

    let ended = sessions::end_session(&pool, org, &end_req, None)
        .await
        .unwrap();
    assert!(ended.is_some(), "end_session should return the session");

    let ended = ended.unwrap();
    assert!(ended.ended_at.is_some(), "ended_at should be set");
    assert_eq!(ended.outcome_type.as_deref(), Some("conversion"));
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn session_end_twice_returns_none(pool: PgPool) {
    let org = agent_org_id();
    let session = sessions::create_session(&pool, org, &minimal_session_request())
        .await
        .unwrap();

    let end_req = SessionEndRequest {
        session_id: session.id.to_string(),
        outcome: SessionOutcome {
            outcome_type: "browse".to_string(),
            value_amount: 0,
            currency: "USD".to_string(),
            products: vec![],
            metadata: serde_json::Value::Null,
        },
    };

    let first = sessions::end_session(&pool, org, &end_req, None)
        .await
        .unwrap();
    assert!(first.is_some(), "first end should succeed");

    let second = sessions::end_session(&pool, org, &end_req, None)
        .await
        .unwrap();
    assert!(
        second.is_none(),
        "second end_session should return None (already ended)"
    );
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn session_end_wrong_org_returns_none(pool: PgPool) {
    let org_a = agent_org_id();
    let org_b = publisher_org_id();

    let session = sessions::create_session(&pool, org_a, &minimal_session_request())
        .await
        .unwrap();

    let end_req = SessionEndRequest {
        session_id: session.id.to_string(),
        outcome: SessionOutcome {
            outcome_type: "browse".to_string(),
            value_amount: 0,
            currency: "USD".to_string(),
            products: vec![],
            metadata: serde_json::Value::Null,
        },
    };

    let result = sessions::end_session(&pool, org_b, &end_req, None)
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "ending a session with the wrong org should return None"
    );
}

// ===================================================================
// Events
// ===================================================================

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn events_created_for_active_session(pool: PgPool) {
    let org = agent_org_id();
    let session = sessions::create_session(&pool, org, &minimal_session_request())
        .await
        .unwrap();

    let now = Utc::now();
    let event_inputs = vec![
        TelemetryEventInput {
            id: Some(Uuid::new_v4()),
            event_type: "content_retrieved".to_string(),
            timestamp: now,
            session_id: None,
            ctx_token: None,
            turn_id: None,
            source_role: Some("agent".to_string()),
            content_telemetry_id: None,
            content_url: Some("https://example.com/article-1".to_string()),
            content_id: None,
            output_id: None,
            output_element_id: None,
            citation_id: None,
            presentation_id: None,
            license_ref: None,
            terms_ref: None,
            product_id: None,
            turn: None,
            data: serde_json::json!({}),
        },
        TelemetryEventInput {
            id: Some(Uuid::new_v4()),
            event_type: "content_cited".to_string(),
            timestamp: now,
            session_id: None,
            ctx_token: None,
            turn_id: Some("turn-1".to_string()),
            source_role: Some("agent".to_string()),
            content_telemetry_id: None,
            content_url: Some("https://example.com/article-1".to_string()),
            content_id: None,
            output_id: None,
            output_element_id: None,
            citation_id: None,
            presentation_id: None,
            license_ref: None,
            terms_ref: None,
            product_id: None,
            turn: None,
            data: serde_json::json!({}),
        },
    ];

    let created = events::create_events(&pool, session.id, org, &event_inputs)
        .await
        .unwrap();

    assert_eq!(created.len(), 2);
    for row in &created {
        assert_eq!(row.session_id, Some(session.id));
        assert_eq!(row.organization_id, org);
    }
    assert_eq!(created[0].event_type, "content_retrieved");
    assert_eq!(created[1].event_type, "content_cited");
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn edge_events_created_without_session(pool: PgPool) {
    let org = publisher_org_id();
    let now = Utc::now();

    let edge_inputs = vec![EdgeEventInput {
        id: None,
        event_type: "content_retrieved".to_string(),
        timestamp: now,
        source_role: Some("edge".to_string()),
        content_telemetry_id: None,
        content_url: Some("https://example.com/page".to_string()),
        content_id: None,
        license_ref: None,
        data: serde_json::json!({"user_agent": "TestBot/1.0"}),
    }];

    let created = events::create_edge_events(&pool, org, &edge_inputs)
        .await
        .unwrap();

    assert_eq!(created.len(), 1);
    assert_eq!(created[0].organization_id, org);
    assert!(
        created[0].session_id.is_none(),
        "edge events should have no session_id"
    );
    assert_eq!(created[0].event_type, "content_retrieved");
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn session_with_events_returns_both(pool: PgPool) {
    let org = agent_org_id();
    let session = sessions::create_session(&pool, org, &minimal_session_request())
        .await
        .unwrap();

    let now = Utc::now();
    let event_inputs = vec![TelemetryEventInput {
        id: Some(Uuid::new_v4()),
        event_type: "content_retrieved".to_string(),
        timestamp: now,
        session_id: None,
        ctx_token: None,
        turn_id: None,
        source_role: Some("agent".to_string()),
        content_telemetry_id: None,
        content_url: Some("https://example.com/article".to_string()),
        content_id: None,
        output_id: None,
        output_element_id: None,
        citation_id: None,
        presentation_id: None,
        license_ref: None,
        terms_ref: None,
        product_id: None,
        turn: None,
        data: serde_json::json!({}),
    }];

    events::create_events(&pool, session.id, org, &event_inputs)
        .await
        .unwrap();

    let result = sessions::get_session_with_events(&pool, session.id)
        .await
        .unwrap();

    assert!(result.is_some(), "should find session with events");
    let swe = result.unwrap();
    assert_eq!(swe.session.id, session.id);
    assert_eq!(swe.events.len(), 1);
    assert_eq!(swe.events[0].event_type, "content_retrieved");
}

// ===================================================================
// Click tokens
// ===================================================================

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn click_token_created_and_looked_up(pool: PgPool) {
    let org = agent_org_id();
    let session = sessions::create_session(&pool, org, &minimal_session_request())
        .await
        .unwrap();

    // Add events with content URLs for the consenting publisher domain
    let now = Utc::now();
    let event_inputs = vec![
        TelemetryEventInput {
            id: Some(Uuid::new_v4()),
            event_type: "content_retrieved".to_string(),
            timestamp: now,
            session_id: None,
            ctx_token: None,
            turn_id: None,
            source_role: Some("agent".to_string()),
            content_telemetry_id: None,
            content_url: Some("https://example.com/article-1".to_string()),
            content_id: None,
            output_id: None,
            output_element_id: None,
            citation_id: None,
            presentation_id: None,
            license_ref: None,
            terms_ref: None,
            product_id: None,
            turn: None,
            data: serde_json::json!({}),
        },
        TelemetryEventInput {
            id: Some(Uuid::new_v4()),
            event_type: "content_cited".to_string(),
            timestamp: now,
            session_id: None,
            ctx_token: None,
            turn_id: Some("turn-1".to_string()),
            source_role: Some("agent".to_string()),
            content_telemetry_id: None,
            content_url: Some("https://example.com/article-1".to_string()),
            content_id: None,
            output_id: Some("response:1".to_string()),
            output_element_id: None,
            citation_id: None,
            presentation_id: None,
            license_ref: None,
            terms_ref: None,
            product_id: None,
            turn: None,
            data: serde_json::json!({}),
        },
        TelemetryEventInput {
            id: Some(Uuid::new_v4()),
            event_type: "content_presented".to_string(),
            timestamp: now,
            session_id: None,
            ctx_token: None,
            turn_id: Some("turn-1".to_string()),
            source_role: Some("agent".to_string()),
            content_telemetry_id: None,
            content_url: Some("https://example.com/article-1".to_string()),
            content_id: None,
            output_id: Some("response:1".to_string()),
            output_element_id: None,
            citation_id: None,
            presentation_id: None,
            license_ref: None,
            terms_ref: None,
            product_id: None,
            turn: None,
            data: serde_json::json!({
                "presentation_kind": "source_reference",
                "presentation_type": "link"
            }),
        },
    ];

    events::create_events(&pool, session.id, org, &event_inputs)
        .await
        .unwrap();

    // A legacy row stored before the v1 rename still surfaces in the
    // manifest, and its URL counts as presented.
    sqlx::query(
        "INSERT INTO events (session_id, organization_id, event_type, content_url, event_data, event_timestamp)
         VALUES ($1, $2, 'content_displayed', 'https://example.com/article-1', '{\"display_type\":\"link\"}', $3)",
    )
    .bind(session.id)
    .bind(org)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    // Create click token
    let ct =
        click_tokens::create_click_token(&pool, session.id, "https://example.com/article-1", None)
            .await
            .unwrap();

    assert_eq!(ct.session_id, session.id);
    assert!(!ct.token.is_empty());

    // Look up click token
    let context = click_tokens::lookup_by_token(&pool, &ct.token)
        .await
        .unwrap();

    assert!(context.is_some(), "lookup should return a click manifest");
    let resolved = context.unwrap();
    assert_eq!(resolved.session_id, session.id);
    let manifest = resolved.manifest;
    assert_eq!(manifest.click_content_url, "https://example.com/article-1");

    // The manifest is the session's grounded/cited/presented events for
    // consenting domains (spec 7.1). Retrieval events are not part of it.
    assert!(
        manifest
            .events
            .iter()
            .all(|e| e.event_type != "content_retrieved"),
        "retrieved events must not appear in the click manifest"
    );
    assert!(
        manifest
            .events
            .iter()
            .any(|e| e.event_type == "content_cited"),
        "cited events for consenting domain should be included"
    );
    assert!(
        !manifest.content_urls_cited.is_empty(),
        "cited URLs for consenting domain should be included"
    );
    assert!(
        manifest
            .events
            .iter()
            .any(|e| e.event_type == "content_presented"),
        "presented events for consenting domain should be included"
    );
    assert!(
        manifest
            .events
            .iter()
            .any(|e| e.event_type == "content_displayed"),
        "stored v0.1 displayed events still surface for legacy sessions"
    );
    assert_eq!(
        manifest.content_urls_presented,
        vec!["https://example.com/article-1".to_string()],
        "presented URLs include the legacy displayed row's URL, deduplicated"
    );

    // The raw session UUID must never cross the click-out boundary
    // (spec 7.1, commerce profile 5.5).
    let serialised = serde_json::to_value(&manifest).unwrap();
    assert!(
        serialised.get("session_id").is_none(),
        "manifest response must not carry session_id"
    );
    assert!(
        !serialised.to_string().contains(&session.id.to_string()),
        "manifest response must not leak the session UUID anywhere"
    );
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn click_token_respects_agent_consent(pool: PgPool) {
    // Use the non-consenting private publisher org as the session owner.
    // This org does NOT have share_sessions_via_click_tokens = true.
    let org = private_pub_org_id();
    let session = sessions::create_session(&pool, org, &minimal_session_request())
        .await
        .unwrap();

    let ct = click_tokens::create_click_token(&pool, session.id, "https://example.com/page", None)
        .await
        .unwrap();

    let context = click_tokens::lookup_by_token(&pool, &ct.token)
        .await
        .unwrap();

    assert!(
        context.is_none(),
        "lookup should return None when agent org has not consented to click token sharing"
    );

    // Event binding is not disclosure: the token still resolves to its
    // session for ingest, so corroborating engagement events from
    // downstream observers are accepted regardless of the disclosure gate.
    let resolved = click_tokens::resolve_session_id(&pool, &ct.token)
        .await
        .unwrap();
    assert_eq!(
        resolved,
        Some(session.id),
        "resolve_session_id should bind events even without disclosure consent"
    );
}

// ===================================================================
// Publisher funnel: bot and category filters must reach session-attached
// self-report stages, not just edge events. A regression guard for the
// filter that collapsed the funnel to retrieval-only under any bot filter.
// ===================================================================

fn agent_event(event_type: &str, url: &str, data: serde_json::Value) -> TelemetryEventInput {
    TelemetryEventInput {
        id: Some(Uuid::new_v4()),
        event_type: event_type.to_string(),
        timestamp: Utc::now(),
        session_id: None,
        ctx_token: None,
        turn_id: None,
        source_role: Some("agent".to_string()),
        content_telemetry_id: None,
        content_url: Some(url.to_string()),
        content_id: None,
        output_id: None,
        output_element_id: None,
        citation_id: None,
        presentation_id: None,
        license_ref: None,
        terms_ref: None,
        product_id: None,
        turn: None,
        data,
    }
}

fn cited_count(summary: &content_telemetry_core::models::query::PublisherSummary) -> i64 {
    summary
        .events_by_type
        .iter()
        .find(|r| r.event_type == "content_cited")
        .map(|r| r.count)
        .unwrap_or(0)
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn publisher_bot_filter_includes_session_attached_citations(pool: PgPool) {
    // An agent self-reports a citation. The bot is identified by the session's
    // agent_id; the event carries no bot_name in event_data. The pre-fix funnel
    // filter keyed only on event_data and silently dropped this citation,
    // collapsing the funnel to retrieval-only under any bot filter.
    let agent_org = agent_org_id();
    let req: SessionCreateRequest = serde_json::from_value(serde_json::json!({
        "initiator_type": "agent",
        "agent_id": "Claude-User",
    }))
    .unwrap();
    let session = sessions::create_session(&pool, agent_org, &req)
        .await
        .unwrap();

    let inputs = vec![
        agent_event(
            "content_retrieved",
            "https://example.com/a",
            serde_json::json!({}),
        ),
        agent_event(
            "content_cited",
            "https://example.com/a",
            serde_json::json!({}),
        ),
    ];
    events::create_events(&pool, session.id, agent_org, &inputs)
        .await
        .unwrap();

    // An unrelated edge crawler, identified the usual way via event_data.
    let edge = vec![EdgeEventInput {
        id: None,
        event_type: "content_retrieved".to_string(),
        timestamp: Utc::now(),
        source_role: Some("edge".to_string()),
        content_telemetry_id: None,
        content_url: Some("https://example.com/a".to_string()),
        content_id: None,
        license_ref: None,
        data: serde_json::json!({"bot_name": "GPTBot", "bot_category": "training"}),
    }];
    events::create_edge_events(&pool, publisher_org_id(), &edge)
        .await
        .unwrap();

    let domains = vec!["example.com".to_string()];

    // Filtering to the agent's bot must surface its session-attached citation.
    let summary = queries::get_publisher_summary(
        &pool,
        publisher_org_id(),
        &domains,
        None,
        None,
        None,
        Some("Claude-User"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        cited_count(&summary),
        1,
        "bot filter should include the session-attached citation"
    );

    // The timeseries funnel must agree.
    let ts = queries::get_publisher_timeseries(
        &pool,
        &domains,
        None,
        None,
        None,
        Some("Claude-User"),
        None,
    )
    .await
    .unwrap();
    let ts_cited: i64 = ts.iter().map(|d| d.cited).sum();
    assert_eq!(
        ts_cited, 1,
        "timeseries bot filter should include the citation"
    );

    // Negative control: a different bot sees none of the agent's citations,
    // but its own edge retrieval is still matched via event_data.
    let other = queries::get_publisher_summary(
        &pool,
        publisher_org_id(),
        &domains,
        None,
        None,
        None,
        Some("GPTBot"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(cited_count(&other), 0, "GPTBot has no citations");
    let other_retrieved = other
        .events_by_type
        .iter()
        .find(|r| r.event_type == "content_retrieved")
        .map(|r| r.count)
        .unwrap_or(0);
    assert_eq!(
        other_retrieved, 1,
        "edge retrieval still matched via event_data"
    );
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn publisher_category_filter_includes_stamped_agent_citations(pool: PgPool) {
    // When the emitter stamps bot_category on the self-report event (as the
    // demo generator now does), a category filter includes the agent stages.
    let agent_org = agent_org_id();
    let req: SessionCreateRequest = serde_json::from_value(serde_json::json!({
        "initiator_type": "agent",
        "agent_id": "Claude-User",
    }))
    .unwrap();
    let session = sessions::create_session(&pool, agent_org, &req)
        .await
        .unwrap();
    let inputs = vec![agent_event(
        "content_cited",
        "https://example.com/a",
        serde_json::json!({"bot_name": "Claude-User", "bot_category": "inference"}),
    )];
    events::create_events(&pool, session.id, agent_org, &inputs)
        .await
        .unwrap();

    let domains = vec!["example.com".to_string()];
    let summary = queries::get_publisher_summary(
        &pool,
        publisher_org_id(),
        &domains,
        None,
        None,
        None,
        None,
        Some("inference"),
    )
    .await
    .unwrap();
    assert_eq!(
        cited_count(&summary),
        1,
        "inference category should include the stamped citation"
    );
}

// ===================================================================
// Publisher summary: HTTP status-code breakdown. Only edge enrichment
// stamps response_status; everything else must land in the NULL bucket
// rather than being dropped or failing the aggregation.
// ===================================================================

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn publisher_summary_breaks_down_status_codes(pool: PgPool) {
    let mk_edge = |status: serde_json::Value| EdgeEventInput {
        id: None,
        event_type: "content_retrieved".to_string(),
        timestamp: Utc::now(),
        source_role: Some("edge".to_string()),
        content_telemetry_id: None,
        content_url: Some("https://example.com/a".to_string()),
        content_id: None,
        license_ref: None,
        data: serde_json::json!({"bot_name": "GPTBot", "bot_category": "training", "response_status": status}),
    };
    let edge = vec![
        mk_edge(serde_json::json!(200)),
        mk_edge(serde_json::json!(200)),
        mk_edge(serde_json::json!(404)),
        // Malformed status must fold into the NULL bucket, not error the query.
        mk_edge(serde_json::json!("cached")),
    ];
    events::create_edge_events(&pool, publisher_org_id(), &edge)
        .await
        .unwrap();

    // A self-report event with no response_status also lands in NULL.
    let agent_org = agent_org_id();
    let session = sessions::create_session(&pool, agent_org, &minimal_session_request())
        .await
        .unwrap();
    let inputs = vec![agent_event(
        "content_cited",
        "https://example.com/a",
        serde_json::json!({}),
    )];
    events::create_events(&pool, session.id, agent_org, &inputs)
        .await
        .unwrap();

    let domains = vec!["example.com".to_string()];
    let summary = queries::get_publisher_summary(
        &pool,
        publisher_org_id(),
        &domains,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let count_for = |status: Option<i32>| {
        summary
            .events_by_status
            .iter()
            .find(|r| r.status == status)
            .map(|r| r.count)
            .unwrap_or(0)
    };
    assert_eq!(count_for(Some(200)), 2, "two 200 edge retrievals");
    assert_eq!(count_for(Some(404)), 1, "one 404 edge retrieval");
    assert_eq!(
        count_for(None),
        2,
        "malformed and statusless events fold into the NULL bucket"
    );
}

// ===================================================================
// Session data container (specification section 5.1.3)
// ===================================================================

/// The standard's own fixtures, copied byte-for-byte; see `spec/README.md`.
const SPEC_SESSION_ACCESS_CONTEXT: &str = include_str!("spec/session-access-context.json");
const SPEC_IDENTIFIER_MISSING_VALUE: &str =
    include_str!("spec/access-context-identifier-missing-value.json");
const SPEC_IDENTIFIERS_NOT_ARRAY: &str =
    include_str!("spec/access-context-identifiers-not-array.json");

fn spec_document(source: &str) -> serde_json::Value {
    serde_json::from_str(source).expect("spec fixture is valid JSON")
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn spec_access_context_survives_ingest_and_retrieval(pool: PgPool) {
    let org = agent_org_id();
    let document = spec_document(SPEC_SESSION_ACCESS_CONTEXT);

    let request: BulkSessionRequest =
        serde_json::from_value(document.clone()).expect("the spec fixture deserialises");
    assert!(
        conformance::session_data_violation(request.data.as_ref()).is_none(),
        "the standard's own valid fixture must pass the access_context check"
    );

    let session = sessions::create_session(&pool, org, &request.session_create())
        .await
        .unwrap();
    events::create_events(&pool, session.id, org, &request.events)
        .await
        .unwrap();

    let stored = sessions::get_session_with_events(&pool, session.id)
        .await
        .unwrap()
        .unwrap();
    let materialised = standard::standard_document(&stored);

    // The round trip is lossless: the container comes back as sent, both
    // core schemes included, with nothing normalised or reordered.
    assert_eq!(materialised["data"], document["data"]);
    assert_eq!(
        materialised["data"]["access_context"]["identifiers"][0]["value"],
        "https://ror.org/013meh722"
    );
    assert_eq!(
        materialised["data"]["access_context"]["identifiers"][1]["scheme"],
        "saml_entity_id"
    );
    assert_eq!(materialised["events"].as_array().unwrap().len(), 4);

    // The fixture's `_test_description` is an upstream harness annotation,
    // not a member of the document format. It is undefined at the session
    // root (spec 5.1.3), so it is recorded rather than dropped (spec 5.7.4).
    assert_eq!(
        materialised["extensions"]["unrecognised_fields"]["_test_description"],
        document["_test_description"]
    );
}

#[sqlx::test(migrations = "./migrations", fixtures("setup"))]
async fn unknown_identifier_schemes_survive_ingest_and_retrieval(pool: PgPool) {
    // Emitters MAY use schemes outside the core three and consumers MUST
    // tolerate them (spec 5.1.3), so an unknown scheme is stored and served
    // like any other.
    let org = agent_org_id();
    let data = serde_json::json!({
        "access_context": {
            "identifiers": [
                { "scheme": "example_consortium", "value": "seat-4471" }
            ]
        },
        "com.example.reporting_period": "2026-08"
    });

    let request: SessionCreateRequest = serde_json::from_value(serde_json::json!({
        "initiator_type": "agent",
        "agent_id": "scholar-assistant.example.com",
        "data": data.clone(),
    }))
    .unwrap();
    assert!(conformance::session_data_violation(request.data.as_ref()).is_none());

    let session = sessions::create_session(&pool, org, &request)
        .await
        .unwrap();
    let stored = sessions::get_session_with_events(&pool, session.id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(standard::standard_document(&stored)["data"], data);
}

#[test]
fn spec_invalid_access_context_fixtures_are_refused() {
    for source in [SPEC_IDENTIFIER_MISSING_VALUE, SPEC_IDENTIFIERS_NOT_ARRAY] {
        let document = spec_document(source);
        let request: BulkSessionRequest = serde_json::from_value(document.clone()).unwrap();
        let violation = conformance::session_data_violation(request.data.as_ref());
        assert!(
            violation.is_some(),
            "the standard's invalid fixture should be refused: {}",
            document["_test_description"]
        );
    }
}
