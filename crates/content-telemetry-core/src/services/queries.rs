use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{domain_like_patterns, effective_domains};
use crate::models::query::{
    AgentAttestedCounts, AgentBreakdown, AgentDomainBreakdown, AgentDomainMetric,
    AgentReconciliation, AgentSummary, DayFunnelCount, DomainPreview, DomainPreviewUrl,
    EventTypeCount, Paginated, PublisherEvent, PublisherSummary, PublisherUrlMetric,
    SourceRoleCount, UnclaimedDomain,
};

/// Get publisher summary with event counts filtered by their domains.
#[allow(clippy::too_many_arguments)]
pub async fn get_publisher_summary(
    pool: &PgPool,
    owner_id: Uuid,
    domains: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    domain_filter: Option<&str>,
    bot: Option<&str>,
    bot_category: Option<&str>,
) -> Result<PublisherSummary, sqlx::Error> {
    let effective = effective_domains(domains, domain_filter);
    let patterns: Vec<String> = domain_like_patterns(&effective);

    if patterns.is_empty() {
        return Ok(empty_summary(owner_id, domains));
    }

    // Query event counts by type, filtered by domain patterns.
    //
    // LEFT JOIN sessions so a bot filter can reach session-attached events.
    // Edge events identify their bot via event_data->>'bot_name'; agent
    // self-report events (grounded/reproduced/cited/presented/engaged)
    // identify it via the session's agent_id, mirroring the agent
    // breakdown's identity model.
    // Without this join, any bot filter silently drops every non-edge funnel
    // stage and the funnel collapses to retrieval-only.
    let mut query = String::from(
        "SELECT e.event_type, COUNT(*) as count, COUNT(DISTINCT e.session_id) as sessions
         FROM events e
         LEFT JOIN sessions s ON e.session_id = s.id
         WHERE (",
    );

    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    query.push_str(&like_clauses.join(" OR "));
    query.push(')');

    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        query.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        query.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }
    if bot.is_some() {
        query.push_str(&format!(
            " AND (e.event_data->>'bot_name' = ${param_idx} OR s.agent_id = ${param_idx})"
        ));
        param_idx += 1;
    }
    if bot_category.is_some() {
        // Category lives on event_data only; agent self-report events carry it
        // when the emitter stamps it (the demo generator does). There is no
        // category column on sessions to fall back to.
        query.push_str(&format!(
            " AND e.event_data->>'bot_category' = ${param_idx}"
        ));
    }
    query.push_str(" GROUP BY e.event_type ORDER BY count DESC");

    let mut q = sqlx::query_as::<_, EventTypeCountRow>(&query);
    for pattern in &patterns {
        q = q.bind(pattern);
    }
    if let Some(ref s) = since {
        q = q.bind(s);
    }
    if let Some(ref u) = until {
        q = q.bind(u);
    }
    if let Some(b) = bot {
        q = q.bind(b);
    }
    if let Some(c) = bot_category {
        q = q.bind(c);
    }

    // Run event counts, source breakdown, and agent breakdown concurrently —
    // they scan the same data independently, so parallelising cuts dashboard latency.
    let (rows, source_rows, agents) = tokio::try_join!(
        q.fetch_all(pool),
        query_source_breakdown(pool, &patterns, since, until, bot, bot_category),
        query_agent_breakdown(pool, &patterns, since, until, bot, bot_category),
    )?;

    let total_events: i64 = rows.iter().map(|r| r.count).sum();
    let total_sessions: i64 = rows.iter().map(|r| r.sessions).max().unwrap_or(0);
    let events_by_type: Vec<EventTypeCount> = rows
        .into_iter()
        .map(|r| EventTypeCount {
            event_type: r.event_type,
            count: r.count,
        })
        .collect();
    let events_by_source: Vec<SourceRoleCount> = source_rows
        .into_iter()
        .map(|r| SourceRoleCount {
            source_role: r.source_role,
            count: r.count,
            sessions: r.sessions,
        })
        .collect();

    Ok(PublisherSummary {
        organization_id: owner_id,
        domains: domains.to_vec(),
        synthetic: false,
        total_events,
        total_sessions,
        events_by_type,
        events_by_source,
        agents,
        period_start: since,
        period_end: until,
    })
}

/// Get per-day funnel counts
/// (retrieved/grounded/reproduced/cited/presented/engaged) for a publisher.
///
/// One row per UTC day in the window. The dashboard chart previously
/// bucketed a sampled events list client-side, which collapsed to "all
/// events on today" whenever the window held more than `limit=500` rows.
/// This computes the buckets server-side over the full event set.
///
/// Returns days in chronological order. Days with zero events are omitted
/// (the chart treats gaps as zero); densifying would just shuffle work
/// from one side of the API boundary to the other.
#[allow(clippy::too_many_arguments)]
pub async fn get_publisher_timeseries(
    pool: &PgPool,
    domains: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    domain_filter: Option<&str>,
    bot: Option<&str>,
    bot_category: Option<&str>,
) -> Result<Vec<DayFunnelCount>, sqlx::Error> {
    let effective = effective_domains(domains, domain_filter);
    let patterns: Vec<String> = domain_like_patterns(&effective);

    if patterns.is_empty() {
        return Ok(vec![]);
    }

    let mut query = String::from(
        "SELECT date_trunc('day', e.event_timestamp)::date AS day,
                COUNT(*) FILTER (WHERE e.event_type = 'content_retrieved') AS retrieved,
                COUNT(*) FILTER (WHERE e.event_type = 'content_grounded') AS grounded,
                COUNT(*) FILTER (WHERE e.event_type = 'content_reproduced') AS reproduced,
                COUNT(*) FILTER (WHERE e.event_type = 'content_cited')    AS cited,
                COUNT(*) FILTER (WHERE e.event_type IN ('content_presented','content_displayed')) AS presented,
                COUNT(*) FILTER (WHERE e.event_type = 'content_engaged')  AS engaged
         FROM events e
         LEFT JOIN sessions s ON e.session_id = s.id
         WHERE (",
    );
    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    query.push_str(&like_clauses.join(" OR "));
    query.push(')');

    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        query.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        query.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }
    if bot.is_some() {
        query.push_str(&format!(
            " AND (e.event_data->>'bot_name' = ${param_idx} OR s.agent_id = ${param_idx})"
        ));
        param_idx += 1;
    }
    if bot_category.is_some() {
        // See get_publisher_summary: category lives on event_data only.
        query.push_str(&format!(
            " AND e.event_data->>'bot_category' = ${param_idx}"
        ));
    }
    query.push_str(
        " AND e.event_type IN ('content_retrieved','content_grounded','content_reproduced','content_cited','content_presented','content_displayed','content_engaged') \
         GROUP BY day ORDER BY day ASC",
    );

    let mut q = sqlx::query_as::<_, (NaiveDate, i64, i64, i64, i64, i64, i64)>(&query);
    for pattern in &patterns {
        q = q.bind(pattern);
    }
    if let Some(ref s) = since {
        q = q.bind(s);
    }
    if let Some(ref u) = until {
        q = q.bind(u);
    }
    if let Some(b) = bot {
        q = q.bind(b);
    }
    if let Some(c) = bot_category {
        q = q.bind(c);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(
            |(date, retrieved, grounded, reproduced, cited, presented, engaged)| DayFunnelCount {
                date,
                retrieved,
                grounded,
                reproduced,
                cited,
                presented,
                engaged,
            },
        )
        .collect())
}

