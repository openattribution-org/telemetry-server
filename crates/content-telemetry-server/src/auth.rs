//! The authentication seam.
//!
//! This server has no auth of its own, on purpose. Everything downstream of
//! this module works from an organisation UUID and nothing else, exactly as
//! `content-telemetry-core` does: the storage and query layer takes an
//! `owner_id` and has no opinion on how you arrived at it.
//!
//! The reference deployment reads that UUID straight from a request header,
//! which means **an unmodified reference server trusts its caller
//! completely**. That is fine behind a gateway that authenticates and then
//! sets the header, and fine for local development against the fixtures. It
//! is not fine on the open internet.
//!
//! To put a real credential model behind it, replace the body of
//! `from_request_parts` — validate an API key, a bearer token, a session
//! cookie, an mTLS identity, whatever you already run — and resolve it to an
//! organisation. No handler needs to change.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::error::ApiError;

pub const ORG_HEADER: &str = "x-organization-id";

/// The organisation a request acts as.
pub struct OrgContext(pub Uuid);

impl<S: Send + Sync> FromRequestParts<S> for OrgContext {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .headers
            .get(ORG_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Uuid::parse_str(value).ok())
            .map(Self)
            .ok_or(ApiError::MissingOrg(ORG_HEADER))
    }
}
