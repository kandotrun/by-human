//! Small authenticated HTTP surface. Events are read from PostgreSQL, not a
//! lossy in-memory broadcast channel.
use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{
        HeaderMap, Method, StatusCode,
        header::{self, AUTHORIZATION},
    },
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    browser_session::{self, BrowserOrigins, CookieSecret, SessionSecret},
    store::{GENERATED_TITLE_CHARS, Run, Store, StoreError},
};

const MAX_MESSAGE_BYTES: usize = 24 * 1024;
const MAX_TITLE_BYTES: usize = 800;
const MAX_KEY_BYTES: usize = 128;
const CHAT_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const CHAT_SNAPSHOT_RUNS: usize = 100;

#[derive(Clone)]
pub struct ApiState {
    store: Store,
    bearer: Arc<str>,
    token_fingerprint: [u8; 32],
    browser_origins: Arc<BrowserOrigins>,
    notifier: Arc<Notify>,
}

impl ApiState {
    pub fn new(store: Store, bearer: String, notifier: Arc<Notify>) -> Result<Self, &'static str> {
        if bearer.len() < 32 || !bearer.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(
                "the server bearer token must contain at least 32 non-whitespace ASCII bytes",
            );
        }
        Ok(Self {
            store,
            token_fingerprint: browser_session::token_fingerprint(&bearer),
            bearer: bearer.into(),
            browser_origins: Arc::default(),
            notifier,
        })
    }

    /// Lets pages served at these exact origins sign in with a browser session.
    pub fn with_browser_origins(mut self, origins: BrowserOrigins) -> Self {
        self.browser_origins = Arc::new(origins);
        self
    }
}

pub fn router(state: ApiState) -> Router {
    let authenticated = Router::new()
        .route("/session", get(session_status).delete(end_session))
        .route(
            "/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route("/conversations/{id}", get(get_conversation))
        .route("/conversations/{id}/snapshot", get(chat_snapshot))
        .route("/conversations/{id}/runs", post(enqueue))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/review", get(review_run))
        .route("/runs/{id}/resolve", post(resolve_run))
        .route("/runs/{id}/events", get(events))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), authenticate));
    // Signing in is the only `/v1` route outside the authentication layer.
    let v1 = Router::new()
        .route("/sessions", post(start_session))
        .merge(authenticated);
    Router::new()
        .merge(crate::web::router())
        .route("/health", get(health))
        .nest("/v1", v1)
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::map_response(no_store))
        .with_state(state)
}

async fn not_found() -> ApiError {
    StoreError::NotFound.into()
}

async fn health(State(state): State<ApiState>) -> impl IntoResponse {
    if matches!(
        tokio::time::timeout(Duration::from_secs(2), state.store.health()).await,
        Ok(Ok(()))
    ) {
        (StatusCode::OK, Json(json!({"status": "ready"})))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "unavailable"})),
        )
    }
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

#[derive(Clone)]
enum Authenticated {
    Bearer,
    Browser {
        secret_hash: [u8; 32],
        expires_at: DateTime<Utc>,
    },
}

async fn authenticate(
    State(state): State<ApiState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<Response, ApiError> {
    let context = if request.headers().contains_key(AUTHORIZATION) {
        // An explicit bearer request never falls back to a browser session.
        let mut values = request.headers().get_all(AUTHORIZATION).iter();
        let token = values
            .next()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let valid = values.next().is_none()
            && token
                .is_some_and(|value| bool::from(value.as_bytes().ct_eq(state.bearer.as_bytes())));
        if !valid {
            return Err(ApiError::unauthorized());
        }
        Authenticated::Bearer
    } else {
        let CookieSecret::Present(secret) = browser_session::read_cookie(request.headers()) else {
            return Err(ApiError::unauthorized());
        };
        // SameSite=Strict blocks cross-site use. The origin check also blocks
        // other origins on the same site from changing anything.
        let reads_only = matches!(*request.method(), Method::GET | Method::HEAD);
        if !reads_only && !state.browser_origins.allows(request.headers()) {
            return Err(ApiError::origin_not_allowed());
        }
        let secret_hash = secret.hash();
        let session = state
            .store
            .authenticate_browser_session(
                &secret_hash,
                &state.token_fingerprint,
                browser_session::IDLE_LIFETIME_SECS,
            )
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        Authenticated::Browser {
            secret_hash,
            expires_at: session.expires_at,
        }
    };
    request.extensions_mut().insert(context);
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
struct SessionRequest {
    token: String,
}

async fn start_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<SessionRequest>,
) -> Result<Response, ApiError> {
    if !state.browser_origins.allows(&headers) {
        return Err(ApiError::origin_not_allowed());
    }
    // Comparing fixed-size fingerprints keeps the timing independent of the
    // submitted and configured token lengths.
    let submitted = browser_session::token_fingerprint(&input.token);
    if !bool::from(submitted[..].ct_eq(&state.token_fingerprint[..])) {
        return Err(ApiError::unauthorized());
    }
    let secret = SessionSecret::generate().ok_or_else(ApiError::session_unavailable)?;
    let session = state
        .store
        .create_browser_session(
            &secret.hash(),
            &state.token_fingerprint,
            browser_session::IDLE_LIFETIME_SECS,
        )
        .await?;
    let mut response = Json(json!({"expires_at": session.expires_at})).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, browser_session::session_cookie(&secret));
    Ok(response)
}

