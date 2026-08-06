-- Development fixtures.
--
-- This server deliberately has no endpoint that creates an organisation.
-- Provisioning is an operator concern that sits alongside whatever auth you
-- put behind `auth.rs`, so in a real deployment these rows come from your own
-- account system. For local development, insert them by hand:
--
--   psql "$DATABASE_URL" -f examples/dev-seed.sql
--
-- The UUIDs are fixed so the curl examples in the README work as written.
-- They are also, therefore, public knowledge. Never load this file into a
-- deployment that is reachable by anyone but you.

-- Agent: owns session lifecycle, issues ctx tokens, and has opted into
-- sharing its sessions through them.
INSERT INTO organizations (id, name, slug, org_type, share_sessions_via_click_tokens)
VALUES (
    '11111111-1111-1111-1111-111111111111',
    'Demo Agent',
    'demo-agent',
    'agent',
    true
)
ON CONFLICT (id) DO NOTHING;

-- Content owner: owns example.com and has opted into being named in ctx
-- token lookups.
INSERT INTO organizations (id, name, slug, org_type, visible_in_click_token_lookups)
VALUES (
    '22222222-2222-2222-2222-222222222222',
    'Demo Publisher',
    'demo-publisher',
    'content_owner',
    true
)
ON CONFLICT (id) DO NOTHING;

-- Verified by fiat, which only ever happens here. A real deployment sets
-- verified_at only after the organisation has proven control of the
-- hostname; until then the domain is invisible to every read path.
INSERT INTO domains (organization_id, domain, verified_at)
VALUES (
    '22222222-2222-2222-2222-222222222222',
    'example.com',
    NOW()
)
ON CONFLICT (organization_id, domain) DO NOTHING;
