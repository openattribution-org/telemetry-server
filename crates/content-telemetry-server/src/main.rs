//! Reference HTTP server for the Content Telemetry standard.
//!
//! One binary, one PostgreSQL database, no bundled UI. It implements the
//! ingest and disclosure surface the specification describes and nothing
//! beyond it: no accounts, no dashboards, no scoring. Auth is a seam you fill
//! in — see `auth.rs`.

mod auth;
mod error;
mod routes;
mod validate;

use std::net::SocketAddr;
use std::time::Duration;

use content_telemetry_core::services::click_tokens;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::signal;

/// Expired ctx tokens are swept on this interval. Expiry is a retention
/// promise, not just a lookup rule: a token that has passed its expiry must
/// stop being a way to reach the session behind it.
const TOKEN_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// How far in the past an emitter may date an event. Bounds how much
    /// history a late or replayed batch can rewrite.
    pub max_event_age_days: i64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL is required (postgres://user:pass@host/db)")?;

    let bind: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()?;

    let max_event_age_days = std::env::var("MAX_EVENT_AGE_DAYS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(7);

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&database_url)
        .await?;

    sqlx::migrate!("../content-telemetry-core/migrations")
        .run(&pool)
        .await?;

    tokio::spawn(sweep_expired_tokens(pool.clone()));

    let state = AppState {
        pool,
        max_event_age_days,
    };

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, max_event_age_days, "content telemetry reference server listening");

    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn sweep_expired_tokens(pool: PgPool) {
    let mut ticker = tokio::time::interval(TOKEN_CLEANUP_INTERVAL);

    loop {
        ticker.tick().await;

        match click_tokens::cleanup_expired(&pool).await {
            Ok(0) => {}
            Ok(deleted) => tracing::info!(deleted, "swept expired ctx tokens"),
            Err(e) => tracing::error!(error = %e, "ctx token sweep failed"),
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }

    tracing::info!("shutdown signal received, draining connections");
}
