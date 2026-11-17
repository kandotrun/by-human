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
    engine::{Engine, EngineError, Limits, Model, ModelTurn, RuntimeEvent, RuntimeSink},
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
        let runner = Runner::start(
            store.clone(),
            Arc::new(Engine::new(model, Limits::default())),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(ApiState::new(store, TOKEN.into(), runner.notifier()).unwrap());
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
