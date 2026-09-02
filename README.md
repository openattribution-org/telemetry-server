# Content Telemetry reference server

A working implementation of the [Content Telemetry](https://github.com/SPUR-Coalition/telemetry)
standard: one Rust binary over one PostgreSQL database. It accepts telemetry
from agents, origins and edge collectors, enforces the specification's
structural rules at ingest, and serves sessions back as standard session
documents.

It exists so that the specification has an executable answer. Where prose and
code disagree, the specification wins and this is a bug.

Two crates:

| Crate | What it is |
|---|---|
| `content-telemetry-core` | Types, specification validation, and PostgreSQL storage and query services. Depend on this directly if you are building your own server. |
| `content-telemetry-server` | The HTTP surface over that library. |

## Running it

You need Rust 1.88 or later and a PostgreSQL 14+ database. The server applies
its own migrations on boot.

```bash
createdb content_telemetry
export DATABASE_URL=postgres://localhost/content_telemetry
cargo run -p content-telemetry-server
```

It listens on `0.0.0.0:8080` unless `BIND_ADDR` says otherwise.

There is no endpoint that creates an organisation — provisioning belongs with
whatever account system you put behind the auth seam below. For local work,
`examples/dev-seed.sql` inserts a demo agent, a demo publisher and a verified
domain with fixed UUIDs:

```bash
psql "$DATABASE_URL" -f examples/dev-seed.sql
```

Then open a session and report against it:

```bash
ORG=11111111-1111-1111-1111-111111111111

SESSION=$(curl -s -X POST localhost:8080/sessions/start \
  -H 'content-type: application/json' -H "x-organization-id: $ORG" \
  -d '{"initiator_type":"user","agent_id":"demo-agent","conformance_level":"citation"}' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["session_id"])')

curl -s -X POST localhost:8080/events \
  -H 'content-type: application/json' -H "x-organization-id: $ORG" \
  -d '{
    "document_type": "event_batch",
    "schema_version": "1.0",
    "session_id": "'"$SESSION"'",
    "events": [
      {"type": "content_grounded",
       "timestamp": "2026-08-06T12:00:00Z",
       "content_url": "https://example.com/article-1",
       "data": {"scope": "session"}}
    ]}'

curl -s "localhost:8080/sessions/$SESSION/document" -H "x-organization-id: $ORG"
```

`scripts/smoke.sh` runs that whole path — ingest, validation, ctx-token
disclosure and document materialisation — against a live build.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health`, `/ready` | Liveness, and readiness including the database |
| `POST` | `/sessions/start` | Open a session |
| `POST` | `/sessions/end` | Close it with an outcome |
| `POST` | `/sessions/bulk` | Ingest a complete session document in one request |
| `GET` | `/sessions/{id}/document` | The session as a standard session document |
| `POST` | `/events` | Ingest events, singly or in batches of up to 500 |
| `POST` | `/click-tokens` | Mint a ctx token for a click-out |
| `GET` | `/ctx/{token}` | Resolve a ctx token to its click context |

Events bind to a session in this order: the event's own `session_id`, the
batch's, the event's `ctx_token`, the batch's, and otherwise nothing —
retrieval-level emitters report standalone events correlated by
`content_telemetry_id`. Events for a session the server has not seen
reconstruct it under a UUIDv5 derived from the organisation and the presented
id, so two emitters cannot collide on a chosen id or reach each other's
sessions.

## Authentication

Every request acts as an organisation, and the reference build reads that
organisation's UUID straight from the `X-Organization-Id` header. An
unmodified server therefore trusts its caller completely, which is right
behind a gateway that authenticates first and fine against the dev fixtures,
and wrong anywhere else.

`crates/content-telemetry-server/src/auth.rs` is the one place that decides.
Replace the body of `from_request_parts` with a real credential check — API
key, bearer token, session cookie, mTLS — and resolve it to an organisation.
No handler changes, because no handler knows how the answer was reached.

The exception is `GET /ctx/{token}`, deliberately unauthenticated: the
destination of a click-out has no account here. The token is the credential,
and what it discloses is bounded by two-sided consent and by a click-context
shape that never contains the session id.

## The database

`crates/content-telemetry-core/migrations/` holds the schema. The telemetry
tables — `sessions`, `events`, `click_tokens` — are the specification's data
model in full.

Alongside them sit two identity tables, cut down to what the query paths
actually consult: `organizations` for the three roles and the two ctx-token
consent flags, and `domains` for which organisation has proven control of a
hostname. Both are matters of consent and ownership rather than
authentication, which is why they are here while users, credentials and the
mechanics of proving control are not. `verified_at` gates every content-owner
read: an organisation sees telemetry for domains it has verified and no
others.

## Conformance

Ingest enforces the v1 structural rules on documents declaring
`schema_version` `"1.0"`: `content_cited` and `content_presented` carry an
event id and an `output_id`; `content_grounded` carries `data.scope`;
`content_cited` carries `data.citation_type`; `content_retrieved` carries
`source_role`; `content_engaged` carries the `presentation_id` of the
presentation it acted on; content events carry a resolvable `content_url` or
`content_id`. Documents still declaring `"0.1"` are accepted for the
transition and normalised under the specification's migration rules instead.
The `content_displayed` type v1 withdrew is refused rather than rewritten
into a claim the emitter never made. `content_reproduced`, which never made
it out of the pre-release draft, is no longer a core type: rows stored under
it are treated like any other extension event and quarantined under the
document's `extensions` member.

Two things are stored as given rather than validated, because the
specification says a consumer must not reject a document over either:
`conformance_level`, which is informational, and unrecognised event types,
which are how the extension mechanism works. Extension types get a token
sanity check and nothing more. Where the specification requires normalisation
instead of rejection — enum synonyms, fields withdrawn on privacy grounds,
turn content above the declared privacy level — the server normalises and logs
what it changed.

Materialising a session document keeps that honest in the other direction.
Events that predate v1 or belong to an extension move under the document's
`extensions` member rather than being dropped or rewritten, so the document
stays valid against the v1 schema and still carries everything the emitter
reported.

## Development

```bash
cargo test                        # unit tests, plus integration tests needing a database
cargo clippy --workspace --all-targets
cargo fmt --check
```

The integration tests use `sqlx::test`, which creates an ephemeral database
per test, so `DATABASE_URL` must point at a server where the connecting user
can `CREATE DATABASE`.

## Licence

Apache 2.0. See [LICENSE](./LICENSE).