async fn session_status(Extension(auth): Extension<Authenticated>) -> Json<Value> {
    Json(match auth {
        Authenticated::Bearer => json!({"method": "bearer", "expires_at": null}),
        Authenticated::Browser { expires_at, .. } => {
            json!({"method": "browser", "expires_at": expires_at})
        }
    })
}

async fn end_session(
    State(state): State<ApiState>,
    Extension(auth): Extension<Authenticated>,
) -> Result<Response, ApiError> {
    let Authenticated::Browser { secret_hash, .. } = auth else {
        return Err(ApiError::bad_request("browser_session_required"));
    };
    state.store.revoke_browser_session(&secret_hash).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, browser_session::expired_cookie());
    Ok(response)
}

#[derive(Deserialize)]
struct NewConversation {
    title: Option<String>,
}

async fn create_conversation(
    State(state): State<ApiState>,
    Json(input): Json<NewConversation>,
) -> Result<impl IntoResponse, ApiError> {
    let conversation = match input.title {
        Some(title) => {
            nonempty(&title, MAX_TITLE_BYTES)?;
            state.store.create_conversation(&title).await?
        }
        None => state.store.create_untitled_conversation().await?,
    };
    Ok((StatusCode::CREATED, Json(conversation)))
}

async fn list_conversations(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"conversations": state.store.list_conversations(100).await?}),
    ))
}

async fn get_conversation(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let messages = state.store.list_messages(id, 100).await?;
    let runs = state.store.list_runs(id, 100).await?;
    let conversation = state.store.get_conversation(id).await?;
    let runs: Vec<_> = runs.iter().map(public_run).collect();
    Ok(Json(
        json!({"conversation": conversation, "messages": messages, "runs": runs}),
    ))
}

async fn chat_snapshot(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let conversation = state.store.get_conversation(id).await?;
    let title_pending = conversation.title_pending;
    let mut snapshot =
        json!({"conversation": conversation, "runs": [], "older_runs_omitted": false});
    let mut bytes = serde_json::to_vec(&snapshot)
        .map_err(|_| StoreError::Storage)?
        .len();
    if title_pending {
        // A first message can commit while the run window is loading. Leave
        // room for its title, whose normalized characters need at most four
        // bytes each in JSON, including escaped quotes and backslashes.
        bytes += 4 * GENERATED_TITLE_CHARS;
    }
    let mut runs = Vec::new();
    {
        let mut rows = std::pin::pin!(state.store.recent_runs(id, CHAT_SNAPSHOT_RUNS as i64 + 1));
        while let Some(run) = rows.try_next().await? {
            let run = public_run(&run);
            // Count the escaped JSON sent on the wire, including separators and
            // the envelope. Keep a contiguous newest window and the latest run.
            bytes += serde_json::to_vec(&run)
                .map_err(|_| StoreError::Storage)?
                .len()
                + usize::from(!runs.is_empty());
            if bytes > CHAT_SNAPSHOT_BYTES || runs.len() == CHAT_SNAPSHOT_RUNS {
                snapshot["older_runs_omitted"] = json!(true);
                break;
            }
            runs.push(run);
        }
    }
    if title_pending && !runs.is_empty() {
        // A visible run was accepted with its title in the same transaction.
        // Re-read after releasing the run stream's pooled connection.
        snapshot["conversation"] = json!(state.store.get_conversation(id).await?);
    }
    snapshot["runs"] = Value::Array(runs);
    Ok(Json(snapshot))
}

#[derive(Deserialize)]
struct NewRun {
    message: String,
    idempotency_key: String,
}

async fn enqueue(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(input): Json<NewRun>,
) -> Result<impl IntoResponse, ApiError> {
    nonempty(&input.message, MAX_MESSAGE_BYTES)?;
    nonempty(&input.idempotency_key, MAX_KEY_BYTES)?;
    if !input
        .idempotency_key
        .bytes()
        .all(|byte| byte.is_ascii_graphic())
    {
        return Err(ApiError::bad_request("invalid_idempotency_key"));
    }
    let queued = state
        .store
        .enqueue(id, &input.idempotency_key, &input.message)
        .await?;
    let status = if queued.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    state.notifier.notify_one();
    let mut result = public_run(&queued.run);
    result["conversation_title"] = json!(queued.conversation_title);
    Ok((status, Json(result)))
}