/// Get paginated events for an owner's domains.
#[allow(clippy::too_many_arguments)]
pub async fn get_publisher_events(
    pool: &PgPool,
    domains: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    domain_filter: Option<&str>,
    bot: Option<&str>,
    bot_category: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Paginated<PublisherEvent>, sqlx::Error> {
    let effective = effective_domains(domains, domain_filter);
    let patterns: Vec<String> = domain_like_patterns(&effective);

    if patterns.is_empty() {
        return Ok(Paginated {
            items: vec![],
            total: 0,
            limit,
            offset,
            synthetic: None,
        });
    }

    let (count_sql, data_sql) =
        build_publisher_event_queries(&patterns, since, until, bot, bot_category);

    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for p in &patterns {
        count_q = count_q.bind(p);
    }
    if let Some(ref s) = since {
        count_q = count_q.bind(s);
    }
    if let Some(ref u) = until {
        count_q = count_q.bind(u);
    }
    if let Some(b) = bot {
        count_q = count_q.bind(b);
    }
    if let Some(c) = bot_category {
        count_q = count_q.bind(c);
    }
    let optional_count = usize::from(since.is_some())
        + usize::from(until.is_some())
        + usize::from(bot.is_some())
        + usize::from(bot_category.is_some());
    let full_data_sql = format!(
        "{data_sql} LIMIT ${} OFFSET ${}",
        patterns.len() + 1 + optional_count,
        patterns.len() + 2 + optional_count,
    );
    let mut data_q = sqlx::query_as::<_, PublisherEventRow>(&full_data_sql);
    for p in &patterns {
        data_q = data_q.bind(p);
    }
    if let Some(ref s) = since {
        data_q = data_q.bind(s);
    }
    if let Some(ref u) = until {
        data_q = data_q.bind(u);
    }
    if let Some(b) = bot {
        data_q = data_q.bind(b);
    }
    if let Some(c) = bot_category {
        data_q = data_q.bind(c);
    }
    data_q = data_q.bind(limit).bind(offset);

    let (total, rows) = tokio::try_join!(count_q.fetch_one(pool), data_q.fetch_all(pool),)?;
    let items = rows
        .into_iter()
        .map(|r| PublisherEvent {
            event_id: r.id,
            session_id: r.session_id,
            event_type: r.event_type,
            source_role: r.source_role,
            content_telemetry_id: r.content_telemetry_id,
            content_url: r.content_url,
            event_timestamp: r.event_timestamp,
            event_data: r.event_data,
            platform_id: r.platform_id,
            agent_id: r.agent_id,
        })
        .collect();

    Ok(Paginated {
        items,
        total,
        limit,
        offset,
        synthetic: None,
    })
}

/// Get URL-level metrics for an owner's domains.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn get_publisher_url_metrics(
    pool: &PgPool,
    domains: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    domain_filter: Option<&str>,
    bot: Option<&str>,
    bot_category: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Paginated<PublisherUrlMetric>, sqlx::Error> {
    let effective = effective_domains(domains, domain_filter);
    let patterns = domain_like_patterns(&effective);

    if patterns.is_empty() {
        return Ok(Paginated {
            items: vec![],
            total: 0,
            limit,
            offset,
            synthetic: None,
        });
    }

    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let where_like = like_clauses.join(" OR ");

    let mut time_filter = String::new();
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }
    if bot.is_some() {
        time_filter.push_str(&format!(" AND e.event_data->>'bot_name' = ${param_idx}"));
        param_idx += 1;
    }
    if bot_category.is_some() {
        time_filter.push_str(&format!(
            " AND e.event_data->>'bot_category' = ${param_idx}"
        ));
        param_idx += 1;
    }

    // Normalise trailing slashes so /policycheck and /policycheck/ aggregate
    // into one row. CDNs and crawlers visit both forms; in the dashboard they
    // are the same piece of content.
    let url_expr = "regexp_replace(e.content_url, '/+$', '')";

    let count_sql = format!(
        "SELECT COUNT(DISTINCT {url_expr}) FROM events e WHERE ({where_like}){time_filter} AND e.content_url IS NOT NULL"
    );
    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for p in &patterns {
        count_q = count_q.bind(p);
    }
    if let Some(ref s) = since {
        count_q = count_q.bind(s);
    }
    if let Some(ref u) = until {
        count_q = count_q.bind(u);
    }
    if let Some(b) = bot {
        count_q = count_q.bind(b);
    }
    if let Some(c) = bot_category {
        count_q = count_q.bind(c);
    }
    let data_sql = format!(
        "SELECT {url_expr} as content_url, COUNT(*) as total_events,
                COUNT(DISTINCT e.session_id) as unique_sessions,
                MAX(e.event_timestamp) as last_seen
         FROM events e
         WHERE ({where_like}){time_filter} AND e.content_url IS NOT NULL
         GROUP BY {url_expr}
         ORDER BY total_events DESC
         LIMIT ${param_idx} OFFSET ${}",
        param_idx + 1
    );

    let mut data_q = sqlx::query_as::<_, UrlMetricRow>(&data_sql);
    for p in &patterns {
        data_q = data_q.bind(p);
    }
    if let Some(ref s) = since {
        data_q = data_q.bind(s);
    }
    if let Some(ref u) = until {
        data_q = data_q.bind(u);
    }
    if let Some(b) = bot {
        data_q = data_q.bind(b);
    }
    if let Some(c) = bot_category {
        data_q = data_q.bind(c);
    }
    data_q = data_q.bind(limit).bind(offset);

    let (total, rows) = tokio::try_join!(count_q.fetch_one(pool), data_q.fetch_all(pool),)?;

    // Batch-fetch event type breakdowns for all URLs in one query (avoids N+1).
    // Match on the normalised (trailing-slash-stripped) URL so the breakdown
    // lines up with the rows above.
    let urls: Vec<String> = rows.iter().map(|r| r.content_url.clone()).collect();
    let type_breakdown = if urls.is_empty() {
        vec![]
    } else {
        let tb_url_expr = "regexp_replace(content_url, '/+$', '')";
        let mut tb_sql = format!(
            "SELECT {tb_url_expr} as content_url, event_type, COUNT(*) as count, 0::bigint as sessions
             FROM events WHERE {tb_url_expr} = ANY($1)"
        );
        let mut tb_param_idx = 2;
        if since.is_some() {
            tb_sql.push_str(&format!(" AND event_timestamp >= ${tb_param_idx}"));
            tb_param_idx += 1;
        }
        if until.is_some() {
            tb_sql.push_str(&format!(" AND event_timestamp <= ${tb_param_idx}"));
            tb_param_idx += 1;
        }
        if bot.is_some() {
            tb_sql.push_str(&format!(" AND event_data->>'bot_name' = ${tb_param_idx}"));
            tb_param_idx += 1;
        }
        if bot_category.is_some() {
            tb_sql.push_str(&format!(
                " AND event_data->>'bot_category' = ${tb_param_idx}"
            ));
        }
        tb_sql.push_str(&format!(
            " GROUP BY {tb_url_expr}, event_type ORDER BY content_url, count DESC"
        ));

        let mut tb_q = sqlx::query_as::<_, UrlEventTypeRow>(&tb_sql).bind(&urls);
        if let Some(ref s) = since {
            tb_q = tb_q.bind(s);
        }
        if let Some(ref u) = until {
            tb_q = tb_q.bind(u);
        }
        if let Some(b) = bot {
            tb_q = tb_q.bind(b);
        }
        if let Some(c) = bot_category {
            tb_q = tb_q.bind(c);
        }
        tb_q.fetch_all(pool).await?
    };

    // Group type breakdowns by URL
    let mut type_map: std::collections::HashMap<String, Vec<EventTypeCount>> =
        std::collections::HashMap::new();
    for tb in type_breakdown {
        type_map
            .entry(tb.content_url)
            .or_default()
            .push(EventTypeCount {
                event_type: tb.event_type,
                count: tb.count,
            });
    }

    let items: Vec<PublisherUrlMetric> = rows
        .into_iter()
        .map(|r| {
            let event_types = type_map.remove(&r.content_url).unwrap_or_default();
            PublisherUrlMetric {
                content_url: r.content_url,
                total_events: r.total_events,
                unique_sessions: r.unique_sessions,
                event_types,
                last_seen: r.last_seen,
            }
        })
        .collect();

    Ok(Paginated {
        items,
        total,
        limit,
        offset,
        synthetic: None,
    })
}

