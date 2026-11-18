#![cfg(feature = "postgres-tests")]

use std::{
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use by_human::{
    api::{ApiState, router},
    browser_session::{BrowserOrigins, SessionSecret, token_fingerprint},
    engine::{
        Engine, EngineError, Limits, Model, ModelTurn, RuntimeEvent, RuntimeSink, ToolDefinition,
    },
    runner::{Runner, RunnerHandle},
    store::{Run, RunStatus, Store},
};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const TOKEN: &str = "synthetic-http-contract-token-32-bytes-minimum";

struct Database {
    admin: PgPool,
    schema: String,
    store: Store,
}

impl Database {
    async fn new() -> Self {
        let url = std::env::var("DATABASE_URL")
            .expect("postgres-tests requires an isolated DATABASE_URL");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("http_contract_{}", Uuid::new_v4().simple());
        // This identifier is a fixed prefix plus generated UUID hex digits;
        // PostgreSQL identifiers cannot be represented with bind parameters.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .application_name(&schema)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        let store = Store::from_pool(pool).await.unwrap();
        Self {
            admin,
            schema,
            store,
        }
    }

    async fn cleanup(self) {
        // Only this test's generated schema is disposable. No shared tables or
        // user data are ever removed by the fixture.
        assert!(
            self.schema.starts_with("http_contract_")
                && self
                    .schema
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        );
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .unwrap();
        self.admin.close().await;
    }
}

#[derive(Default)]
struct HappyModel {
    calls: AtomicUsize,
    inputs: Mutex<Vec<Vec<Value>>>,
    first_gate: Option<Arc<Notify>>,
}

#[async_trait]
impl Model for HappyModel {
    async fn turn(
        &self,
        input: &[Value],
        _: &[ToolDefinition],
        sink: &dyn RuntimeSink,
        _: &CancellationToken,
    ) -> Result<ModelTurn, EngineError> {
        self.inputs.lock().unwrap().push(input.to_vec());
        let count = self.calls.fetch_add(1, Ordering::SeqCst);
        if count == 0
            && let Some(gate) = &self.first_gate
        {
            gate.notified().await;
        }
        if count.is_multiple_of(2) {
            Ok(ModelTurn {
                output: vec![
                    json!({"type":"function_call","call_id":format!("clock-{count}"),"name":"get_current_time","arguments":"{}"}),
                ],
                usage: None,
            })
        } else {
            sink.emit(RuntimeEvent::TextDelta {
                delta: "こんにちは".into(),
            })
            .await?;
            Ok(ModelTurn {
                output: vec![
                    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"こんにちは"}]}),
                ],
                usage: None,
            })
        }
    }
}

#[derive(Default)]
struct DelayedModel {
    calls: AtomicUsize,
    entered: Notify,
    release: Notify,
}

#[async_trait]
impl Model for DelayedModel {
    async fn turn(
        &self,
        _: &[Value],
        _: &[ToolDefinition],
        _: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> Result<ModelTurn, EngineError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        tokio::select! {
            _ = cancel.cancelled() => Err(EngineError::cancelled()),
            _ = self.release.notified() => Ok(ModelTurn {
                output: vec![json!({"type":"function_call","call_id":"must-not-dispatch","name":"get_current_time","arguments":"{}"})],
                usage: None,
            }),
        }
    }
}

#[derive(Default)]
struct UnresponsiveModel {
    entered: Notify,
}

#[async_trait]
impl Model for UnresponsiveModel {
    async fn turn(
        &self,
        _: &[Value],
        _: &[ToolDefinition],
        _: &dyn RuntimeSink,
        _: &CancellationToken,
    ) -> Result<ModelTurn, EngineError> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

struct OversizedModel;

#[async_trait]
impl Model for OversizedModel {
    async fn turn(
        &self,
        _: &[Value],
        _: &[ToolDefinition],
        _: &dyn RuntimeSink,
        _: &CancellationToken,
    ) -> Result<ModelTurn, EngineError> {
        Ok(ModelTurn {
            output: vec![
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"a".repeat(1_048_577)}]}),
            ],
            usage: None,
        })
    }
}