async fn get_run(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({"run": public_run(&state.store.get_run(id).await?)}),
    ))
}

async fn cancel_run(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let run = state.store.request_cancel(id).await?;
    state.notifier.notify_one();
    Ok(Json(json!({"run": public_run(&run)})))
}

async fn review_run(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let run = state.store.get_run(id).await?;
    let tools = state.store.list_tools(id).await?;
    Ok(Json(json!({"run": public_run(&run), "tools": tools})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeReview {
    note: String,
    expected_event_seq: i64,
    acknowledge_unknown: bool,
}

async fn resolve_run(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(input): Json<OutcomeReview>,
) -> Result<Json<Value>, ApiError> {
    nonempty(&input.note, 4096)?;
    if !input.acknowledge_unknown || input.expected_event_seq < 0 {
        return Err(ApiError::bad_request("invalid_outcome_review"));
    }
    let run = state
        .store
        .resolve_uncertain(id, input.expected_event_seq, &input.note)
        .await?;
    Ok(Json(json!({"run": public_run(&run)})))
}

async fn events(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    // Validate this run's durable cursor before opening SSE. An event number
    // beyond this run's committed sequence must never skip future events.
    state.store.get_run(id).await?;
    let mut cursor = match headers.get("last-event-id") {
        None => 0,
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0)
            .ok_or_else(|| ApiError::bad_request("invalid_event_cursor"))?,
    };
    if cursor > 0 {
        let at_cursor = state.store.list_events(id, cursor - 1, 1).await?;
        if at_cursor.first().is_none_or(|event| event.seq != cursor) {
            return Err(ApiError::bad_request("invalid_event_cursor"));
        }
    }
    let stream = async_stream::stream! {
        loop {
            let page = match state.store.list_events(id, cursor, 100).await {
                Ok(page) => page,
                Err(_) => {
                    yield Ok::<Event, Infallible>(Event::default().event("stream_error").data("{\"error\":\"storage_unavailable\"}"));
                    break;
                }
            };
            let count = page.len();
            for item in page {
                cursor = item.seq;
                // Serialization contains only the declared event payload, never
                // transport errors, credentials or raw provider responses.
                let event = Event::default().id(item.seq.to_string()).event(item.kind)
                    .json_data(item.payload).expect("JSON values are serializable");
                yield Ok::<Event, Infallible>(event);
            }
            let run = match state.store.get_run(id).await {
                Ok(run) => run,
                Err(_) => {
                    yield Ok::<Event, Infallible>(Event::default().event("stream_error").data("{\"error\":\"storage_unavailable\"}"));
                    break;
                }
            };
            if run.status.is_terminal() {
                // Finish commits its terminal event and status atomically. A
                // final read closes the gap with the earlier event-page query.
                if count == 100 { continue; }
                match state.store.list_events(id, cursor, 100).await {
                    Ok(tail) if !tail.is_empty() => { continue; }
                    Ok(_) => break,
                    Err(_) => {
                        yield Ok::<Event, Infallible>(Event::default().event("stream_error").data("{\"error\":\"storage_unavailable\"}"));
                        break;
                    }
                }
            }
            if count < 100 { tokio::time::sleep(Duration::from_millis(250)).await; }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}

fn nonempty(value: &str, max_bytes: usize) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(ApiError::bad_request("invalid_input"));
    }
    Ok(())
}

// The public run view deliberately excludes canonical provider items, reasoning
// and tool payloads. They remain in PostgreSQL for the execution contract.
fn public_run(run: &Run) -> Value {
    json!({
        "id":run.id,"conversation_id":run.conversation_id,"status":run.status,
        "idempotency_key":run.idempotency_key,
        "input_text":run.input_text,"output_text":run.output_text,
        "cancel_requested":run.cancel_requested,"error_code":run.error_code,
        "event_seq":run.event_seq,"created_at":run.created_at,
        "started_at":run.started_at,"finished_at":run.finished_at,
        "reviewed_at":run.reviewed_at,"review_note":run.review_note,
    })
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
        }
    }
    fn bad_request(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
        }
    }
    fn origin_not_allowed() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "origin_not_allowed",
        }
    }
    fn session_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "session_unavailable",
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "not_found",
            },
            StoreError::Conflict(_) => Self {
                status: StatusCode::CONFLICT,
                code: "conflict",
            },
            StoreError::InvalidInput(_) => Self::bad_request("invalid_input"),
            StoreError::Cancelled => Self {
                status: StatusCode::CONFLICT,
                code: "cancelled",
            },
            _ => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "storage_unavailable",
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"error": self.code}))).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert("www-authenticate", "Bearer".parse().unwrap());
        }
        response
            .headers_mut()
            .insert("cache-control", "no-store".parse().unwrap());
        response
    }
}
