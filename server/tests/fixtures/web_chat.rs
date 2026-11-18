//! Synthetic browser contract host. Never used by the application binary.
//! It owns one generated PostgreSQL schema and cleans only that schema.
use std::{str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use by_human::{
    api::{self, ApiState},
    engine::{
        Engine, EngineError, EngineErrorKind, Limits, Model, ModelTurn, RuntimeEvent, RuntimeSink,
        Tool, ToolDefinition, ToolRegistry,
    },
    runner::Runner,
    store::{RunCompletion, RunStatus, Store},
};
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const TOKEN: &str = "by-human-web-contract-test-token-2026";

async fn seed_large_history(store: &Store) -> anyhow::Result<()> {
    let conversation = store.create_conversation("大きな履歴").await?;
    let owner = store.acquire_owner().await?;
    let line = "partial reply\n";
    let output_text = line.repeat(1_048_576 / line.len());
    for i in 1..=9 {
        let run = store
            .enqueue(conversation.id, &format!("large-{i}"), &format!("依頼 {i}"))
            .await?;
        store.claim_next(owner.owner_id()).await?;
        store
            .finish(
                run.run.id,
                owner.owner_id(),
                RunCompletion {
                    status: RunStatus::Failed,
                    output_items: vec![],
                    output_text: output_text.clone(),
                    error_code: Some("SYNTHETIC_FAILURE".to_owned()),
                },
            )
            .await?;
    }
    owner.release().await?;
    Ok(())
}

struct BrowserModel;

#[async_trait]
impl Model for BrowserModel {
    async fn turn(
        &self,
        input: &[Value],
        _: &[ToolDefinition],
        sink: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> Result<ModelTurn, EngineError> {
        let message = input
            .iter()
            .rev()
            .find(|item| item["role"] == "user")
            .and_then(|item| item["content"][0]["text"].as_str())
            .unwrap_or("メッセージ");
        if message == "復旧テスト" {
            return Ok(ModelTurn {
                output: vec![json!({
                    "type":"function_call", "call_id":format!("fixture-{}",Uuid::new_v4()),
                    "name":"fixture_unknown_action", "arguments":"{\"message\":\"synthetic operation only\"}"
                })],
                usage: None,
            });
        }
        if message == "停止テスト" {
            sink.emit(RuntimeEvent::TextDelta {
                delta: "停止操作を待っています。".into(),
            })
            .await?;
            cancel.cancelled().await;
            return Err(EngineError::cancelled());
        }
        let first = "こんにちは。";
        let last = format!("「{message}」を受け取りました。");
        sink.emit(RuntimeEvent::TextDelta {
            delta: first.into(),
        })
        .await?;
        // Synthetic provider latency gives the browser a real partial state.
        tokio::select! {
            _ = cancel.cancelled() => return Err(EngineError::cancelled()),
            _ = tokio::time::sleep(Duration::from_millis(150)) => {}
        }
        sink.emit(RuntimeEvent::TextDelta {
            delta: last.clone(),
        })
        .await?;
        Ok(ModelTurn {
            output: vec![json!({
                "type":"message", "role":"assistant", "content":[{
                    "type":"output_text", "text":format!("{first}{last}")
                }]
            })],
            usage: None,
        })
    }
}

struct UnknownAction;

#[async_trait]
impl Tool for UnknownAction {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fixture_unknown_action".into(),
            description: "Synthetic test operation; no external effect.".into(),
            parameters: json!({"type":"object","properties":{"message":{"type":"string"}},"required":["message"],"additionalProperties":false}),
            strict: true,
        }
    }

    fn validate_arguments(&self, arguments: &Value) -> Result<(), EngineError> {
        if arguments["message"].as_str() == Some("synthetic operation only") {
            Ok(())
        } else {
            Err(EngineError::new(
                EngineErrorKind::Tool,
                "invalid synthetic arguments",
            ))
        }
    }

    async fn execute(&self, _: &Value, cancel: &CancellationToken) -> Result<String, EngineError> {
        // No real side effect. The actual engine intent exists, but no
        // definitive receipt is fabricated when the browser asks to Stop.
        cancel.cancelled().await;
        Err(EngineError::cancelled())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("browser fixture requires an isolated DATABASE_URL"))?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:43219").await?;
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .map_err(|_| anyhow::anyhow!("browser fixture database unavailable"))?;
    let schema = format!("web_contract_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await?;
    let options = PgConnectOptions::from_str(&url)
        .map_err(|_| anyhow::anyhow!("invalid browser fixture database URL"))?
        .application_name(&schema)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;
    let store = Store::from_pool(pool.clone()).await?;
    seed_large_history(&store).await?;
    let shutdown = CancellationToken::new();
    let mut tools = ToolRegistry::default();
    tools.register(Arc::new(UnknownAction))?;
    let runner = Runner::start(
        store.clone(),
        Arc::new(Engine::with_tools(
            Arc::new(BrowserModel),
            Limits::default(),
            Arc::new(tools),
        )),
        shutdown.clone(),
    )
    .await?;
    let state = ApiState::new(store, TOKEN.into(), runner.notifier())
        .map_err(|_| anyhow::anyhow!("invalid fixture token"))?
        .with_browser_origins(by_human::browser_session::BrowserOrigins::loopback(
            listener.local_addr()?,
        ));
    let http_shutdown = shutdown.clone();
    let mut http = tokio::spawn(async move {
        axum::serve(listener, api::router(state))
            .with_graceful_shutdown(http_shutdown.cancelled_owned())
            .await
    });
    println!("by-human browser fixture ready at http://127.0.0.1:43219");
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    shutdown.cancel();
    runner.shutdown().await?;
    if tokio::time::timeout(Duration::from_secs(3), &mut http)
        .await
        .is_err()
    {
        http.abort();
        let _ = http.await;
    }
    pool.close().await;
    // The identifier is generated locally, never supplied by a request.
    assert!(
        schema.starts_with("web_contract_")
            && schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    );
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await?;
    admin.close().await;
    Ok(())
}
