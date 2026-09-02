use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use url::Url;
use uuid::Uuid;

use crate::models::click_token::{
    ClickManifestEvent, ClickManifestResponse, ClickTokenRow, ResolvedClickToken,
};
use crate::models::event::EventRow;

/// Whether a ctx token value satisfies the spec 7.4.1 pattern
/// `^ct_[A-Za-z0-9_-]{16,240}$`. Applied at mint — including to
/// caller-supplied overrides — and at ingest, so no other shape enters the
/// system. The pattern is ASCII-only, so byte-wise checks are exact.
pub fn ctx_token_well_formed(token: &str) -> bool {
    let Some(suffix) = token.strip_prefix("ct_") else {
        return false;
    };
    (16..=240).contains(&suffix.len())
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Mint a well-formed ctx token: `ct_` plus 32 bytes from the operating
/// system's CSPRNG, base64url-encoded without padding. That is 256 bits of
/// randomness against the spec's 96-bit floor (7.4.1): holding one token
/// gives no way to derive or enumerate another, and the value encodes no
/// content, session or user identifier.
fn mint_ctx_token() -> String {
    use base64::Engine as _;
    use rand::RngCore as _;

    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!(
        "ct_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Create a click token mapping a click-out event to a session.
///
/// If `token` is None, a fresh `ct_`-prefixed token is minted from the
/// system CSPRNG. Callers passing their own token are responsible for
/// checking [`ctx_token_well_formed`] first (the HTTP layer does); the
/// debug assertion catches library misuse.
pub async fn create_click_token(
    pool: &PgPool,
    session_id: Uuid,
    content_url: &str,
    token: Option<&str>,
) -> Result<ClickTokenRow, sqlx::Error> {
    debug_assert!(
        token.is_none_or(ctx_token_well_formed),
        "caller-supplied ctx tokens must match ^ct_[A-Za-z0-9_-]{{16,240}}$"
    );
    let token_value = token.map_or_else(mint_ctx_token, String::from);

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
/// TODO(spec 7.4.4): this is still the v0.1 click-manifest shape. The v1
/// click context replaces it with four components - the engagement, the
/// clicked content's lineage by content identity, a contributing-source set
/// scoped to the click's turn under the per-owner opt-in gate, and at most
/// a count-based session summary. That narrowing is design work for a
/// separate change; until then this resolver keeps the whole-session scope
/// under the existing consent gates.
///
/// TODO(spec 7.3): events identified only by `content_id` are withheld
/// because owner consent is resolved by domain alone. v1 makes the
/// registered `content_id` prefix a co-primary owner-resolution path;
/// implementing prefix registration and resolution belongs with the click
/// context rebuild.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_tokens_match_the_spec_pattern() {
        for _ in 0..16 {
            let token = mint_ctx_token();
            assert!(ctx_token_well_formed(&token), "minted {token} malformed");
            // 32 bytes base64url without padding is 43 characters.
            assert_eq!(token.len(), "ct_".len() + 43);
        }
        // Distinct mints never collide in practice; two in a row certainly
        // must not (a collision here means the RNG is broken).
        assert_ne!(mint_ctx_token(), mint_ctx_token());
    }

    #[test]
    fn well_formedness_follows_the_spec_grammar() {
        assert!(ctx_token_well_formed("ct_0123456789abcdef"));
        assert!(ctx_token_well_formed(&format!("ct_{}", "a".repeat(240))));

        // Wrong or missing prefix.
        assert!(!ctx_token_well_formed("cx_0123456789abcdef"));
        assert!(!ctx_token_well_formed("0123456789abcdef"));
        // Legacy UUID tokens are not well-formed.
        assert!(!ctx_token_well_formed(
            "d76318b8-4a06-4c48-8929-0c2f9b59d0c8"
        ));
        // Suffix length bounds: 16..=240.
        assert!(!ctx_token_well_formed("ct_012345678901234"));
        assert!(!ctx_token_well_formed(&format!("ct_{}", "a".repeat(241))));
        // Characters outside [A-Za-z0-9_-].
        assert!(!ctx_token_well_formed("ct_0123456789abcde!"));
        assert!(!ctx_token_well_formed("ct_0123456789abcdé"));
        assert!(!ctx_token_well_formed(""));
    }
}
