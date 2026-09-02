-- v1 event fields the original schema predates.
--
-- ctx_token (specification section 5.2, 7.4.1): on an agent-reported
-- content_engaged event, the click token minted for that engagement's
-- presentation, recorded so destination reports can be joined to it at
-- resolution. The token-to-presentation binding is issuer state and this
-- column is where it survives.
--
-- terms_ref (specification section 5.2.4): reference to the governing terms
-- the emitter associates with the event. A processor MUST preserve it
-- unchanged - it is stored and served byte-for-byte, never normalised.

ALTER TABLE events ADD COLUMN IF NOT EXISTS ctx_token TEXT;
ALTER TABLE events ADD COLUMN IF NOT EXISTS terms_ref TEXT;

-- Destination reports join to the agent-recorded engagement by token.
CREATE INDEX IF NOT EXISTS idx_events_ctx_token ON events(ctx_token) WHERE ctx_token IS NOT NULL;
