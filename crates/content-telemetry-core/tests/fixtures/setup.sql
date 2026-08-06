-- Per-test seed data. Schema comes from this crate's migrations/, so this
-- file is data-only.
--
-- Three organisations covering both sides of the ctx-token consent gate: an
-- agent that shares, a publisher that is visible and owns a verified domain,
-- and a publisher that has opted into neither.

-- Agent org (consents to click token sharing)
INSERT INTO organizations (id, name, slug, share_sessions_via_click_tokens)
VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', 'Test Agent', 'test-agent', true);

-- Publisher org (consents to click token visibility, has verified domain)
INSERT INTO organizations (id, name, slug, visible_in_click_token_lookups)
VALUES ('bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', 'Test Publisher', 'test-publisher', true);

INSERT INTO domains (id, organization_id, domain, verified_at)
VALUES (
    gen_random_uuid(),
    'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb',
    'example.com',
    NOW()
);

-- Non-consenting publisher
INSERT INTO organizations (id, name, slug)
VALUES ('cccccccc-cccc-cccc-cccc-cccccccccccc', 'Private Publisher', 'private-pub');
