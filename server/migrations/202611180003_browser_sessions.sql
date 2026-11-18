-- Browser sign-in sessions. Only hashes are stored. The server token
-- fingerprint ends every session when the owner rotates the token.
CREATE TABLE browser_sessions (
    id UUID PRIMARY KEY,
    secret_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(secret_hash) = 32),
    server_token_hash BYTEA NOT NULL CHECK (octet_length(server_token_hash) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX browser_sessions_expiry ON browser_sessions (expires_at);