struct TestApp {
    base: String,
    client: Client,
    stop_http: CancellationToken,
    http: JoinHandle<()>,
    runner: RunnerHandle,
}

impl TestApp {
    async fn start(store: Store, model: Arc<dyn Model>) -> Self {
        Self::start_with(store, model, true).await
    }

    async fn start_with(store: Store, model: Arc<dyn Model>, browser_sessions: bool) -> Self {
        let runner = Runner::start(
            store.clone(),
            Arc::new(Engine::new(model, Limits::default())),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let state = ApiState::new(store, TOKEN.into(), runner.notifier()).unwrap();
        let app = router(if browser_sessions {
            state.with_browser_origins(BrowserOrigins::loopback(addr))
        } else {
            state
        });
        let stop_http = CancellationToken::new();
        let done = stop_http.clone();
        let http = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(done.cancelled_owned())
                .await
                .unwrap();
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        Self {
            base,
            client,
            stop_http,
            http,
            runner,
        }
    }

    async fn conversation(&self, title: &str) -> Uuid {
        let result: Value = self
            .client
            .post(format!("{}/v1/conversations", self.base))
            .bearer_auth(TOKEN)
            .json(&json!({"title":title}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        result["id"].as_str().unwrap().parse().unwrap()
    }

    async fn send(&self, conversation: Uuid, message: &str, key: &str) -> reqwest::Response {
        self.client
            .post(format!(
                "{}/v1/conversations/{conversation}/runs",
                self.base
            ))
            .bearer_auth(TOKEN)
            .json(&json!({"message":message,"idempotency_key":key}))
            .send()
            .await
            .unwrap()
    }

    async fn shutdown(self) {
        self.runner.shutdown().await.unwrap();
        self.stop_http.cancel();
        self.http.await.unwrap();
    }
}

async fn terminal(store: &Store, id: Uuid) -> Run {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = store.get_run(id).await.unwrap();
            if run.status.is_terminal() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run did not settle")
}

fn sse_ids(body: &str) -> Vec<i64> {
    body.lines()
        .filter_map(|line| {
            line.strip_prefix("id: ")
                .or_else(|| line.strip_prefix("id:"))
        })
        .map(|value| value.trim().parse().unwrap())
        .collect()
}

#[tokio::test]
async fn authenticated_send_retry_and_durable_sse_are_one_execution() {
    let db = Database::new().await;
    let gate = Arc::new(Notify::new());
    let model = Arc::new(HappyModel {
        first_gate: Some(gate.clone()),
        ..Default::default()
    });
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    let health = app
        .client
        .get(format!("{}/health", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(health.headers()["cache-control"], "no-store");
    let denied = app
        .client
        .post(format!("{}/v1/conversations", app.base))
        .json(&json!({"title":"unauthorized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    assert!(db.store.list_conversations(100).await.unwrap().is_empty());
    let wrong = app
        .client
        .get(format!("{}/v1/conversations", app.base))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let unknown = app
        .client
        .get(format!("{}/v1/unknown", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    let conversation = app.conversation("日本語の会話").await;
    let initial = app.send(conversation, "時刻を調べて", "request-one").await;
    assert_eq!(initial.status(), StatusCode::ACCEPTED);
    let initial: Value = initial.json().await.unwrap();
    let id: Uuid = initial["id"].as_str().unwrap().parse().unwrap();
    let retry = app.send(conversation, "時刻を調べて", "request-one").await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry: Value = retry.json().await.unwrap();
    assert_eq!(retry["id"], initial["id"]);
    let mismatch = app.send(conversation, "違う内容", "request-one").await;
    assert_eq!(mismatch.status(), StatusCode::CONFLICT);
    let busy = app.send(conversation, "next input", "another-key").await;
    assert_eq!(busy.status(), StatusCode::CONFLICT);

    // Disconnect while the model is still waiting. This must not cancel the
    // durable run; later text and tool events remain available for replay.
    let response = app
        .client
        .get(format!("{}/v1/runs/{id}/events", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let mut bytes = response.bytes_stream();
    let first = bytes.next().await.unwrap().unwrap();
    assert!(!first.is_empty());
    drop(bytes);
    gate.notify_one();
    let finished = terminal(&db.store, id).await;
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.output_text, "こんにちは");
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(db.store.list_tools(id).await.unwrap().len(), 1);
    assert!(
        model.inputs.lock().unwrap()[1]
            .iter()
            .any(|item| item["type"] == "function_call_output")
    );

    // Resume with the initial durable event's per-run sequence.
    let replay = app
        .client
        .get(format!("{}/v1/runs/{id}/events", app.base))
        .bearer_auth(TOKEN)
        .header("Last-Event-ID", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let body = replay.text().await.unwrap();
    let ids = sse_ids(&body);
    assert_eq!(ids, (2..=finished.event_seq).collect::<Vec<_>>());
    assert!(body.contains("event: run.finished"));
    assert!(body.contains("event: text_delta"));
    assert_eq!(body.matches("event: tool.started").count(), 1);
    assert_eq!(body.matches("event: tool.completed").count(), 1);
    let text_events: Vec<_> = body
        .split("\n\n")
        .filter(|event| event.contains("event: text_delta"))
        .collect();
    assert!(
        text_events
            .iter()
            .all(|event| !event.contains("arguments") && !event.contains("function_call"))
    );
    let future = app
        .client
        .get(format!("{}/v1/runs/{id}/events", app.base))
        .bearer_auth(TOKEN)
        .header("Last-Event-ID", (finished.event_seq + 1).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(future.status(), StatusCode::BAD_REQUEST);
    let history: Value = app
        .client
        .get(format!("{}/v1/conversations/{conversation}", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);
    assert_eq!(history["messages"][1]["text"], "こんにちは");
    assert!(history["runs"][0].get("output_items").is_none());
    assert!(history["runs"][0].get("owner_id").is_none());
    let missing = app
        .client
        .get(format!("{}/v1/runs/{}", app.base, Uuid::new_v4()))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let oversized = app
        .send(conversation, &"a".repeat(33_000), "oversized")
        .await;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn stop_during_provider_wait_prevents_tool_dispatch_and_success() {
    let db = Database::new().await;
    let model = Arc::new(DelayedModel::default());
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    let conversation = app.conversation("stop").await;
    let run: Value = app
        .send(conversation, "stop before tool", "stop-key")
        .await
        .json()
        .await
        .unwrap();
    let id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.entered.notified())
        .await
        .unwrap();
    // The sole worker is occupied, so this second run is stopped before claim.
    let queued_conversation = app.conversation("queued stop").await;
    let queued: Value = app
        .send(queued_conversation, "never dispatch", "queued-stop")
        .await
        .json()
        .await
        .unwrap();
    let queued_id = queued["id"].as_str().unwrap();
    let stopped_queued = app
        .client
        .post(format!("{}/v1/runs/{queued_id}/cancel", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(stopped_queued.status(), StatusCode::OK);
    let stopped_queued: Value = stopped_queued.json().await.unwrap();
    assert_eq!(stopped_queued["run"]["status"], "cancelled");
    let stopped = app
        .client
        .post(format!("{}/v1/runs/{id}/cancel", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(stopped.status(), StatusCode::OK);
    model.release.notify_one();
    let finished = terminal(&db.store, id).await;
    assert_eq!(finished.status, RunStatus::Cancelled);
    assert!(db.store.list_tools(id).await.unwrap().is_empty());
    assert!(finished.output_text.is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    let retry: Value = app
        .send(conversation, "stop before tool", "stop-key")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(retry["status"], "cancelled");
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn startup_recovers_unknown_without_replay_and_resumes_queued_work() {
    let db = Database::new().await;
    let uncertain_conversation = db.store.create_conversation("uncertain").await.unwrap();
    let interrupted = db
        .store
        .enqueue(uncertain_conversation.id, "crashed", "do not replay")
        .await
        .unwrap()
        .run;
    let guard = db.store.acquire_owner().await.unwrap();
    db.store
        .claim_next(guard.owner_id())
        .await
        .unwrap()
        .unwrap();
    guard.release().await.unwrap();
    let queued_conversation = db.store.create_conversation("queued").await.unwrap();
    let queued = db
        .store
        .enqueue(queued_conversation.id, "queued", "resume queued")
        .await
        .unwrap()
        .run;
    let model = Arc::new(HappyModel::default());
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    let duplicate_owner = Runner::start(
        db.store.clone(),
        Arc::new(Engine::new(
            Arc::new(HappyModel::default()),
            Limits::default(),
        )),
        CancellationToken::new(),
    )
    .await;
    assert!(duplicate_owner.is_err());
    let finished = terminal(&db.store, queued.id).await;
    assert_eq!(finished.status, RunStatus::Completed);
    let interrupted = db.store.get_run(interrupted.id).await.unwrap();
    assert_eq!(interrupted.status, RunStatus::Uncertain);
    assert!(
        db.store
            .enqueue(
                uncertain_conversation.id,
                "another",
                "must review previous outcome"
            )
            .await
            .is_err()
    );
    assert!(
        !model
            .inputs
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|item| item.to_string().contains("do not replay"))
    );
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn explicit_authenticated_review_retains_receipts_and_allows_new_work() {
    let db = Database::new().await;
    let conversation = db
        .store
        .create_conversation("review through API")
        .await
        .unwrap();
    let original = db
        .store
        .enqueue(
            conversation.id,
            "original-key",
            "historical external operation",
        )
        .await
        .unwrap()
        .run;
    let owner = db.store.acquire_owner().await.unwrap();
    db.store
        .claim_next(owner.owner_id())
        .await
        .unwrap()
        .unwrap();
    db.store
        .before_tool(
            original.id,
            owner.owner_id(),
            "read",
            "lookup",
            json!({"synthetic_private_argument":"retained"}),
        )
        .await
        .unwrap();
    db.store
        .after_tool(
            original.id,
            owner.owner_id(),
            "read",
            "synthetic_private_receipt",
        )
        .await
        .unwrap();
    db.store
        .before_tool(
            original.id,
            owner.owner_id(),
            "write",
            "external_operation",
            json!({"effect":"unconfirmed"}),
        )
        .await
        .unwrap();
    owner.release().await.unwrap();
    let model = Arc::new(HappyModel::default());
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    // The credential-free shell is public; all conversation/review data still
    // requires the server bearer. No separate application server is needed.
    let shell = app
        .client
        .get(format!("{}/", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(shell.status(), StatusCode::OK);
    assert_eq!(shell.headers()["content-type"], "text/html; charset=utf-8");
    assert_eq!(shell.headers()["cache-control"], "no-store");
    assert_eq!(shell.headers()["x-content-type-options"], "nosniff");
    let csp = shell.headers()["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("script-src 'self'"));
    assert!(csp.contains("connect-src 'self'"));
    assert!(!csp.contains("unsafe-inline"));
    assert!(!shell.text().await.unwrap().contains(TOKEN));
    let script = app
        .client
        .get(format!("{}/chat.js", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    assert_eq!(
        script.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(script.headers()["cache-control"], "no-store");
    assert_eq!(script.headers()["x-content-type-options"], "nosniff");
    let uncertain = db.store.get_run(original.id).await.unwrap();
    assert_eq!(uncertain.status, RunStatus::Uncertain);
    let review_url = format!("{}/v1/runs/{}/review", app.base, original.id);
    let resolve_url = format!("{}/v1/runs/{}/resolve", app.base, original.id);
    assert_eq!(
        app.client.get(&review_url).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.client.post(&resolve_url).json(&json!({"note":"review", "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":true})).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    let review: Value = app
        .client
        .get(&review_url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(review["run"]["status"], "uncertain");
    assert_eq!(review["run"]["idempotency_key"], "original-key");
    assert_eq!(review["tools"].as_array().unwrap().len(), 2);
    assert_eq!(review["tools"][0]["output"], "synthetic_private_receipt");
    assert_eq!(review["tools"][1]["status"], "uncertain");
    let history_url = format!("{}/v1/conversations/{}", app.base, conversation.id);
    let history = app
        .client
        .get(&history_url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!history.contains("synthetic_private_argument"));
    assert!(!history.contains("synthetic_private_receipt"));
    assert_eq!(
        app.send(conversation.id, "new work", "next-key")
            .await
            .status(),
        StatusCode::CONFLICT
    );
    for body in [
        json!({"note":"review", "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":false}),
        json!({"note":" ", "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":true}),
        json!({"note":"x".repeat(4097), "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":true}),
        json!({"note":"review", "expected_event_seq":-1,"acknowledge_unknown":true}),
    ] {
        assert_eq!(
            app.client
                .post(&resolve_url)
                .bearer_auth(TOKEN)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for body in [
        json!({"note":"review", "expected_event_seq":uncertain.event_seq}),
        json!({"note":"review", "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":true,"extra":true}),
    ] {
        assert_eq!(
            app.client
                .post(&resolve_url)
                .bearer_auth(TOKEN)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let stale = json!({"note":"review", "expected_event_seq":uncertain.event_seq-1,"acknowledge_unknown":true});
    assert_eq!(
        app.client
            .post(&resolve_url)
            .bearer_auth(TOKEN)
            .json(&stale)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let note = "外部の結果は確認できていません。前の操作は繰り返さず、新しい相談を続けます。";
    let body =
        json!({"note":note, "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":true});
    let reviewed: Value = app
        .client
        .post(&resolve_url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reviewed["run"]["status"], "uncertain");
    assert!(reviewed["run"]["reviewed_at"].is_string());
    assert_eq!(reviewed["run"]["review_note"], note);
    let replay: Value = app
        .client
        .post(&resolve_url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay, reviewed);
    let different = json!({"note":"different note", "expected_event_seq":uncertain.event_seq,"acknowledge_unknown":true});
    assert_eq!(
        app.client
            .post(&resolve_url)
            .bearer_auth(TOKEN)
            .json(&different)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let duplicate: Value = app
        .send(
            conversation.id,
            "historical external operation",
            "original-key",
        )
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate["id"], original.id.to_string());
    assert_eq!(duplicate["status"], "uncertain");
    let next: Value = app
        .send(conversation.id, "new work", "next-key")
        .await
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let next_id = next["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        terminal(&db.store, next_id).await.status,
        RunStatus::Completed
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    let input = serde_json::to_string(&model.inputs.lock().unwrap()[0]).unwrap();
    assert!(input.contains(note));
    assert!(input.contains("do not repeat its external operations"));
    assert!(!input.contains("synthetic_private_receipt"));
    assert!(!input.contains("synthetic_private_argument"));
    assert_eq!(
        serde_json::to_value(db.store.list_tools(original.id).await.unwrap()).unwrap(),
        review["tools"]
    );
    let event_body = app
        .client
        .get(format!("{}/v1/runs/{}/events", app.base, original.id))
        .bearer_auth(TOKEN)
        .header("Last-Event-ID", uncertain.event_seq.to_string())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(event_body.contains("event: run.reviewed"));
    assert!(!event_body.contains(note));
    assert!(!event_body.contains("synthetic_private_receipt"));
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn shutdown_records_uncertainty_before_releasing_the_owner() {
    let db = Database::new().await;
    let model = Arc::new(UnresponsiveModel::default());
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    let conversation = app.conversation("shutdown").await;
    let run: Value = app
        .send(conversation, "unsettled provider", "shutdown-key")
        .await
        .json()
        .await
        .unwrap();
    let id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.entered.notified())
        .await
        .unwrap();
    app.shutdown().await;
    let run = db.store.get_run(id).await.unwrap();
    assert_eq!(run.status, RunStatus::Uncertain);
    assert_eq!(run.error_code.as_deref(), Some("PERSISTENCE_UNCERTAIN"));
    assert_eq!(
        db.store
            .list_events(id, 0, 100)
            .await
            .unwrap()
            .last()
            .unwrap()
            .kind,
        "run.finished"
    );
    // Shutdown released the lock synchronously; a replacement can own it now.
    let guard = db.store.acquire_owner().await.unwrap();
    guard.release().await.unwrap();
    db.cleanup().await;
}

#[tokio::test]
async fn oversized_final_text_fails_without_retiring_the_worker() {
    let db = Database::new().await;
    let app = TestApp::start(db.store.clone(), Arc::new(OversizedModel)).await;
    let conversation = app.conversation("display limits").await;
    for key in ["first-limit", "next-limit"] {
        let response = app.send(conversation, "large output", key).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let run: Value = response.json().await.unwrap();
        let id = run["id"].as_str().unwrap().parse().unwrap();
        let run = terminal(&db.store, id).await;
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.error_code.as_deref(), Some("RUN_LIMIT_REACHED"));
        assert!(run.output_text.is_empty());
    }
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn lost_owner_stops_the_runner_and_requires_recovery() {
    let db = Database::new().await;
    let model = Arc::new(DelayedModel::default());
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    let conversation = app.conversation("owner loss").await;
    let run: Value = app
        .send(conversation, "wait before tool", "lost-owner")
        .await
        .json()
        .await
        .unwrap();
    let id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.entered.notified())
        .await
        .unwrap();
    // Terminate only this generated fixture's dedicated lock-owning session.
    // Ordinary pool sessions and other tests are excluded by both predicates.
    let terminated: Vec<bool> = sqlx::query_scalar("SELECT pg_terminate_backend(a.pid) FROM pg_stat_activity a WHERE a.application_name = $1 AND EXISTS (SELECT 1 FROM pg_locks l WHERE l.pid = a.pid AND l.locktype = 'advisory')")
        .bind(&db.schema).fetch_all(&db.admin).await.unwrap();
    assert_eq!(terminated, vec![true]);
    tokio::time::timeout(Duration::from_secs(3), app.runner.fatal_token().cancelled())
        .await
        .expect("ownership loss did not stop the runner");
    assert!(app.runner.shutdown().await.is_err());
    app.stop_http.cancel();
    app.http.await.unwrap();
    assert_eq!(
        db.store.get_run(id).await.unwrap().status,
        RunStatus::Running
    );
    assert!(db.store.list_tools(id).await.unwrap().is_empty());
    let mut replacement = db.store.acquire_owner().await.unwrap();
    assert_eq!(
        db.store
            .recover_interrupted(&mut replacement)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        db.store.get_run(id).await.unwrap().status,
        RunStatus::Uncertain
    );
    replacement.release().await.unwrap();
    db.cleanup().await;
}

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
    assert_eq!(
        login(None, TOKEN).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        login(Some("http://evil.example"), TOKEN)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let wrong = login(Some(&app.base), "wrong-token-with-at-least-32-bytes-0000")
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.headers().get("set-cookie").is_none());

    let signed_in = login(Some(&app.base), TOKEN).await.unwrap();
    assert_eq!(signed_in.status(), StatusCode::OK);
    let set_cookie = signed_in.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.starts_with("by_human_session="));
    assert!(set_cookie.ends_with("; Path=/; HttpOnly; SameSite=Strict; Max-Age=34560000"));
    assert!(!set_cookie.contains(TOKEN));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    let body: Value = signed_in.json().await.unwrap();
    assert!(body["expires_at"].is_string());

    let get = |path: &str, cookie: &str| {
        app.client
            .get(format!("{}/v1{path}", app.base))
            .header("cookie", cookie)
            .send()
    };
    let status: Value = get("/session", &cookie)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["method"], "browser");
    assert_eq!(
        get("/conversations", &cookie).await.unwrap().status(),
        StatusCode::OK
    );

    let create = |origin: Option<&str>| {
        let mut request = app
            .client
            .post(format!("{}/v1/conversations", app.base))
            .header("cookie", &cookie)
            .json(&json!({}));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.send()
    };
    assert_eq!(create(None).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(
        create(Some("http://evil.example")).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        create(Some(&app.base)).await.unwrap().status(),
        StatusCode::CREATED
    );

    let bearer: Value = app
        .client
        .get(format!("{}/v1/session", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bearer["method"], "bearer");
    let invalid_bearer = app
        .client
        .get(format!("{}/v1/conversations", app.base))
        .header("cookie", &cookie)
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_bearer.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        get("/conversations", &format!("{cookie}; {cookie}"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get("/conversations", "by_human_session=short")
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // A session issued under another server token or past its idle expiry is rejected.
    for (token_hash, idle) in [
        (
            token_fingerprint("another-server-token-with-32-bytes-000"),
            3_600,
        ),
        (token_fingerprint(TOKEN), -60),
    ] {
        let secret = SessionSecret::generate().unwrap();
        db.store
            .create_browser_session(&secret.hash(), &token_hash, idle)
            .await
            .unwrap();
        let stale = format!("by_human_session={}", secret.encode());
        assert_eq!(
            get("/conversations", &stale).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    let ended = app
        .client
        .delete(format!("{}/v1/session", app.base))
        .header("cookie", &cookie)
        .header("origin", &app.base)
        .send()
        .await
        .unwrap();
    assert_eq!(ended.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        ended.headers()["set-cookie"],
        "by_human_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
    );
    assert_eq!(
        get("/conversations", &cookie).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn browser_sessions_are_unavailable_without_an_allowed_origin() {
    let db = Database::new().await;
    let app = TestApp::start_with(db.store.clone(), Arc::new(HappyModel::default()), false).await;
    let signed_in = app
        .client
        .post(format!("{}/v1/sessions", app.base))
        .header("origin", &app.base)
        .json(&json!({"token": TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(signed_in.status(), StatusCode::FORBIDDEN);
    // A cookie issued while sessions were available no longer authenticates.
    let secret = SessionSecret::generate().unwrap();
    db.store
        .create_browser_session(&secret.hash(), &token_fingerprint(TOKEN), 3_600)
        .await
        .unwrap();
    let read = app
        .client
        .get(format!("{}/v1/conversations", app.base))
        .header("cookie", format!("by_human_session={}", secret.encode()))
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::UNAUTHORIZED);
    let bearer = app
        .client
        .get(format!("{}/v1/conversations", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(bearer.status(), StatusCode::OK);
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn conversations_can_be_renamed_and_deleted_over_http() {
    let db = Database::new().await;
    let app = TestApp::start(db.store.clone(), Arc::new(HappyModel::default())).await;
    let url = |id: Uuid| format!("{}/v1/conversations/{id}", app.base);
    let rename = |id: Uuid, body: Value| {
        app.client
            .patch(url(id))
            .bearer_auth(TOKEN)
            .json(&body)
            .send()
    };
    let delete = |id: Uuid| app.client.delete(url(id)).bearer_auth(TOKEN).send();

    let id = app.conversation("元の名前").await;
    let renamed = rename(id, json!({"title": " 新しい名前 "})).await.unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(renamed.headers()["cache-control"], "no-store");
    let body: Value = renamed.json().await.unwrap();
    assert_eq!(body["id"], id.to_string());
    assert_eq!(body["title"], "新しい名前");
    assert!(body["last_activity_at"].is_string());
    assert!(body.get("title_pending").is_none());
    for (payload, status) in [
        (json!({"title": "   "}), StatusCode::BAD_REQUEST),
        (json!({"title": "a\u{0}b"}), StatusCode::BAD_REQUEST),
        (json!({"title": "名".repeat(201)}), StatusCode::BAD_REQUEST),
        (
            json!({"title": "x", "extra": 1}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (json!({}), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        assert_eq!(rename(id, payload).await.unwrap().status(), status);
    }
    let missing = rename(Uuid::new_v4(), json!({"title": "x"})).await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        missing.json::<Value>().await.unwrap(),
        json!({"error": "not_found"})
    );

    let other = app.conversation("残す").await;
    let sent: Value = app
        .send(id, "削除前のメッセージ", "before-delete")
        .await
        .json()
        .await
        .unwrap();
    let run: Uuid = sent["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(terminal(&db.store, run).await.status, RunStatus::Completed);
    let listed: Value = app
        .client
        .get(format!("{}/v1/conversations", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // The new message moved the renamed conversation above the newer one.
    assert_eq!(listed["conversations"][0]["id"], id.to_string());
    assert_eq!(listed["conversations"][0]["title"], "新しい名前");
    assert_eq!(listed["conversations"][1]["id"], other.to_string());

    let deleted = delete(id).await.unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(deleted.bytes().await.unwrap().is_empty());
    assert_eq!(delete(id).await.unwrap().status(), StatusCode::NOT_FOUND);
    let gone = app
        .client
        .get(url(id))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        app.send(id, "削除後", "after-delete").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.client
            .get(format!("{}/v1/runs/{run}", app.base))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    // The runner keeps serving other conversations.
    let next: Value = app
        .send(other, "別の会話", "after-delete-other")
        .await
        .json()
        .await
        .unwrap();
    let next: Uuid = next["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(terminal(&db.store, next).await.status, RunStatus::Completed);

    // Browser sessions need an allowed Origin for both changes.
    let signed_in = app
        .client
        .post(format!("{}/v1/sessions", app.base))
        .header("origin", &app.base)
        .json(&json!({"token": TOKEN}))
        .send()
        .await
        .unwrap();
    let cookie = signed_in.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let browser = |request: reqwest::RequestBuilder, origin: Option<&str>| {
        let request = request.header("cookie", &cookie);
        match origin {
            Some(origin) => request.header("origin", origin),
            None => request,
        }
        .send()
    };
    for origin in [None, Some("http://evil.example")] {
        assert_eq!(
            browser(
                app.client.patch(url(other)).json(&json!({"title": "x"})),
                origin
            )
            .await
            .unwrap()
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            browser(app.client.delete(url(other)), origin)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        browser(
            app.client
                .patch(url(other))
                .json(&json!({"title": "ブラウザ"})),
            Some(&app.base)
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        browser(app.client.delete(url(other)), Some(&app.base))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    app.shutdown().await;
    db.cleanup().await;
}

#[tokio::test]
async fn a_conversation_with_active_work_cannot_be_deleted_over_http() {
    let db = Database::new().await;
    let model = Arc::new(DelayedModel::default());
    let app = TestApp::start(db.store.clone(), model.clone()).await;
    let conversation = app.conversation("実行中").await;
    let url = format!("{}/v1/conversations/{conversation}", app.base);
    let run: Value = app
        .send(conversation, "待つ", "busy")
        .await
        .json()
        .await
        .unwrap();
    let id: Uuid = run["id"].as_str().unwrap().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(3), model.entered.notified())
        .await
        .unwrap();
    let refused = app
        .client
        .delete(&url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(
        refused.json::<Value>().await.unwrap(),
        json!({"error": "conflict"})
    );
    app.client
        .post(format!("{}/v1/runs/{id}/cancel", app.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    model.release.notify_one();
    assert_eq!(terminal(&db.store, id).await.status, RunStatus::Cancelled);
    assert_eq!(
        app.client
            .delete(&url)
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    app.shutdown().await;
    db.cleanup().await;
}
