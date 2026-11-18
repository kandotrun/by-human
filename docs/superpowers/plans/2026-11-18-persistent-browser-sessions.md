# Persistent Browser Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The owner enters the server token once per browser and stays signed in for 30 days of use, without the browser ever storing the token.

**Architecture:** `POST /v1/sessions` exchanges the server token for a random secret in an `HttpOnly; SameSite=Strict` cookie. PostgreSQL stores only SHA-256 hashes, bound to a fingerprint of the server token. The existing authentication middleware accepts either the bearer token (unchanged) or the cookie, and cookie-authenticated writes must come from an allowed loopback origin. The embedded client switches to same-origin credentials, resumes the session on load and gains sign-out.

**Tech Stack:** Rust (axum 0.8, sqlx 0.9/PostgreSQL 17, sha2, base64, getrandom, subtle), vanilla ES modules, Playwright.

Design: [daily-driver browser chat, track 1](../specs/2026-11-18-daily-driver-web-chat-design.md).

---

## File structure

| File | Responsibility |
| --- | --- |
| `server/src/browser_session.rs` (new) | Pure helpers: secret generation/encoding/hashing, token fingerprint, cookie parsing and `Set-Cookie` values, allowed-origin policy. |
| `server/migrations/202611180003_browser_sessions.sql` (new) | `browser_sessions` table. |
| `server/src/store.rs` | `BrowserSession` row and create/authenticate/revoke methods. |
| `server/src/api.rs` | `ApiState` origins and fingerprint, session routes, cookie-aware `authenticate`. |
| `server/src/main.rs`, `server/tests/fixtures/web_chat.rs`, `server/tests/http_runtime.rs` | Pass loopback origins of the bound listener. |
| `server/web/chat.js`, `server/web/index.html` | Same-origin credentials, resume on load, sign-in/sign-out copy. |
| `web-tests/tests/chat.spec.js` | Browser contracts. |
| `docs/web-chat.md`, `docs/http-api.md`, `README.md` | Behaviour and limits. |

Commands used throughout (with an isolated PostgreSQL 17 in `DATABASE_URL`):

```sh
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
(cd web-tests && npx playwright test)
```

## Task 1: Session helpers

**Files:** Create `server/src/browser_session.rs`; modify `server/src/lib.rs`.

- [ ] **Step 1: Write the failing unit tests** at the bottom of the new module:

```rust
#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::http::{HeaderMap, HeaderValue, header};

    use super::*;

    fn cookies(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::COOKIE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn secrets_round_trip_as_43_base64url_characters() {
        let secret = SessionSecret::generate().unwrap();
        let encoded = secret.encode();
        assert_eq!(encoded.len(), 43);
        let CookieSecret::Present(parsed) = read_cookie(&cookies(&[&format!("{COOKIE_NAME}={encoded}")])) else {
            panic!("cookie was not read");
        };
        assert_eq!(parsed.hash(), secret.hash());
        assert_ne!(secret.hash(), token_fingerprint(&encoded));
    }

    #[test]
    fn exactly_one_well_formed_session_cookie_is_accepted() {
        let value = "A".repeat(43);
        assert!(matches!(read_cookie(&HeaderMap::new()), CookieSecret::Missing));
        assert!(matches!(read_cookie(&cookies(&["theme=dark"])), CookieSecret::Missing));
        assert!(matches!(read_cookie(&cookies(&[&format!("by_human_session_x={value}")])), CookieSecret::Missing));
        assert!(matches!(read_cookie(&cookies(&[&format!("theme=dark; {COOKIE_NAME}={value}")])), CookieSecret::Present(_)));
        assert!(matches!(read_cookie(&cookies(&[&format!("{COOKIE_NAME}={value}; {COOKIE_NAME}={value}")])), CookieSecret::Invalid));
        assert!(matches!(read_cookie(&cookies(&[&format!("{COOKIE_NAME}={value}"), &format!("{COOKIE_NAME}={value}")])), CookieSecret::Invalid));
        assert!(matches!(read_cookie(&cookies(&[&format!("{COOKIE_NAME}=short")])), CookieSecret::Invalid));
        assert!(matches!(read_cookie(&cookies(&[&format!("{COOKIE_NAME}={}=", "A".repeat(42))])), CookieSecret::Invalid));
    }

    #[test]
    fn cookies_are_http_only_strict_and_long_lived_handles() {
        let secret = SessionSecret::generate().unwrap();
        let value = session_cookie(&secret);
        assert_eq!(
            value.to_str().unwrap(),
            format!("{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=34560000", secret.encode())
        );
        assert_eq!(
            expired_cookie().to_str().unwrap(),
            format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
        );
    }

    #[test]
    fn only_exact_loopback_origins_of_the_listener_are_allowed() {
        let origins = BrowserOrigins::loopback("127.0.0.1:4310".parse::<SocketAddr>().unwrap());
        let origin = |values: &[&str]| {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(header::ORIGIN, HeaderValue::from_str(value).unwrap());
            }
            origins.allows(&headers)
        };
        assert!(origin(&["http://127.0.0.1:4310"]));
        assert!(origin(&["http://localhost:4310"]));
        assert!(!origin(&[]));
        assert!(!origin(&["null"]));
        assert!(!origin(&["http://127.0.0.1:4311"]));
        assert!(!origin(&["http://127.0.0.1:4310/"]));
        assert!(!origin(&["https://127.0.0.1:4310"]));
        assert!(!origin(&["http://evil.example"]));
        assert!(!origin(&["http://127.0.0.1:4310", "http://127.0.0.1:4310"]));
        let v6 = BrowserOrigins::loopback("[::1]:4310".parse().unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://[::1]:4310"));
        assert!(v6.allows(&headers));
        let public = BrowserOrigins::loopback("192.0.2.1:4310".parse().unwrap());
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://192.0.2.1:4310"));
        assert!(!public.allows(&headers));
    }
}
```

