-- Content Telemetry v1 storage: sessions, events, ctx tokens.
--
-- Column-for-column the schema the reference services query. Where a CHECK
-- constraint is absent below, its absence is deliberate and load-bearing --
-- see the notes on conformance_level and event_type.

CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- ---------------------------------------------------------------------------
-- Sessions (specification section 5.1)
--
-- A bounded interaction between an initiator and an agent. Retrieval-level
-- emitters (origin, edge, index) do not open sessions at all -- they emit
-- standalone events correlated by content_telemetry_id.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Owning organisation, resolved by the caller from its own auth model.
    organization_id UUID NOT NULL,

    initiator_type TEXT NOT NULL DEFAULT 'user',

    -- Initiator identity, for agent-to-agent sessions.
    initiator JSONB,

    -- Opaque identifier for the content collection / permissions context.
    content_scope TEXT,

    -- URL of the publishing participant's manifest (specification section 8).
    manifest_ref TEXT,

    -- Informational conformance level: retrieval | grounding | citation
    -- (specification section 5.7). Intentionally unconstrained. The spec
    -- requires that consumers MUST NOT reject a document over this field, so
    -- the database cannot be the layer that rejects unknown values. Ingest
    -- normalises legacy values and logs non-standard ones.
    conformance_level TEXT,

    -- Hash of the content configuration at session start.
    config_snapshot_hash TEXT,

    agent_id            TEXT,
    external_session_id TEXT,

    -- Cross-session journey linking, and the immediate parent of a delegated
    -- session (specification section 5.1).
    prior_session_ids UUID[] DEFAULT '{}',
    parent_session_id UUID,

    -- Segmentation context. No PII.
    user_context JSONB NOT NULL DEFAULT '{}',

    -- Platform identification extensions.
    platform_id TEXT,
    client_type TEXT,
    client_info JSONB,

    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at   TIMESTAMPTZ,

    outcome_type  TEXT,
    outcome_value JSONB,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT sessions_initiator_type_check CHECK (
        initiator_type IN ('user', 'agent')
    ),

    CONSTRAINT sessions_outcome_type_check CHECK (
        outcome_type IS NULL OR outcome_type IN ('conversion', 'abandonment', 'browse')
    )
);

-- ---------------------------------------------------------------------------
-- Events (specification section 5.2)
--
-- Individual telemetry events, either within a session or standalone for
-- retrieval-level emitters.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- NULL for sessionless origin/edge/index events.
    session_id UUID REFERENCES sessions(id) ON DELETE CASCADE,

    -- Direct organisation owner; sessionless events have no session to inherit
    -- ownership from.
    organization_id UUID NOT NULL,

    -- Core event type. Intentionally unconstrained: the spec permits
    -- implementation-defined extension events (section 5.3), and a consumer
    -- tolerating them means the database must not reject them. Ingest applies
    -- a token sanity check (1-64 chars of letters, digits, '_', '.', '-').
    event_type TEXT NOT NULL,

    -- origin | index | edge | agent (specification section 2.4).
    source_role TEXT,

    -- Cross-observer correlation identifier, carried on the wire as the
    -- Content-Telemetry-ID header. Lets an origin, an edge and an agent each
    -- report the same retrieval independently without coordinating.
    content_telemetry_id UUID,

    content_url TEXT,

    -- Stable content identifier: CMS ID, DOI, ISBN, ISCC, C2PA hash.
    content_id TEXT,

    -- Associates the event with a conversation turn.
    turn_id TEXT,

    -- Licence reference connecting the event to an access licence.
    license_ref TEXT,

    -- Commerce extension.
    product_id UUID,

    -- v1 output-construction and presentation identity (specification 5.2):
    -- output_id joins reproduction/citation/presentation events to the output
    -- artifact they describe, output_element_id names the element within it,
    -- citation_id links a presentation or reproduction to the citation it
    -- carries, and presentation_id links an engagement to the exact
    -- presentation occurrence acted upon.
    output_id         TEXT,
    output_element_id TEXT,
    citation_id       UUID,
    presentation_id   UUID,

    -- Turn data for turn_started / turn_completed.
    turn_data JSONB,

    -- Type-specific metadata (data profiles).
    event_data JSONB NOT NULL DEFAULT '{}',

    event_timestamp TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT events_source_role_check CHECK (
        source_role IS NULL OR source_role IN ('origin', 'index', 'edge', 'agent')
    ),

    CONSTRAINT events_has_owner CHECK (
        session_id IS NOT NULL OR organization_id IS NOT NULL
    )
);

-- ---------------------------------------------------------------------------
-- ctx tokens
--
-- Map a click-out to the session that produced it, so a landing page can
-- report engagement against an interaction it did not observe. Disclosure
-- through these is consent-gated on both sides -- see 0001_identity.sql.
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS click_tokens (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token       TEXT NOT NULL UNIQUE,
    session_id  UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    content_url TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at  TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '90 days')
);

-- ---------------------------------------------------------------------------
-- Indexes
-- ---------------------------------------------------------------------------

CREATE INDEX idx_sessions_org           ON sessions(organization_id);
CREATE INDEX idx_sessions_scope         ON sessions(content_scope)       WHERE content_scope IS NOT NULL;
CREATE INDEX idx_sessions_external      ON sessions(external_session_id) WHERE external_session_id IS NOT NULL;
CREATE INDEX idx_sessions_outcome       ON sessions(outcome_type)        WHERE outcome_type IS NOT NULL;
CREATE INDEX idx_sessions_ended         ON sessions(ended_at)            WHERE ended_at IS NOT NULL;
CREATE INDEX idx_sessions_ended_started ON sessions(ended_at, started_at DESC) WHERE ended_at IS NOT NULL;
CREATE INDEX idx_sessions_platform      ON sessions(platform_id)         WHERE platform_id IS NOT NULL;
CREATE INDEX idx_sessions_prior         ON sessions USING GIN (prior_session_ids);

CREATE INDEX idx_events_session              ON events(session_id, event_timestamp);
CREATE INDEX idx_events_content              ON events(content_url) WHERE content_url IS NOT NULL;
CREATE INDEX idx_events_content_pattern      ON events(content_url text_pattern_ops) WHERE content_url IS NOT NULL;
CREATE INDEX idx_events_content_id           ON events(content_id) WHERE content_id IS NOT NULL;
CREATE INDEX idx_events_turn_id              ON events(session_id, turn_id) WHERE turn_id IS NOT NULL;
CREATE INDEX idx_events_content_telemetry_id ON events(content_telemetry_id) WHERE content_telemetry_id IS NOT NULL;
CREATE INDEX idx_events_timestamp            ON events(event_timestamp DESC);
CREATE INDEX idx_events_org_timestamp        ON events(organization_id, event_timestamp DESC);

-- Clickthrough joins engagement -> exact presentation occurrence.
CREATE INDEX idx_events_presentation_id ON events(presentation_id) WHERE presentation_id IS NOT NULL;

CREATE INDEX idx_click_tokens_session ON click_tokens(session_id);
CREATE INDEX idx_click_tokens_expires ON click_tokens(expires_at);

CREATE TRIGGER sessions_updated_at
    BEFORE UPDATE ON sessions
    FOR EACH ROW
    EXECUTE FUNCTION set_updated_at();
