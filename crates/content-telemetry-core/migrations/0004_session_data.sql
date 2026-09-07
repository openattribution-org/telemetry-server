-- The session-level data container, and the top-level members the
-- specification does not define.
--
-- session_data (specification section 5.1.3): the session document's `data`
-- object, an extension container mirroring the event-level `data` field.
-- Core defines one member inside it, `access_context`, which records the
-- institution whose access rights the session used. Consumers MUST tolerate
-- unknown fields within the container and unknown identifier schemes within
-- access_context, so the column holds what the emitter sent, unnormalised,
-- and materialisation serves it back unchanged.
--
-- unrecognised_fields (specification sections 5.1.3, 5.7.4): top-level
-- siblings of `events` that this server does not define. The session root is
-- not an extension point, so nothing here is interpreted, but a conforming
-- consumer MUST tolerate unknown fields without error - so they are recorded
-- instead of being dropped, and materialisation returns them under the
-- document's `extensions` member.

ALTER TABLE sessions ADD COLUMN IF NOT EXISTS session_data JSONB;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS unrecognised_fields JSONB;
