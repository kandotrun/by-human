CREATE TABLE conversations (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE runs (
    id UUID PRIMARY KEY,
    ordinal BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    conversation_id UUID NOT NULL REFERENCES conversations(id),
    idempotency_key TEXT NOT NULL CHECK (octet_length(idempotency_key) BETWEEN 1 AND 128),
    input_text TEXT NOT NULL CHECK (octet_length(input_text) BETWEEN 1 AND 65536),
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled', 'uncertain')),
    cancel_requested BOOLEAN NOT NULL DEFAULT FALSE,
    owner_id UUID,
    output_text TEXT NOT NULL DEFAULT '',
    output_items JSONB NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(output_items) = 'array'),
    error_code TEXT,
    event_seq BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    UNIQUE (conversation_id, idempotency_key)
);

-- An uncertain external operation must be reviewed before new work in this
-- conversation. Reconnecting or retrying is never permission to replay it.
CREATE UNIQUE INDEX one_unfinished_run_per_conversation ON runs (conversation_id)
    WHERE status IN ('queued', 'running', 'uncertain');
CREATE INDEX queued_runs ON runs (ordinal) WHERE status = 'queued';
CREATE INDEX conversation_runs ON runs (conversation_id, ordinal);

CREATE TABLE run_events (
    run_id UUID NOT NULL REFERENCES runs(id),
    seq BIGINT NOT NULL,
    kind TEXT NOT NULL CHECK (octet_length(kind) BETWEEN 1 AND 64),
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (run_id, seq)
);

CREATE TABLE tool_executions (
    run_id UUID NOT NULL REFERENCES runs(id),
    call_id TEXT NOT NULL CHECK (octet_length(call_id) BETWEEN 1 AND 256),
    name TEXT NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 128),
    arguments JSONB NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('started', 'completed', 'uncertain')),
    output TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    finished_at TIMESTAMPTZ,
    PRIMARY KEY (run_id, call_id)
);
