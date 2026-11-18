ALTER TABLE runs
    ADD COLUMN reviewed_at TIMESTAMPTZ,
    ADD COLUMN review_note TEXT,
    ADD COLUMN reviewed_event_seq BIGINT,
    ADD CONSTRAINT valid_outcome_review CHECK (
        (reviewed_at IS NULL AND review_note IS NULL AND reviewed_event_seq IS NULL)
        OR (status = 'uncertain' AND reviewed_at IS NOT NULL
            AND review_note IS NOT NULL AND octet_length(review_note) BETWEEN 1 AND 4096
            AND reviewed_event_seq IS NOT NULL AND reviewed_event_seq >= 0)
    );

-- Review acknowledges uncertainty; it does not change the original outcome or
-- permit that operation to be replayed. Only new work may use the conversation.
DROP INDEX one_unfinished_run_per_conversation;
CREATE UNIQUE INDEX one_unfinished_run_per_conversation ON runs (conversation_id)
    WHERE status IN ('queued', 'running')
        OR (status = 'uncertain' AND reviewed_at IS NULL);