// ---------------------------------------------------------------------------
// Agent queries
// ---------------------------------------------------------------------------

/// Get agent summary: event counts and top domains accessed by this org's sessions.
pub async fn get_agent_summary(
    pool: &PgPool,
    org_id: Uuid,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<AgentSummary, sqlx::Error> {
    let mut time_filter = String::new();
    let mut param_idx = 2u32;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
    }

    let type_sql = format!(
        "SELECT e.event_type, COUNT(*) as count, COUNT(DISTINCT e.session_id) as sessions
         FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1{time_filter}
         GROUP BY e.event_type ORDER BY count DESC"
    );

    let domain_sql = format!(
        "SELECT substring(e.content_url from 'https?://([^/]+)') as domain,
                COUNT(*) as event_count,
                COUNT(DISTINCT e.session_id) as session_count
         FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1{time_filter} AND e.content_url IS NOT NULL
         GROUP BY domain
         HAVING substring(e.content_url from 'https?://([^/]+)') IS NOT NULL
         ORDER BY event_count DESC
         LIMIT 50"
    );

    let mut type_q = sqlx::query_as::<_, EventTypeCountRow>(&type_sql).bind(org_id);
    if let Some(ref s) = since {
        type_q = type_q.bind(s);
    }
    if let Some(ref u) = until {
        type_q = type_q.bind(u);
    }

    let mut domain_q = sqlx::query_as::<_, AgentDomainRow>(&domain_sql).bind(org_id);
    if let Some(ref s) = since {
        domain_q = domain_q.bind(s);
    }
    if let Some(ref u) = until {
        domain_q = domain_q.bind(u);
    }

    let (type_rows, domain_rows) =
        tokio::try_join!(type_q.fetch_all(pool), domain_q.fetch_all(pool))?;

    let total_events: i64 = type_rows.iter().map(|r| r.count).sum();
    let total_sessions: i64 = type_rows.iter().map(|r| r.sessions).max().unwrap_or(0);
    let events_by_type = type_rows
        .into_iter()
        .map(|r| EventTypeCount {
            event_type: r.event_type,
            count: r.count,
        })
        .collect();
    let domains = domain_rows
        .into_iter()
        .map(|r| AgentDomainBreakdown {
            domain: r.domain,
            event_count: r.event_count,
            session_count: r.session_count,
        })
        .collect();

    Ok(AgentSummary {
        organization_id: org_id,
        total_events,
        total_sessions,
        events_by_type,
        domains,
        period_start: since,
        period_end: until,
    })
}

/// Per-day funnel counts for an agent org's session events.
///
/// Mirror of `get_publisher_timeseries` but scoped to sessions owned by
/// `org_id` instead of by domain match. Powers the agent dashboard's
/// "Daily interactions" chart.
pub async fn get_agent_timeseries(
    pool: &PgPool,
    org_id: Uuid,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<Vec<DayFunnelCount>, sqlx::Error> {
    let mut query = String::from(
        "SELECT date_trunc('day', e.event_timestamp)::date AS day,
                COUNT(*) FILTER (WHERE e.event_type = 'content_retrieved') AS retrieved,
                COUNT(*) FILTER (WHERE e.event_type = 'content_grounded') AS grounded,
                COUNT(*) FILTER (WHERE e.event_type = 'content_reproduced') AS reproduced,
                COUNT(*) FILTER (WHERE e.event_type = 'content_cited')    AS cited,
                COUNT(*) FILTER (WHERE e.event_type IN ('content_presented','content_displayed')) AS presented,
                COUNT(*) FILTER (WHERE e.event_type = 'content_engaged')  AS engaged
         FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1",
    );
    let mut param_idx = 2u32;
    if since.is_some() {
        query.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        query.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
    }
    query.push_str(
        " AND e.event_type IN ('content_retrieved','content_grounded','content_reproduced','content_cited','content_presented','content_displayed','content_engaged') \
         GROUP BY day ORDER BY day ASC",
    );

    let mut q = sqlx::query_as::<_, (NaiveDate, i64, i64, i64, i64, i64, i64)>(&query).bind(org_id);
    if let Some(ref s) = since {
        q = q.bind(s);
    }
    if let Some(ref u) = until {
        q = q.bind(u);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(
            |(date, retrieved, grounded, reproduced, cited, presented, engaged)| DayFunnelCount {
                date,
                retrieved,
                grounded,
                reproduced,
                cited,
                presented,
                engaged,
            },
        )
        .collect())
}

/// Get paginated events for an agent org's sessions.
pub async fn get_agent_events(
    pool: &PgPool,
    org_id: Uuid,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: i64,
    offset: i64,
) -> Result<Paginated<PublisherEvent>, sqlx::Error> {
    let mut time_filter = String::new();
    let mut param_idx = 2u32;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }

    let count_sql = format!(
        "SELECT COUNT(*) FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1{time_filter}"
    );

    let data_sql = format!(
        "SELECT e.id, e.session_id, e.event_type, e.source_role, e.content_telemetry_id,
                e.content_url, e.event_timestamp, e.event_data,
                s.platform_id, s.agent_id
         FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1{time_filter}
         ORDER BY e.event_timestamp DESC
         LIMIT ${param_idx} OFFSET ${}",
        param_idx + 1
    );

    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql).bind(org_id);
    if let Some(ref s) = since {
        count_q = count_q.bind(s);
    }
    if let Some(ref u) = until {
        count_q = count_q.bind(u);
    }

    let mut data_q = sqlx::query_as::<_, PublisherEventRow>(&data_sql).bind(org_id);
    if let Some(ref s) = since {
        data_q = data_q.bind(s);
    }
    if let Some(ref u) = until {
        data_q = data_q.bind(u);
    }
    data_q = data_q.bind(limit).bind(offset);

    let (total, rows) = tokio::try_join!(count_q.fetch_one(pool), data_q.fetch_all(pool))?;
    let items = rows
        .into_iter()
        .map(|r| PublisherEvent {
            event_id: r.id,
            session_id: r.session_id,
            event_type: r.event_type,
            source_role: r.source_role,
            content_telemetry_id: r.content_telemetry_id,
            content_url: r.content_url,
            event_timestamp: r.event_timestamp,
            event_data: r.event_data,
            platform_id: r.platform_id,
            agent_id: r.agent_id,
        })
        .collect();

    Ok(Paginated {
        items,
        total,
        limit,
        offset,
        synthetic: None,
    })
}

