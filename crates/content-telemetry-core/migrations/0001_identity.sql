-- Minimal identity slice.
--
-- This crate is auth-unaware: callers resolve an owner from their own auth
-- model and pass an `owner_id: Uuid` in. Two tables are still needed, because
-- the telemetry query paths consult them for *consent* and *ownership*, which
-- are semantics of the data model rather than of authentication:
--
--   organizations -- the three-role registry, and the two ctx-token consent
--                    flags. Click-token manifest lookups are opt-in on both
--                    sides and default to closed.
--   domains       -- which organisation owns a hostname. `verified_at` gates
--                    every content-owner read: an organisation only sees
--                    telemetry for domains it has proven it controls.
--
-- Everything else an operator needs -- users, credentials, sessions, the
-- mechanics of proving domain control -- is deliberately absent. Those are
-- operator-specific. Bring your own; only the columns below are load-bearing
-- for this crate.

CREATE TABLE IF NOT EXISTS organizations (
    id       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name     TEXT NOT NULL,
    slug     TEXT NOT NULL UNIQUE,

    -- Position in the value chain (specification section 2.4):
    --   content_owner -- supply side, owns content, reads telemetry
    --   platform      -- middle, observes flows, consumes ctx_tokens
    --   agent         -- demand side, owns session lifecycle, issues ctx_tokens
    -- Source roles (origin | index | edge | agent) live on events, not here.
    org_type TEXT NOT NULL DEFAULT 'content_owner',

    -- ctx-token consent. Both default false: a click-token manifest lookup
    -- discloses one party's session to another, so it requires the agent to
    -- have opted into sharing AND the content owner to have opted into being
    -- visible. Neither is inferred.
    share_sessions_via_click_tokens BOOLEAN NOT NULL DEFAULT FALSE,
    visible_in_click_token_lookups  BOOLEAN NOT NULL DEFAULT FALSE,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ,

    CONSTRAINT chk_org_type CHECK (org_type IN ('content_owner', 'platform', 'agent'))
);

CREATE TABLE IF NOT EXISTS domains (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    domain          TEXT NOT NULL,

    -- How control was proven, for the operator's own records. This crate does
    -- not read it; it only ever checks that verified_at is non-NULL.
    verification_method TEXT NOT NULL DEFAULT 'dns_txt',

    -- NULL until the organisation has proven control. Unverified domains are
    -- invisible to every read path in this crate.
    verified_at TIMESTAMPTZ,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    UNIQUE (organization_id, domain)
);

CREATE INDEX idx_domains_organization_id ON domains(organization_id);

-- Ownership lookups match on the bare hostname, so a partial index over the
-- verified set keeps the common case cheap.
CREATE INDEX idx_domains_verified ON domains(domain) WHERE verified_at IS NOT NULL;