- [ ] **Step 2: Run** `cargo test --locked -p by-human --lib browser_session` and confirm it fails to compile (module missing).

- [ ] **Step 3: Implement the module** above the tests and register it with `pub mod browser_session;` in `server/src/lib.rs`:

```rust
//! Browser sign-in sessions. The browser keeps a random secret in an
//! `HttpOnly` cookie and PostgreSQL keeps only its SHA-256 hash, so the server
//! token itself is never stored by a browser.
use std::net::SocketAddr;

use axum::http::{HeaderMap, HeaderValue, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub const COOKIE_NAME: &str = "by_human_session";
/// A session ends after 30 days without use.
pub const IDLE_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;
/// The cookie is only a handle; the server decides when it expires. 400 days
/// is the longest lifetime browsers keep.
const COOKIE_MAX_AGE_SECS: i64 = 400 * 24 * 60 * 60;

pub struct SessionSecret([u8; 32]);

impl SessionSecret {
    pub fn generate() -> Option<Self> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).ok()?;
        Some(Self(bytes))
    }

    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    fn decode(value: &str) -> Option<Self> {
        if value.len() != 43 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0).into()
    }
}

/// Identifies the server token a session was issued under. Rotating the token
/// therefore ends every browser session.
pub fn token_fingerprint(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub enum CookieSecret {
    Missing,
    Invalid,
    Present(SessionSecret),
}

/// Accepts exactly one well-formed session cookie across all `Cookie` headers.
pub fn read_cookie(headers: &HeaderMap) -> CookieSecret {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else {
            return CookieSecret::Invalid;
        };
        for pair in value.split(';') {
            let Some((name, secret)) = pair.trim().split_once('=') else {
                continue;
            };
            if name != COOKIE_NAME {
                continue;
            }
            if found.replace(secret).is_some() {
                return CookieSecret::Invalid;
            }
        }
    }
    match found {
        None => CookieSecret::Missing,
        Some(value) => SessionSecret::decode(value).map_or(CookieSecret::Invalid, CookieSecret::Present),
    }
}

pub fn session_cookie(secret: &SessionSecret) -> HeaderValue {
    cookie(&secret.encode(), COOKIE_MAX_AGE_SECS)
}

pub fn expired_cookie() -> HeaderValue {
    cookie("", 0)
}

fn cookie(value: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"
    ))
    .expect("session cookies contain only base64url header text")
}

/// Exact page origins that may create and use browser sessions.
#[derive(Clone, Default)]
pub struct BrowserOrigins(Vec<String>);

impl BrowserOrigins {
    /// Pages served directly by a loopback listener. Other listeners get none.
    pub fn loopback(addr: SocketAddr) -> Self {
        if !addr.ip().is_loopback() {
            return Self::default();
        }
        let port = addr.port();
        let direct = match addr {
            SocketAddr::V4(v4) => format!("http://{}:{port}", v4.ip()),
            SocketAddr::V6(v6) => format!("http://[{}]:{port}", v6.ip()),
        };
        Self(vec![direct, format!("http://localhost:{port}")])
    }

    /// True only for a single `Origin` header that exactly matches.
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(header::ORIGIN).iter();
        let (Some(origin), None) = (values.next(), values.next()) else {
            return false;
        };
        origin
            .to_str()
            .is_ok_and(|origin| self.0.iter().any(|allowed| allowed == origin))
    }
}
```