/// Get domain-level metrics for content accessed by an agent org.
#[allow(clippy::too_many_lines)]
pub async fn get_agent_domain_metrics(
    pool: &PgPool,
    org_id: Uuid,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: i64,
    offset: i64,
) -> Result<Paginated<AgentDomainMetric>, sqlx::Error> {
    let mut time_filter = String::new();
    let mut param_idx = 2u32;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }

    let count_sql = format!(
        "SELECT COUNT(DISTINCT substring(e.content_url from 'https?://([^/]+)'))
         FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1{time_filter} AND e.content_url IS NOT NULL"
    );

    let data_sql = format!(
        "SELECT substring(e.content_url from 'https?://([^/]+)') as domain,
                COUNT(*) as total_events,
                COUNT(DISTINCT e.session_id) as unique_sessions,
                MAX(e.event_timestamp) as last_seen
         FROM events e
         JOIN sessions s ON e.session_id = s.id
         WHERE s.organization_id = $1{time_filter} AND e.content_url IS NOT NULL
         GROUP BY domain
         HAVING substring(e.content_url from 'https?://([^/]+)') IS NOT NULL
         ORDER BY total_events DESC
         LIMIT ${param_idx} OFFSET ${}",
        param_idx + 1
    );

    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql).bind(org_id);
    if let Some(ref s) = since {
        count_q = count_q.bind(s);
    }
    if let Some(ref u) = until {
        count_q = count_q.bind(u);
    }

    let mut data_q = sqlx::query_as::<_, DomainMetricRow>(&data_sql).bind(org_id);
    if let Some(ref s) = since {
        data_q = data_q.bind(s);
    }
    if let Some(ref u) = until {
        data_q = data_q.bind(u);
    }
    data_q = data_q.bind(limit).bind(offset);

    let (total, rows) = tokio::try_join!(count_q.fetch_one(pool), data_q.fetch_all(pool))?;

    // Batch-fetch event type breakdowns per domain
    let domain_names: Vec<String> = rows.iter().map(|r| r.domain.clone()).collect();
    let type_breakdown = if domain_names.is_empty() {
        vec![]
    } else {
        let mut tb_sql = String::from(
            "SELECT substring(e.content_url from 'https?://([^/]+)') as domain,
                    e.event_type, COUNT(*) as count, 0::bigint as sessions
             FROM events e
             JOIN sessions s ON e.session_id = s.id
             WHERE s.organization_id = $1
               AND substring(e.content_url from 'https?://([^/]+)') = ANY($2)",
        );
        let mut tb_param_idx = 3u32;
        if since.is_some() {
            tb_sql.push_str(&format!(" AND e.event_timestamp >= ${tb_param_idx}"));
            tb_param_idx += 1;
        }
        if until.is_some() {
            tb_sql.push_str(&format!(" AND e.event_timestamp <= ${tb_param_idx}"));
        }
        let _ = tb_param_idx;
        tb_sql.push_str(" GROUP BY domain, e.event_type ORDER BY domain, count DESC");

        let mut tb_q = sqlx::query_as::<_, DomainEventTypeRow>(&tb_sql)
            .bind(org_id)
            .bind(&domain_names);
        if let Some(ref s) = since {
            tb_q = tb_q.bind(s);
        }
        if let Some(ref u) = until {
            tb_q = tb_q.bind(u);
        }
        tb_q.fetch_all(pool).await?
    };

    let mut type_map: std::collections::HashMap<String, Vec<EventTypeCount>> =
        std::collections::HashMap::new();
    for tb in type_breakdown {
        type_map.entry(tb.domain).or_default().push(EventTypeCount {
            event_type: tb.event_type,
            count: tb.count,
        });
    }

    let items = rows
        .into_iter()
        .map(|r| {
            let event_types = type_map.remove(&r.domain).unwrap_or_default();
            AgentDomainMetric {
                domain: r.domain,
                total_events: r.total_events,
                unique_sessions: r.unique_sessions,
                event_types,
                last_seen: r.last_seen,
            }
        })
        .collect();

    Ok(Paginated {
        items,
        total,
        limit,
        offset,
        synthetic: None,
    })
}

// ---------------------------------------------------------------------------
// Admin queries
// ---------------------------------------------------------------------------

