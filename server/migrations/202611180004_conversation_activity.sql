-- The sidebar orders conversations by their latest activity: creation or a
-- newly accepted message. Finished replies, renames and reviews do not count.
ALTER TABLE conversations ADD COLUMN last_activity_at TIMESTAMPTZ;
UPDATE conversations c SET last_activity_at = GREATEST(
    c.created_at,
    (SELECT max(r.created_at) FROM runs r WHERE r.conversation_id = c.id));
ALTER TABLE conversations
    ALTER COLUMN last_activity_at SET DEFAULT clock_timestamp(),
    ALTER COLUMN last_activity_at SET NOT NULL;
CREATE INDEX conversations_by_activity ON conversations (last_activity_at DESC, id DESC);