- [ ] **Step 4: Run** `cargo test --locked -p by-human --lib browser_session`. Expected: 4 passed.

- [ ] **Step 5: Commit** `feat: add browser session cookie helpers`.

## Task 2: Session storage

**Files:** Create `server/migrations/202611180003_browser_sessions.sql`; modify `server/src/store.rs`; test in `server/tests/postgres_store.rs`.

- [ ] **Step 1: Write the failing store contract** in `server/tests/postgres_store.rs`:

```rust
#[tokio::test]
async fn browser_sessions_expire_revoke_and_follow_the_server_token() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let token = [7; 32];
    let secret = [1; 32];
    let created = store.create_browser_session(&secret, &token, 3_600).await.unwrap();
    let found = store.authenticate_browser_session(&secret, &token, 3_600).await.unwrap().unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.expires_at, created.expires_at);
    assert!(store.authenticate_browser_session(&secret, &[8; 32], 3_600).await.unwrap().is_none());
    assert!(store.authenticate_browser_session(&[2; 32], &token, 3_600).await.unwrap().is_none());

    // Use refreshes an idle expiry at most once per hour.
    sqlx::query("UPDATE browser_sessions SET last_used_at = last_used_at - interval '2 hours'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let refreshed = store.authenticate_browser_session(&secret, &token, 7_200).await.unwrap().unwrap();
    assert!(refreshed.expires_at > created.expires_at);
    let unchanged = store.authenticate_browser_session(&secret, &token, 86_400).await.unwrap().unwrap();
    assert_eq!(unchanged.expires_at, refreshed.expires_at);

    store.revoke_browser_session(&secret).await.unwrap();
    assert!(store.authenticate_browser_session(&secret, &token, 3_600).await.unwrap().is_none());

    let expired = [3; 32];
    store.create_browser_session(&expired, &token, -60).await.unwrap();
    assert!(store.authenticate_browser_session(&expired, &token, 3_600).await.unwrap().is_none());
    fixture.cleanup().await;
}
```

- [ ] **Step 2: Run** `cargo test --locked -p by-human --features postgres-tests --test postgres_store browser_sessions` and confirm it fails to compile (methods missing).

- [ ] **Step 3: Add the migration:**

```sql
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
```

- [ ] **Step 4: Add the row type and methods to `Store`** in `server/src/store.rs`:

```rust
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct BrowserSession {
    pub id: Uuid,
    pub expires_at: DateTime<Utc>,
}
```