/// Get a preview of telemetry activity for any domain (admin use).
///
/// Same content_url LIKE matching as publisher queries but without requiring
/// org ownership. Reports registration status from the domains table.
pub async fn get_domain_preview(
    pool: &PgPool,
    domain: &str,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<DomainPreview, sqlx::Error> {
    let patterns = domain_like_patterns(&[domain]);

    // Check registration status.
    let registration: Option<Uuid> = sqlx::query_scalar(
        "SELECT organization_id FROM domains WHERE domain = $1 AND verified_at IS NOT NULL",
    )
    .bind(domain)
    .fetch_optional(pool)
    .await?;

    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let where_like = like_clauses.join(" OR ");

    let mut time_filter = String::new();
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }

    let type_sql = format!(
        "SELECT e.event_type, COUNT(*) as count, COUNT(DISTINCT e.session_id) as sessions
         FROM events e
         WHERE ({where_like}){time_filter}
         GROUP BY e.event_type ORDER BY count DESC"
    );

    let url_sql = format!(
        "SELECT e.content_url, COUNT(*) as event_count,
                MAX(e.event_timestamp) as last_seen
         FROM events e
         WHERE ({where_like}){time_filter} AND e.content_url IS NOT NULL
         GROUP BY e.content_url
         ORDER BY event_count DESC
         LIMIT ${param_idx}"
    );
    let url_limit: i64 = 20;

    let mut type_q = sqlx::query_as::<_, EventTypeCountRow>(&type_sql);
    for p in &patterns {
        type_q = type_q.bind(p);
    }
    if let Some(ref s) = since {
        type_q = type_q.bind(s);
    }
    if let Some(ref u) = until {
        type_q = type_q.bind(u);
    }

    let mut url_q = sqlx::query_as::<_, DomainPreviewUrlRow>(&url_sql);
    for p in &patterns {
        url_q = url_q.bind(p);
    }
    if let Some(ref s) = since {
        url_q = url_q.bind(s);
    }
    if let Some(ref u) = until {
        url_q = url_q.bind(u);
    }
    url_q = url_q.bind(url_limit);

    let (type_rows, url_rows, agents) = tokio::try_join!(
        type_q.fetch_all(pool),
        url_q.fetch_all(pool),
        query_agent_breakdown(pool, &patterns, since, until, None, None),
    )?;

    let total_events: i64 = type_rows.iter().map(|r| r.count).sum();
    let events_by_type = type_rows
        .into_iter()
        .map(|r| EventTypeCount {
            event_type: r.event_type,
            count: r.count,
        })
        .collect();
    let top_urls = url_rows
        .into_iter()
        .map(|r| DomainPreviewUrl {
            content_url: r.content_url,
            event_count: r.event_count,
            last_seen: r.last_seen,
        })
        .collect();

    Ok(DomainPreview {
        domain: domain.to_string(),
        registered: registration.is_some(),
        organization_id: registration,
        total_events,
        events_by_type,
        top_urls,
        agents,
        period_start: since,
        period_end: until,
    })
}

/// List domains that have telemetry events but no verified owner.
///
/// Extracts domains from event content_urls, filters out those with a
/// verified entry in the domains table. Normalises www. prefix for matching.
pub async fn get_unclaimed_domains(
    pool: &PgPool,
    min_events: i64,
    limit: i64,
    offset: i64,
) -> Result<Paginated<UnclaimedDomain>, sqlx::Error> {
    // Normalise extracted domains: strip www. prefix to match domains table.
    let domain_expr =
        "regexp_replace(substring(e.content_url from 'https?://([^/]+)'), '^www\\.', '')";

    let count_sql = format!(
        "SELECT COUNT(*) FROM (
            SELECT {domain_expr} as domain
            FROM events e
            WHERE e.content_url IS NOT NULL
            GROUP BY domain
            HAVING {domain_expr} IS NOT NULL
               AND COUNT(*) >= $1
               AND {domain_expr} NOT IN (
                   SELECT d.domain FROM domains d WHERE d.verified_at IS NOT NULL
               )
        ) sub"
    );

    let data_sql = format!(
        "SELECT {domain_expr} as domain,
                COUNT(*) as total_events,
                MIN(e.event_timestamp) as first_seen,
                MAX(e.event_timestamp) as last_seen
         FROM events e
         WHERE e.content_url IS NOT NULL
         GROUP BY domain
         HAVING {domain_expr} IS NOT NULL
            AND COUNT(*) >= $1
            AND {domain_expr} NOT IN (
                SELECT d.domain FROM domains d WHERE d.verified_at IS NOT NULL
            )
         ORDER BY total_events DESC
         LIMIT $2 OFFSET $3"
    );

    let (total, rows) = tokio::try_join!(
        sqlx::query_scalar::<_, i64>(&count_sql)
            .bind(min_events)
            .fetch_one(pool),
        sqlx::query_as::<_, UnclaimedDomainRow>(&data_sql)
            .bind(min_events)
            .bind(limit)
            .bind(offset)
            .fetch_all(pool),
    )?;

    // Batch-fetch event type breakdowns for the returned domains.
    let domain_names: Vec<String> = rows.iter().map(|r| r.domain.clone()).collect();
    let type_breakdown = if domain_names.is_empty() {
        vec![]
    } else {
        sqlx::query_as::<_, DomainEventTypeRow>(
            "SELECT regexp_replace(substring(e.content_url from 'https?://([^/]+)'), '^www\\.', '') as domain,
                    e.event_type, COUNT(*) as count, 0::bigint as sessions
             FROM events e
             WHERE regexp_replace(substring(e.content_url from 'https?://([^/]+)'), '^www\\.', '') = ANY($1)
             GROUP BY domain, e.event_type
             ORDER BY domain, count DESC",
        )
        .bind(&domain_names)
        .fetch_all(pool)
        .await?
    };

    let mut type_map: std::collections::HashMap<String, Vec<EventTypeCount>> =
        std::collections::HashMap::new();
    for tb in type_breakdown {
        type_map.entry(tb.domain).or_default().push(EventTypeCount {
            event_type: tb.event_type,
            count: tb.count,
        });
    }

    let items = rows
        .into_iter()
        .map(|r| {
            let event_types = type_map.remove(&r.domain).unwrap_or_default();
            UnclaimedDomain {
                domain: r.domain,
                total_events: r.total_events,
                first_seen: r.first_seen,
                last_seen: r.last_seen,
                event_types,
            }
        })
        .collect();

    Ok(Paginated {
        items,
        total,
        limit,
        offset,
        synthetic: None,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn build_publisher_event_queries(
    patterns: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    bot: Option<&str>,
    bot_category: Option<&str>,
) -> (String, String) {
    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let where_like = like_clauses.join(" OR ");

    let mut time_filter = String::new();
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }
    if bot.is_some() {
        time_filter.push_str(&format!(" AND e.event_data->>'bot_name' = ${param_idx}"));
        param_idx += 1;
    }
    if bot_category.is_some() {
        time_filter.push_str(&format!(
            " AND e.event_data->>'bot_category' = ${param_idx}"
        ));
    }

    let count_sql = format!("SELECT COUNT(*) FROM events e WHERE ({where_like}){time_filter}");
    let data_sql = format!(
        "SELECT e.id, e.session_id, e.event_type, e.source_role, e.content_telemetry_id,
                e.content_url, e.event_timestamp, e.event_data,
                s.platform_id, s.agent_id
         FROM events e
         LEFT JOIN sessions s ON e.session_id = s.id
         WHERE ({where_like}){time_filter}
         ORDER BY e.event_timestamp DESC"
    );

    (count_sql, data_sql)
}

async fn query_source_breakdown(
    pool: &PgPool,
    patterns: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    bot: Option<&str>,
    bot_category: Option<&str>,
) -> Result<Vec<SourceRoleRow>, sqlx::Error> {
    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let where_like = like_clauses.join(" OR ");

    let mut time_filter = String::new();
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }
    if bot.is_some() {
        time_filter.push_str(&format!(" AND e.event_data->>'bot_name' = ${param_idx}"));
        param_idx += 1;
    }
    if bot_category.is_some() {
        time_filter.push_str(&format!(
            " AND e.event_data->>'bot_category' = ${param_idx}"
        ));
    }

    let sql = format!(
        "SELECT e.source_role,
                COUNT(*) as count,
                COUNT(DISTINCT e.session_id) as sessions
         FROM events e
         WHERE ({where_like}){time_filter}
         GROUP BY e.source_role
         ORDER BY count DESC"
    );

    let mut q = sqlx::query_as::<_, SourceRoleRow>(&sql);
    for p in patterns {
        q = q.bind(p);
    }
    if let Some(ref s) = since {
        q = q.bind(s);
    }
    if let Some(ref u) = until {
        q = q.bind(u);
    }
    if let Some(b) = bot {
        q = q.bind(b);
    }
    if let Some(c) = bot_category {
        q = q.bind(c);
    }

    q.fetch_all(pool).await
}

