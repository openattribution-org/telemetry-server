//! Content Telemetry Core
//!
//! Types, validation and PostgreSQL storage services for the Content
//! Telemetry standard. This is the reference implementation of the storage
//! and read model the specification describes; `content-telemetry-server`
//! wraps it in HTTP, and other implementations are free to use it directly.
//!
//! This crate has no opinion on auth. Callers provide an `owner_id: Uuid`
//! (resolved from their own auth model) and this crate handles storage and
//! queries against PostgreSQL.
//!
//! The schema it expects ships in `migrations/`, applied with `sqlx::migrate!`
//! or the `sqlx` CLI. It covers the telemetry tables in full plus the narrow
//! slice of organisation and domain state the consent and ownership checks
//! read; see `migrations/0001_identity.sql` for why those two tables are here
//! and everything else about identity is not.

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::format_push_string)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::option_if_let_else)]

pub mod conformance;
pub mod domain;
pub mod models;
pub mod profile;
pub mod services;
pub mod standard;