```rust
    /// Stores a new browser session and prunes rows that ended a week ago.
    pub async fn create_browser_session(
        &self,
        secret_hash: &[u8; 32],
        token_hash: &[u8; 32],
        idle_secs: i64,
    ) -> Result<BrowserSession> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM browser_sessions WHERE expires_at < clock_timestamp() - interval '7 days' OR revoked_at < clock_timestamp() - interval '7 days'",
        )
        .execute(&mut *tx)
        .await?;
        let session = sqlx::query_as(
            "INSERT INTO browser_sessions (id, secret_hash, server_token_hash, expires_at) VALUES ($1, $2, $3, clock_timestamp() + make_interval(secs => $4)) RETURNING id, expires_at",
        )
        .bind(Uuid::new_v4())
        .bind(&secret_hash[..])
        .bind(&token_hash[..])
        .bind(idle_secs as f64)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    /// Returns the live session for this secret and server token. An idle
    /// expiry is extended at most once per hour to avoid a write per request.
    pub async fn authenticate_browser_session(
        &self,
        secret_hash: &[u8; 32],
        token_hash: &[u8; 32],
        idle_secs: i64,
    ) -> Result<Option<BrowserSession>> {
        Ok(sqlx::query_as(
            "WITH live AS (
                 SELECT id, expires_at FROM browser_sessions
                 WHERE secret_hash = $1 AND server_token_hash = $2
                   AND revoked_at IS NULL AND expires_at > clock_timestamp()
             ), touched AS (
                 UPDATE browser_sessions
                 SET last_used_at = clock_timestamp(), expires_at = clock_timestamp() + make_interval(secs => $3)
                 WHERE id IN (SELECT id FROM live) AND last_used_at < clock_timestamp() - interval '1 hour'
                 RETURNING id, expires_at
             )
             SELECT live.id, COALESCE(touched.expires_at, live.expires_at) AS expires_at
             FROM live LEFT JOIN touched ON touched.id = live.id",
        )
        .bind(&secret_hash[..])
        .bind(&token_hash[..])
        .bind(idle_secs as f64)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn revoke_browser_session(&self, secret_hash: &[u8; 32]) -> Result<()> {
        sqlx::query(
            "UPDATE browser_sessions SET revoked_at = clock_timestamp() WHERE secret_hash = $1 AND revoked_at IS NULL",
        )
        .bind(&secret_hash[..])
        .execute(&self.pool)
        .await?;
        Ok(())
    }
```

- [ ] **Step 5: Run** the store contract again. Expected: PASS. Then run the whole `postgres_store` test binary.

- [ ] **Step 6: Commit** `feat: store revocable browser sessions`.

## Task 3: Session routes and cookie authentication

**Files:** Modify `server/src/api.rs`, `server/src/main.rs`, `server/tests/fixtures/web_chat.rs`, `server/tests/http_runtime.rs`.

- [ ] **Step 1: Write the failing HTTP contract** in `server/tests/http_runtime.rs`. `TestApp::start` now builds the state with `.with_browser_origins(BrowserOrigins::loopback(addr))`; `app.base` is the page origin (`http://127.0.0.1:<port>`). Import `by_human::browser_session::{BrowserOrigins, SessionSecret, token_fingerprint}`:

```rust
#[tokio::test]
async fn browser_sessions_need_the_token_and_an_allowed_origin_and_can_be_ended() {
    let db = Database::new().await;
    let app = TestApp::start(db.store.clone(), Arc::new(HappyModel::default())).await;
    let sessions = format!("{}/v1/sessions", app.base);
    let login = |origin: Option<&str>, token: &str| {
        let mut request = app.client.post(&sessions).json(&json!({"token": token}));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.send()
    };
    assert_eq!(login(None, TOKEN).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(login(Some("http://evil.example"), TOKEN).await.unwrap().status(), StatusCode::FORBIDDEN);
    let wrong = login(Some(&app.base), "wrong-token-with-at-least-32-bytes-0000").await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.headers().get("set-cookie").is_none());

    let signed_in = login(Some(&app.base), TOKEN).await.unwrap();
    assert_eq!(signed_in.status(), StatusCode::OK);
    let set_cookie = signed_in.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(set_cookie.starts_with("by_human_session="));
    assert!(set_cookie.ends_with("; Path=/; HttpOnly; SameSite=Strict; Max-Age=34560000"));
    assert!(!set_cookie.contains(TOKEN));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    let body: Value = signed_in.json().await.unwrap();
    assert!(body["expires_at"].is_string());

    let get = |path: &str, cookie: &str| app.client.get(format!("{}/v1{path}", app.base)).header("cookie", cookie).send();
    let status: Value = get("/session", &cookie).await.unwrap().json().await.unwrap();
    assert_eq!(status["method"], "browser");
    assert_eq!(get("/conversations", &cookie).await.unwrap().status(), StatusCode::OK);

    let create = |origin: Option<&str>| {
        let mut request = app.client.post(format!("{}/v1/conversations", app.base)).header("cookie", &cookie).json(&json!({}));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.send()
    };
    assert_eq!(create(None).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(create(Some("http://evil.example")).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(create(Some(&app.base)).await.unwrap().status(), StatusCode::CREATED);

    let bearer: Value = app.client.get(format!("{}/v1/session", app.base)).bearer_auth(TOKEN).send().await.unwrap().json().await.unwrap();
    assert_eq!(bearer["method"], "bearer");
    let invalid_bearer = app.client.get(format!("{}/v1/conversations", app.base)).header("cookie", &cookie).bearer_auth("not-the-token").send().await.unwrap();
    assert_eq!(invalid_bearer.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(get("/conversations", &format!("{cookie}; {cookie}")).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(get("/conversations", "by_human_session=short").await.unwrap().status(), StatusCode::UNAUTHORIZED);

    // A session issued under another server token or past its idle expiry is rejected.
    for (token_hash, idle) in [(token_fingerprint("another-server-token-with-32-bytes-000"), 3_600), (token_fingerprint(TOKEN), -60)] {
        let secret = SessionSecret::generate().unwrap();
        db.store.create_browser_session(&secret.hash(), &token_hash, idle).await.unwrap();
        let stale = format!("by_human_session={}", secret.encode());
        assert_eq!(get("/conversations", &stale).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    }

    let ended = app.client.delete(format!("{}/v1/session", app.base)).header("cookie", &cookie).header("origin", &app.base).send().await.unwrap();
    assert_eq!(ended.status(), StatusCode::NO_CONTENT);
    assert_eq!(ended.headers()["set-cookie"], "by_human_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    assert_eq!(get("/conversations", &cookie).await.unwrap().status(), StatusCode::UNAUTHORIZED);

    app.shutdown().await;
    db.cleanup().await;
}
```