async fn query_agent_breakdown(
    pool: &PgPool,
    patterns: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    bot: Option<&str>,
    bot_category: Option<&str>,
) -> Result<Vec<AgentBreakdown>, sqlx::Error> {
    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let where_like = like_clauses.join(" OR ");

    let mut time_filter = String::new();
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_filter.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
        param_idx += 1;
    }
    if bot.is_some() {
        time_filter.push_str(&format!(" AND e.event_data->>'bot_name' = ${param_idx}"));
        param_idx += 1;
    }
    if bot_category.is_some() {
        time_filter.push_str(&format!(
            " AND e.event_data->>'bot_category' = ${param_idx}"
        ));
    }

    // Group key. Session-attached events have agent_id; edge events do not, so
    // fall back to the bot identity the edge worker stamps onto event_data.
    // Without this, every edge retrieval collapses into a single "Unknown" row.
    let bot_name_expr = "NULLIF(e.event_data->>'bot_name', '')";
    let bot_category_expr = "NULLIF(e.event_data->>'bot_category', '')";

    // Get totals per agent
    let totals_sql = format!(
        "SELECT s.platform_id, s.agent_id,
                {bot_name_expr} as bot_name,
                {bot_category_expr} as bot_category,
                COUNT(*) as event_count,
                COUNT(DISTINCT e.session_id) as session_count
         FROM events e
         LEFT JOIN sessions s ON e.session_id = s.id
         WHERE ({where_like}){time_filter}
         GROUP BY s.platform_id, s.agent_id, bot_name, bot_category
         ORDER BY event_count DESC"
    );

    // Get per-agent source_role breakdown
    let source_sql = format!(
        "SELECT s.platform_id, s.agent_id,
                {bot_name_expr} as bot_name,
                {bot_category_expr} as bot_category,
                e.source_role,
                COUNT(*) as count,
                COUNT(DISTINCT e.session_id) as sessions
         FROM events e
         LEFT JOIN sessions s ON e.session_id = s.id
         WHERE ({where_like}){time_filter}
         GROUP BY s.platform_id, s.agent_id, bot_name, bot_category, e.source_role
         ORDER BY s.platform_id, s.agent_id, count DESC"
    );

    // Get per-agent event_type breakdown (used for citation rate)
    let type_sql = format!(
        "SELECT s.platform_id, s.agent_id,
                {bot_name_expr} as bot_name,
                {bot_category_expr} as bot_category,
                e.event_type,
                COUNT(*) as count
         FROM events e
         LEFT JOIN sessions s ON e.session_id = s.id
         WHERE ({where_like}){time_filter}
         GROUP BY s.platform_id, s.agent_id, bot_name, bot_category, e.event_type
         ORDER BY s.platform_id, s.agent_id, count DESC"
    );

    let mut totals_q = sqlx::query_as::<_, AgentBreakdownRow>(&totals_sql);
    for p in patterns {
        totals_q = totals_q.bind(p);
    }
    if let Some(ref s) = since {
        totals_q = totals_q.bind(s);
    }
    if let Some(ref u) = until {
        totals_q = totals_q.bind(u);
    }
    if let Some(b) = bot {
        totals_q = totals_q.bind(b);
    }
    if let Some(c) = bot_category {
        totals_q = totals_q.bind(c);
    }

    let mut source_q = sqlx::query_as::<_, AgentSourceRow>(&source_sql);
    for p in patterns {
        source_q = source_q.bind(p);
    }
    if let Some(ref s) = since {
        source_q = source_q.bind(s);
    }
    if let Some(ref u) = until {
        source_q = source_q.bind(u);
    }
    if let Some(b) = bot {
        source_q = source_q.bind(b);
    }
    if let Some(c) = bot_category {
        source_q = source_q.bind(c);
    }

    let mut type_q = sqlx::query_as::<_, AgentTypeRow>(&type_sql);
    for p in patterns {
        type_q = type_q.bind(p);
    }
    if let Some(ref s) = since {
        type_q = type_q.bind(s);
    }
    if let Some(ref u) = until {
        type_q = type_q.bind(u);
    }
    if let Some(b) = bot {
        type_q = type_q.bind(b);
    }
    if let Some(c) = bot_category {
        type_q = type_q.bind(c);
    }

    let (totals, sources, types) = tokio::try_join!(
        totals_q.fetch_all(pool),
        source_q.fetch_all(pool),
        type_q.fetch_all(pool)
    )?;

    type BreakdownKey = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    // Group source breakdowns by (platform_id, agent_id, bot_name, bot_category)
    let mut source_map: std::collections::HashMap<BreakdownKey, Vec<SourceRoleCount>> =
        std::collections::HashMap::new();
    for s in sources {
        source_map
            .entry((s.platform_id, s.agent_id, s.bot_name, s.bot_category))
            .or_default()
            .push(SourceRoleCount {
                source_role: s.source_role,
                count: s.count,
                sessions: s.sessions,
            });
    }

    // Group event_type breakdowns by (platform_id, agent_id, bot_name, bot_category)
    let mut type_map: std::collections::HashMap<BreakdownKey, Vec<EventTypeCount>> =
        std::collections::HashMap::new();
    for t in types {
        type_map
            .entry((t.platform_id, t.agent_id, t.bot_name, t.bot_category))
            .or_default()
            .push(EventTypeCount {
                event_type: t.event_type,
                count: t.count,
            });
    }

    Ok(totals
        .into_iter()
        .map(|r| {
            let key = (
                r.platform_id.clone(),
                r.agent_id.clone(),
                r.bot_name.clone(),
                r.bot_category.clone(),
            );
            let by_source = source_map.remove(&key).unwrap_or_default();
            let by_event_type = type_map.remove(&key).unwrap_or_default();
            AgentBreakdown {
                platform_id: r.platform_id,
                agent_id: r.agent_id,
                bot_name: r.bot_name,
                bot_category: r.bot_category,
                event_count: r.event_count,
                session_count: r.session_count,
                by_source,
                by_event_type,
            }
        })
        .collect())
}

