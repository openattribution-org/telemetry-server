use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

use crate::models::click_token::{
    ClickManifestEvent, ClickManifestResponse, ClickTokenRow, ResolvedClickToken,
};
use crate::models::event::EventRow;

/// Create a click token mapping a click-out event to a session.
///
/// If `token` is None, a random UUID-based token is generated.
pub async fn create_click_token(
    pool: &PgPool,
    session_id: Uuid,
    content_url: &str,
    token: Option<&str>,
) -> Result<ClickTokenRow, sqlx::Error> {
    let token_value = token.map_or_else(|| Uuid::new_v4().to_string(), String::from);

    sqlx::query_as::<_, ClickTokenRow>(
        "INSERT INTO click_tokens (token, session_id, content_url)
         VALUES ($1, $2, $3)
         RETURNING *",
    )
    .bind(&token_value)
    .bind(session_id)
    .bind(content_url)
    .fetch_one(pool)
    .await
}

/// Extract the host (domain) from a URL, or None if unparseable.
fn extract_domain(content_url: &str) -> Option<String> {
    Url::parse(content_url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
}

/// Resolve a ctx_token to its owning session for event binding.
///
/// No consent gate: a downstream observer reporting a corroborating
/// `content_engaged` event with a ctx_token (spec 7.1) is delivering
/// telemetry to the consumer, not reading the session. The consent gate
/// applies to manifest disclosure (`lookup_by_token`), where session data
/// flows the other way.
///
/// Returns None if the token doesn't exist or has expired.
pub async fn resolve_session_id(pool: &PgPool, token: &str) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT session_id FROM click_tokens WHERE token = $1 AND expires_at > NOW()",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
}

/// Look up the click manifest for a click token.
///
/// The manifest is the resolved session's `content_grounded`,
/// `content_cited` and `content_presented` events (spec 7.1, commerce
/// profile 5.7.2) - every source that informed the response that produced
/// the click. Retrieval events are not part of the manifest: retrieved-but-
/// never-grounded content did not inform the response. Stored v0.1
/// `content_displayed` events are included for sessions recorded before
/// the v1 rename; their URLs count as presented.
///
/// Consent enforcement (two-sided, spec 7.1):
/// - If the agent org has not opted in (`share_sessions_via_click_tokens`),
///   returns None (token appears not found).
/// - Events are filtered to content URLs on domains whose verified owners
///   have opted in (`visible_in_click_token_lookups`). Events identified
///   only by `content_id` are withheld: the owner cannot be resolved by
///   domain, so consent cannot be confirmed.
///
/// Privacy gating: the manifest carries content events only, never
/// conversation-turn data, and `content_url` on content events is visible
/// at every privacy level including `minimal` (spec 5.5) - so the manifest
/// shape is valid at any privacy level the session declares.
///
/// Returns None if the token doesn't exist or has expired.
pub async fn lookup_by_token(
    pool: &PgPool,
    token: &str,
) -> Result<Option<ResolvedClickToken>, sqlx::Error> {
    let click_token = sqlx::query_as::<_, ClickTokenRow>(
        "SELECT * FROM click_tokens WHERE token = $1 AND expires_at > NOW()",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;

    let Some(click_token) = click_token else {
        return Ok(None);
    };

    let sid = click_token.session_id;

    // Check agent consent: has the session-owning org opted in?
    let agent_consent = sqlx::query_scalar::<_, bool>(
        "SELECT o.share_sessions_via_click_tokens
         FROM sessions s
         JOIN organizations o ON o.id = s.organization_id
         WHERE s.id = $1",
    )
    .bind(sid)
    .fetch_one(pool)
    .await?;

    if !agent_consent {
        return Ok(None);
    }

    // Agent consented - fetch session start and the manifest events.
    let (started_at, rows) = tokio::try_join!(
        sqlx::query_scalar::<_, DateTime<Utc>>("SELECT started_at FROM sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(pool),
        sqlx::query_as::<_, EventRow>(
            "SELECT * FROM events
             WHERE session_id = $1
               AND event_type IN ('content_grounded', 'content_cited', 'content_presented',
                                  'content_displayed')
             ORDER BY event_timestamp ASC",
        )
        .bind(sid)
        .fetch_all(pool),
    )?;

    // Batch-check which domains have content owner consent.
    let all_domains: Vec<String> = rows
        .iter()
        .filter_map(|r| r.content_url.as_deref().and_then(extract_domain))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let consenting_domains: HashSet<String> = if all_domains.is_empty() {
        HashSet::new()
    } else {
        sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT d.domain
             FROM domains d
             JOIN organizations o ON o.id = d.organization_id
             WHERE d.domain = ANY($1)
               AND d.verified_at IS NOT NULL
               AND o.visible_in_click_token_lookups = true",
        )
        .bind(&all_domains)
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect()
    };

    let events: Vec<ClickManifestEvent> = rows
        .into_iter()
        .filter(|r| {
            r.content_url
                .as_deref()
                .and_then(extract_domain)
                .is_some_and(|d| consenting_domains.contains(&d))
        })
        .map(|r| ClickManifestEvent {
            id: r.id,
            event_type: r.event_type,
            timestamp: r.event_timestamp,
            turn_id: r.turn_id,
            output_id: r.output_id,
            output_element_id: r.output_element_id,
            citation_id: r.citation_id,
            content_url: r.content_url,
            content_id: r.content_id,
            data: {
                let mut data = r.event_data;
                crate::conformance::strip_withdrawn_data_fields(&mut data);
                data
            },
        })
        .collect();

    let urls_for = |event_types: &[&str]| -> Vec<String> {
        let mut seen = HashSet::new();
        events
            .iter()
            .filter(|e| event_types.contains(&e.event_type.as_str()))
            .filter_map(|e| e.content_url.clone())
            .filter(|u| seen.insert(u.clone()))
            .collect()
    };

    let manifest = ClickManifestResponse {
        started_at,
        click_content_url: click_token.content_url,
        content_urls_grounded: urls_for(&["content_grounded"]),
        content_urls_cited: urls_for(&["content_cited"]),
        content_urls_presented: urls_for(&["content_presented", "content_displayed"]),
        events,
    };

    Ok(Some(ResolvedClickToken {
        session_id: sid,
        manifest,
    }))
}

/// Delete expired click tokens.
pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM click_tokens WHERE expires_at < NOW()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