- [ ] **Step 2: Run** `cargo test --locked -p by-human --features postgres-tests --test http_runtime browser_sessions` and confirm it fails (no `/v1/sessions`, no `with_browser_origins`).

- [ ] **Step 3: Extend `ApiState`:**

```rust
#[derive(Clone)]
pub struct ApiState {
    store: Store,
    bearer: Arc<str>,
    token_fingerprint: [u8; 32],
    browser_origins: Arc<BrowserOrigins>,
    notifier: Arc<Notify>,
}
```

`new` sets `token_fingerprint: browser_session::token_fingerprint(&bearer)` and `browser_origins: Arc::default()`, then:

```rust
    /// Lets pages served at these exact origins sign in with a browser session.
    pub fn with_browser_origins(mut self, origins: BrowserOrigins) -> Self {
        self.browser_origins = Arc::new(origins);
        self
    }
```

- [ ] **Step 4: Route the session endpoints.** `POST /v1/sessions` is merged beside the authenticated router so it bypasses `authenticate`; `GET`/`DELETE /v1/session` are authenticated:

```rust
    let authenticated = Router::new()
        .route("/session", get(session_status).delete(end_session))
        .route(/* existing routes unchanged */)
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));
    let v1 = Router::new()
        .route("/sessions", post(start_session))
        .merge(authenticated);
    Router::new()
        .merge(crate::web::router())
        .route("/health", get(health))
        .nest("/v1", v1)
        // existing layers unchanged
```

- [ ] **Step 5: Implement authentication and handlers:**