/// Per-agent edge-vs-self-report reconciliation over a publisher's domains
/// self-reports.
///
/// Two independent tallies joined on agent identity: what the publisher's
/// own edge emitter observed (`source_role='edge'` retrievals, identity
/// from `event_data.bot_name`) against what agents self-reported
/// (session-attached `source_role='agent'` events, identity from the
/// session's `agent_id`). A FULL OUTER JOIN keeps both tails: edge-only
/// identities (crawlers and non-reporting agents) and self-only identities
/// (edge coverage gaps, naming mismatches, or reports about fetches the
/// edge never served). Nothing is silently dropped — unattributed edge
/// events group under a NULL identity.
#[allow(clippy::cast_precision_loss)]
pub async fn get_publisher_reconciliation(
    pool: &PgPool,
    domains: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    domain_filter: Option<&str>,
) -> Result<Vec<AgentReconciliation>, sqlx::Error> {
    let effective = effective_domains(domains, domain_filter);
    let patterns: Vec<String> = domain_like_patterns(&effective);

    if patterns.is_empty() {
        return Ok(vec![]);
    }

    let like_clauses = |first: usize| -> String {
        (first..first + patterns.len())
            .map(|i| format!("e.content_url LIKE ${i}"))
            .collect::<Vec<_>>()
            .join(" OR ")
    };

    // Bind order: patterns once ($1..$n, reused by both CTEs via the same
    // placeholders), then optional since/until.
    let mut time_clause = String::new();
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        time_clause.push_str(&format!(" AND e.event_timestamp >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        time_clause.push_str(&format!(" AND e.event_timestamp <= ${param_idx}"));
    }

    let query = format!(
        "WITH edge AS (
             SELECT e.event_data->>'bot_name' AS identity,
                    COUNT(*) AS edge_retrievals
             FROM events e
             WHERE e.source_role = 'edge'
               AND e.event_type = 'content_retrieved'
               AND ({patterns_clause}){time_clause}
             GROUP BY 1
         ),
         self_report AS (
             SELECT s.agent_id AS identity,
                    COUNT(*) FILTER (WHERE e.event_type = 'content_retrieved') AS self_retrievals,
                    COUNT(*) FILTER (WHERE e.event_type = 'content_grounded')  AS grounded,
                    COUNT(*) FILTER (WHERE e.event_type = 'content_cited')     AS cited,
                    COUNT(*) FILTER (WHERE e.event_type = 'content_displayed') AS displayed,
                    COUNT(*) FILTER (WHERE e.event_type = 'content_engaged')   AS engaged,
                    COUNT(DISTINCT e.session_id) AS sessions_reporting
             FROM events e
             JOIN sessions s ON e.session_id = s.id
             WHERE e.source_role = 'agent'
               AND ({patterns_clause}){time_clause}
             GROUP BY 1
         )
         SELECT COALESCE(edge.identity, self_report.identity) AS identity,
                COALESCE(edge.edge_retrievals, 0)      AS edge_retrievals,
                COALESCE(self_report.self_retrievals, 0) AS self_reported_retrievals,
                COALESCE(self_report.grounded, 0)      AS grounded,
                COALESCE(self_report.cited, 0)         AS cited,
                COALESCE(self_report.displayed, 0)     AS displayed,
                COALESCE(self_report.engaged, 0)       AS engaged,
                COALESCE(self_report.sessions_reporting, 0) AS sessions_reporting
         FROM edge
         FULL OUTER JOIN self_report ON edge.identity = self_report.identity
         ORDER BY COALESCE(edge.edge_retrievals, 0) DESC,
                  COALESCE(self_report.self_retrievals, 0) DESC",
        patterns_clause = like_clauses(1),
        time_clause = time_clause,
    );

    let mut q = sqlx::query_as::<_, ReconciliationRow>(&query);
    for pattern in &patterns {
        q = q.bind(pattern);
    }
    if let Some(ref s) = since {
        q = q.bind(s);
    }
    if let Some(ref u) = until {
        q = q.bind(u);
    }
    let rows = q.fetch_all(pool).await?;

    Ok(rows
        .into_iter()
        .map(|r| AgentReconciliation {
            identity: r.identity,
            edge_retrievals: r.edge_retrievals,
            self_reported_retrievals: r.self_reported_retrievals,
            self_report_ratio: (r.edge_retrievals > 0)
                .then(|| r.self_reported_retrievals as f64 / r.edge_retrievals as f64),
            sessions_reporting: r.sessions_reporting,
            agent_attested: AgentAttestedCounts {
                grounded: r.grounded,
                cited: r.cited,
                displayed: r.displayed,
                engaged: r.engaged,
            },
        })
        .collect())
}