```rust
#[derive(Clone)]
enum Authenticated {
    Bearer,
    Browser { secret_hash: [u8; 32], expires_at: DateTime<Utc> },
}

async fn authenticate(State(state): State<ApiState>, mut request: axum::extract::Request, next: Next) -> Result<Response, ApiError> {
    let context = if request.headers().contains_key(AUTHORIZATION) {
        // An explicit bearer request never falls back to a browser session.
        let mut values = request.headers().get_all(AUTHORIZATION).iter();
        let token = values.next().and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer "));
        let valid = values.next().is_none() && token.is_some_and(|value| bool::from(value.as_bytes().ct_eq(state.bearer.as_bytes())));
        if !valid {
            return Err(ApiError::unauthorized());
        }
        Authenticated::Bearer
    } else {
        let CookieSecret::Present(secret) = browser_session::read_cookie(request.headers()) else {
            return Err(ApiError::unauthorized());
        };
        // SameSite=Strict blocks cross-site use; this also blocks other origins on the same site.
        let reads_only = matches!(*request.method(), Method::GET | Method::HEAD);
        if !reads_only && !state.browser_origins.allows(request.headers()) {
            return Err(ApiError::origin_not_allowed());
        }
        let secret_hash = secret.hash();
        let session = state
            .store
            .authenticate_browser_session(&secret_hash, &state.token_fingerprint, browser_session::IDLE_LIFETIME_SECS)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        Authenticated::Browser { secret_hash, expires_at: session.expires_at }
    };
    request.extensions_mut().insert(context);
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
struct SessionRequest {
    token: String,
}

async fn start_session(State(state): State<ApiState>, headers: HeaderMap, Json(input): Json<SessionRequest>) -> Result<Response, ApiError> {
    if !state.browser_origins.allows(&headers) {
        return Err(ApiError::origin_not_allowed());
    }
    if !bool::from(input.token.as_bytes().ct_eq(state.bearer.as_bytes())) {
        return Err(ApiError::unauthorized());
    }
    let secret = SessionSecret::generate().ok_or_else(ApiError::session_unavailable)?;
    let session = state
        .store
        .create_browser_session(&secret.hash(), &state.token_fingerprint, browser_session::IDLE_LIFETIME_SECS)
        .await?;
    let mut response = Json(json!({"expires_at": session.expires_at})).into_response();
    response.headers_mut().insert(header::SET_COOKIE, browser_session::session_cookie(&secret));
    Ok(response)
}

async fn session_status(Extension(auth): Extension<Authenticated>) -> Json<Value> {
    Json(match auth {
        Authenticated::Bearer => json!({"method": "bearer", "expires_at": null}),
        Authenticated::Browser { expires_at, .. } => json!({"method": "browser", "expires_at": expires_at}),
    })
}

async fn end_session(State(state): State<ApiState>, Extension(auth): Extension<Authenticated>) -> Result<Response, ApiError> {
    let Authenticated::Browser { secret_hash, .. } = auth else {
        return Err(ApiError::bad_request("browser_session_required"));
    };
    state.store.revoke_browser_session(&secret_hash).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::SET_COOKIE, browser_session::expired_cookie());
    Ok(response)
}
```

Add `ApiError::origin_not_allowed()` (`403`, `origin_not_allowed`) and `ApiError::session_unavailable()` (`503`, `session_unavailable`).

- [ ] **Step 6: Pass loopback origins** where state is built:
  - `server/src/main.rs`: `.with_browser_origins(BrowserOrigins::loopback(listening_at))` after `ApiState::new(...)?`.
  - `server/tests/fixtures/web_chat.rs`: use `listener.local_addr()?` the same way.
  - `server/tests/http_runtime.rs`: `TestApp::start` uses its listener address.

- [ ] **Step 7: Run** the HTTP contract (PASS), then the whole Rust suite, Clippy and fmt.

- [ ] **Step 8: Commit** `feat: sign in browsers with revocable session cookies`.

## Task 4: Client sign-in, resume and sign-out

**Files:** Modify `server/web/chat.js`, `server/web/index.html`, `web-tests/tests/chat.spec.js`.

- [ ] **Step 1: Write the failing browser contracts.** Update the helpers and the two reload tests, then add:

```js
const signInDialog = page => page.getByRole('dialog', { name: 'by-human にログイン', exact: true });

async function connect(page, navigate = true) {
  if (navigate) await page.goto('/');
  const dialog = signInDialog(page);
  await dialog.getByLabel('サーバートークン', { exact: true }).fill(TOKEN);
  await dialog.getByRole('button', { name: 'ログイン', exact: true }).click();
  await expect(dialog).toBeHidden();
}

async function expectSignedIn(page) {
  await expect(signInDialog(page)).toBeHidden();
  await expect(page.locator('#connection-state')).toHaveText('ログイン中');
}

test('sign-in survives reload and sign-out returns to the dialog', async ({ page, context }) => {
  await connect(page);
  const id = await newConversation(page);
  await page.reload();
  await expectSignedIn(page);
  await expect(conversationButton(page, id)).toHaveAttribute('aria-current', 'page');
  const session = (await context.cookies()).find(cookie => cookie.name === 'by_human_session');
  expect(session).toMatchObject({ httpOnly: true, sameSite: 'Strict', path: '/' });
  expect(session.value).not.toContain(TOKEN);
  expect(await page.evaluate(() => document.cookie)).not.toContain('by_human_session');
  await page.getByRole('button', { name: 'ログアウト', exact: true }).click();
  await expect(signInDialog(page)).toBeVisible();
  await page.reload();
  await expect(signInDialog(page)).toBeVisible();
  expect((await context.cookies()).some(cookie => cookie.name === 'by_human_session')).toBeFalsy();
});

test('an unknown session cookie asks to sign in again', async ({ page, context }) => {
  await context.addCookies([{ name: 'by_human_session', value: 'A'.repeat(43), url: 'http://127.0.0.1:43219' }]);
  await page.goto('/');
  await expect(signInDialog(page)).toBeVisible();
  await connect(page, false);
  await expectSignedIn(page);
});

test('browser sessions reject writes from another origin', async ({ page }) => {
  await connect(page);
  const denied = await page.request.post('/v1/conversations', { data: {}, headers: { Origin: 'http://evil.example' } });
  expect(denied.status()).toBe(403);
  const allowed = await page.request.post('/v1/conversations', { data: {}, headers: { Origin: 'http://127.0.0.1:43219' } });
  expect(allowed.status()).toBe(201);
});
```

In the two reload tests replace `await connect(page, false);` after `page.reload()` with `await expectSignedIn(page);`. In the rejected-token test, look the dialog up with `signInDialog(page)`.

- [ ] **Step 2: Run** `cd web-tests && npx playwright test` and confirm the new and updated tests fail (dialog still named 接続, reload asks for the token).

- [ ] **Step 3: Update `index.html` copy:** footer `未ログイン` / `ログアウト`; dialog title `by-human にログイン`, text `サーバートークンを入力してください。このブラウザでは、使うたびに30日間ログインが続きます。`, note `トークン自体はブラウザに保存しません。ログイン状態は、ページのスクリプトから読めない Cookie で保持します。`, button `ログイン`; storage dialog `サーバートークンと受信した会話履歴はブラウザに保存しません。ログイン状態はスクリプトから読めない Cookie で保持します。共有の端末では、利用後にログアウトしてください。`

- [ ] **Step 4: Update `chat.js`:**
  - Replace `let token = "";` with `let signedIn = false;` and every `token` guard with `signedIn`.
  - `api()` sends no `Authorization` header, uses `credentials: "same-origin"`, returns `null` for `204`, and on `401` calls `disconnect(SIGNED_OUT)` where `const SIGNED_OUT = "ログインの有効期限が切れたか、ログアウトしました。サーバートークンを入力してください。";`.
  - `friendlyError` maps `403` to `このアドレスからはログインできません。サーバーと同じアドレス（例: http://127.0.0.1:4310）で開いてください。`
  - The event stream fetch drops `Authorization`, uses `credentials: "same-origin"`, and handles `401` with `disconnect(SIGNED_OUT)`.
  - Add `openSignedIn(capturedConnection)` (load conversations, close the dialog, select the remembered or newest conversation), `resumeSession()` (`GET /v1/session`; `401` opens the dialog; other failures show the error in the dialog) and `signOut()` (`DELETE /v1/session`, then `disconnect()`; other failures show a notice).
  - `connect()` posts `{token}` to `/v1/sessions` with `fetch`, clears the field immediately, shows `サーバートークンが正しくありません。` for `401` and `friendlyError` otherwise, then calls `openSignedIn`.
  - The sidebar shows `ログイン中` / `未ログイン`; the footer button calls `signOut`.
  - Start-up: `restoreSession(); render(); if (secureOrigin()) resumeSession(); else $("connect-dialog").showModal();`

- [ ] **Step 5: Run** the whole browser suite. Expected: all pass, including the 3 new tests.

- [ ] **Step 6: Commit** `feat: keep the browser chat signed in`.

## Task 5: Documentation

**Files:** `docs/web-chat.md`, `docs/http-api.md`, `README.md`.

- [ ] Describe: one sign-in per browser, 30 days after last use, `HttpOnly`/`SameSite=Strict` cookie that holds only a random handle, hashes in PostgreSQL, sign-out, token rotation ends all browser sessions, cookie writes require the page's own loopback origin, and bearer clients are unchanged. Document the three routes and status codes in the HTTP API page. Keep the Japanese summary sections in sync.
- [ ] Run `git diff --check` and commit `docs: describe persistent browser sign-in`.

## Task 6: Full verification

- [ ] Run the four commands at the top on a fresh disposable PostgreSQL and save logs under `.work/`.
- [ ] Start the browser fixture flow manually with Playwright screenshots of the sign-in dialog and a reloaded signed-in page; inspect them.