/// Evaluate the most recent agent sessions that touched a publisher's
/// domains against a reporting profile (RSL profile, sections 7–8).
///
/// Isolation (profile s8.2): only events on the publisher's own domains
/// enter the evaluation, plus the sessions' turn events — turn events carry
/// no content and are what the grounding level obliges (standard s5.7.2),
/// so excluding them would fail every grounding claim while including them
/// discloses no other publisher's content usage.
///
/// Sessions are taken most-recent-first up to `session_limit`; when the
/// report's `sessions_evaluated` equals the limit it is a sample, not a
/// census, and the response says so.
#[allow(clippy::too_many_arguments, clippy::cast_precision_loss)]
pub async fn get_publisher_conformance(
    pool: &PgPool,
    profile: &crate::profile::ProfileDefinition,
    domains: &[String],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    domain_filter: Option<&str>,
    agent: Option<&str>,
    session_limit: i64,
) -> Result<crate::profile::ConformanceReport, sqlx::Error> {
    use crate::profile::{
        AgentAggregator, ConformanceReport, OPERATIONALLY_ASSESSED, ProfileEvent, evaluate_session,
    };

    let effective = effective_domains(domains, domain_filter);
    let patterns: Vec<String> = domain_like_patterns(&effective);

    if patterns.is_empty() {
        return Ok(ConformanceReport {
            profile_uri: profile.profile_uri.clone(),
            constrains: profile.constrains.clone(),
            sessions_evaluated: 0,
            session_limit,
            agents: vec![],
            operationally_assessed: OPERATIONALLY_ASSESSED,
        });
    }

    // 1. Candidate sessions: most recent agent sessions with at least one
    //    event on the publisher's domains in the window.
    let like_clauses: Vec<String> = (1..=patterns.len())
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let mut query = format!(
        "SELECT s.id, s.platform_id, s.agent_id, s.conformance_level
         FROM sessions s
         WHERE EXISTS (
             SELECT 1 FROM events e
             WHERE e.session_id = s.id AND ({})
         )",
        like_clauses.join(" OR ")
    );
    let mut param_idx = patterns.len() + 1;
    if since.is_some() {
        query.push_str(&format!(" AND s.started_at >= ${param_idx}"));
        param_idx += 1;
    }
    if until.is_some() {
        query.push_str(&format!(" AND s.started_at <= ${param_idx}"));
        param_idx += 1;
    }
    if agent.is_some() {
        query.push_str(&format!(" AND s.agent_id = ${param_idx}"));
        param_idx += 1;
    }
    query.push_str(&format!(" ORDER BY s.started_at DESC LIMIT ${param_idx}"));

    let mut q = sqlx::query_as::<_, ConformanceSessionRow>(&query);
    for pattern in &patterns {
        q = q.bind(pattern);
    }
    if let Some(ref s) = since {
        q = q.bind(s);
    }
    if let Some(ref u) = until {
        q = q.bind(u);
    }
    if let Some(a) = agent {
        q = q.bind(a);
    }
    q = q.bind(session_limit);
    let session_rows = q.fetch_all(pool).await?;

    if session_rows.is_empty() {
        return Ok(ConformanceReport {
            profile_uri: profile.profile_uri.clone(),
            constrains: profile.constrains.clone(),
            sessions_evaluated: 0,
            session_limit,
            agents: vec![],
            operationally_assessed: OPERATIONALLY_ASSESSED,
        });
    }

    // 2. Those sessions' events: the publisher's content events plus turn
    //    events (which carry no content_url and no other publisher's data).
    let session_ids: Vec<Uuid> = session_rows.iter().map(|r| r.id).collect();
    let like_clauses: Vec<String> = (2..=patterns.len() + 1)
        .map(|i| format!("e.content_url LIKE ${i}"))
        .collect();
    let events_query = format!(
        "SELECT e.session_id, e.event_type, e.license_ref, e.event_data,
                EXTRACT(EPOCH FROM (e.created_at - e.event_timestamp))::float8 AS lag_seconds
         FROM events e
         WHERE e.session_id = ANY($1)
           AND (({}) OR e.event_type IN ('turn_started', 'turn_completed'))",
        like_clauses.join(" OR ")
    );
    let mut eq = sqlx::query_as::<_, ConformanceEventRow>(&events_query).bind(&session_ids);
    for pattern in &patterns {
        eq = eq.bind(pattern);
    }
    let event_rows = eq.fetch_all(pool).await?;

    // 3. Group events per session, evaluate, and fold into per-agent
    //    aggregates.
    let mut per_session: std::collections::HashMap<Uuid, Vec<ConformanceEventRow>> =
        std::collections::HashMap::new();
    for row in event_rows {
        if let Some(sid) = row.session_id {
            per_session.entry(sid).or_default().push(row);
        }
    }

    let mut aggregator = AgentAggregator::new(profile);
    let mut sessions_evaluated = 0_u64;
    for session in &session_rows {
        let rows = per_session.remove(&session.id).unwrap_or_default();
        let events: Vec<ProfileEvent> = rows
            .iter()
            .map(|r| {
                ProfileEvent::from_stored(&r.event_type, r.license_ref.as_deref(), &r.event_data)
            })
            .collect();
        let lags: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.lag_seconds)
            .map(|l| l.max(0.0))
            .collect();
        let evaluation = evaluate_session(profile, session.conformance_level.as_deref(), &events);
        aggregator.add_session(
            session.platform_id.as_deref(),
            session.agent_id.as_deref(),
            &evaluation,
            &lags,
        );
        sessions_evaluated += 1;
    }

    Ok(ConformanceReport {
        profile_uri: profile.profile_uri.clone(),
        constrains: profile.constrains.clone(),
        sessions_evaluated,
        session_limit,
        agents: aggregator.finish(),
        operationally_assessed: OPERATIONALLY_ASSESSED,
    })
}

fn empty_summary(owner_id: Uuid, domains: &[String]) -> PublisherSummary {
    PublisherSummary {
        organization_id: owner_id,
        domains: domains.to_vec(),
        synthetic: false,
        total_events: 0,
        total_sessions: 0,
        events_by_type: vec![],
        events_by_source: vec![],
        agents: vec![],
        period_start: None,
        period_end: None,
    }
}

// Internal row types for sqlx::FromRow
#[derive(Debug, sqlx::FromRow)]
struct ReconciliationRow {
    identity: Option<String>,
    edge_retrievals: i64,
    self_reported_retrievals: i64,
    grounded: i64,
    cited: i64,
    displayed: i64,
    engaged: i64,
    sessions_reporting: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ConformanceSessionRow {
    id: Uuid,
    platform_id: Option<String>,
    agent_id: Option<String>,
    conformance_level: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ConformanceEventRow {
    session_id: Option<Uuid>,
    event_type: String,
    license_ref: Option<String>,
    event_data: serde_json::Value,
    lag_seconds: Option<f64>,
}

#[derive(Debug, sqlx::FromRow)]
struct EventTypeCountRow {
    event_type: String,
    count: i64,
    #[allow(dead_code)]
    sessions: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct PublisherEventRow {
    id: Uuid,
    session_id: Option<Uuid>,
    event_type: String,
    source_role: Option<String>,
    content_telemetry_id: Option<Uuid>,
    content_url: Option<String>,
    event_timestamp: DateTime<Utc>,
    event_data: serde_json::Value,
    platform_id: Option<String>,
    agent_id: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct AgentBreakdownRow {
    platform_id: Option<String>,
    agent_id: Option<String>,
    bot_name: Option<String>,
    bot_category: Option<String>,
    event_count: i64,
    session_count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct SourceRoleRow {
    source_role: Option<String>,
    count: i64,
    sessions: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct AgentSourceRow {
    platform_id: Option<String>,
    agent_id: Option<String>,
    bot_name: Option<String>,
    bot_category: Option<String>,
    source_role: Option<String>,
    count: i64,
    sessions: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct AgentTypeRow {
    platform_id: Option<String>,
    agent_id: Option<String>,
    bot_name: Option<String>,
    bot_category: Option<String>,
    event_type: String,
    count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct UrlMetricRow {
    content_url: String,
    total_events: i64,
    unique_sessions: i64,
    last_seen: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct UrlEventTypeRow {
    content_url: String,
    event_type: String,
    count: i64,
    #[allow(dead_code)]
    sessions: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct AgentDomainRow {
    domain: String,
    event_count: i64,
    session_count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct DomainMetricRow {
    domain: String,
    total_events: i64,
    unique_sessions: i64,
    last_seen: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct DomainEventTypeRow {
    domain: String,
    event_type: String,
    count: i64,
    #[allow(dead_code)]
    sessions: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct DomainPreviewUrlRow {
    content_url: String,
    event_count: i64,
    last_seen: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct UnclaimedDomainRow {
    domain: String,
    total_events: i64,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
}
