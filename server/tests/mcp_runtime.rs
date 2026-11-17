#![cfg(all(feature = "postgres-tests", feature = "mcp-test-fixture"))]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use by_human::{
    engine::{
        Engine, EngineError, Limits, Model, ModelTurn, RuntimeEvent, RuntimeSink, ToolDefinition,
        ToolRegistry,
    },
    mcp::{McpConnections, config::McpConfig},
    runner::{Runner, RunnerHandle},
    store::{Run, RunStatus, Store},
};
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const FIXTURE: &str = env!("CARGO_BIN_EXE_by-human-mcp-fixture");

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("by-human-mcp-contract-{}", Uuid::new_v4().simple()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).unwrap();
        Self(path)
    }

    fn journal(&self) -> PathBuf {
        self.0.join("journal.jsonl")
    }

    fn config(&self, tool: &str) -> Value {
        json!({"servers":[{
            "id":"fixture", "tools":[tool], "timeout_seconds":30,
            "transport":{"type":"stdio", "command":FIXTURE,
                "args":["--journal", self.journal()], "env":[]},
        }]})
    }

    fn entries(&self) -> Vec<Value> {
        fs::read_to_string(self.journal())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn call_count(&self) -> usize {
        self.entries()
            .iter()
            .filter(|entry| entry["method"] == "tools/call")
            .count()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // This UUID directory was created by this fixture and contains no user data.
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Database {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    store: Store,
}

impl Database {
    async fn new() -> Self {
        let url = std::env::var("DATABASE_URL")
            .expect("MCP runtime contracts require an isolated DATABASE_URL");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("mcp_contract_{}", Uuid::new_v4().simple());
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
        let store = Store::from_pool(pool.clone()).await.unwrap();
        Self {
            admin,
            pool,
            schema,
            store,
        }
    }

    async fn cleanup(self) {
        assert!(
            self.schema.starts_with("mcp_contract_")
                && self
                    .schema
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        );
        self.pool.close().await;
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

struct FixtureModel {
    name: String,
    arguments: Value,
    answer: &'static str,
    calls: AtomicUsize,
    receipts: Mutex<Vec<Value>>,
}

impl FixtureModel {
    fn new(name: String, arguments: Value, answer: &'static str) -> Self {
        Self {
            name,
            arguments,
            answer,
            calls: AtomicUsize::new(0),
            receipts: Mutex::new(vec![]),
        }
    }
}

#[async_trait]
impl Model for FixtureModel {
    async fn turn(
        &self,
        input: &[Value],
        tools: &[ToolDefinition],
        sink: &dyn RuntimeSink,
        _: &CancellationToken,
    ) -> Result<ModelTurn, EngineError> {
        assert!(tools.iter().any(|tool| tool.name == self.name));
        let count = self.calls.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            return Ok(ModelTurn {
                output: vec![json!({
                    "type":"function_call", "call_id":"fixture-call-1", "name":self.name,
                    "namespace":"by_human", "arguments":self.arguments.to_string(),
                })],
                usage: None,
            });
        }
        assert_eq!(
            count, 1,
            "the tool result should need only one final model turn"
        );
        let output = input
            .iter()
            .rev()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(output["namespace"], "by_human");
        let result = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
        self.receipts.lock().unwrap().push(result);
        sink.emit(RuntimeEvent::TextDelta {
            delta: self.answer.into(),
        })
        .await?;
        Ok(ModelTurn {
            output: vec![json!({"type":"message", "role":"assistant",
            "content":[{"type":"output_text", "text":self.answer}]})],
            usage: None,
        })
    }
}

async fn connect(directory: &Directory, tool: &str) -> McpConnections {
    let config =
        McpConfig::from_json(&serde_json::to_vec(&directory.config(tool)).unwrap()).unwrap();
    McpConnections::connect(&config, &CancellationToken::new())
        .await
        .unwrap()
}

fn tool_name(connections: &McpConnections, remote_name: &str) -> String {
    let tools = connections.tools();
    assert_eq!(tools.len(), 1, "unconfigured tools must not be published");
    assert_eq!(tools[0].remote_name, remote_name);
    tools[0].name.clone()
}

async fn start(store: Store, connections: &McpConnections, model: Arc<dyn Model>) -> RunnerHandle {
    let mut registry = ToolRegistry::default();
    connections.register(&mut registry).unwrap();
    Runner::start(
        store,
        Arc::new(Engine::with_tools(
            model,
            Limits::default(),
            Arc::new(registry),
        )),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

async fn terminal(store: &Store, id: Uuid) -> Run {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let run = store.get_run(id).await.unwrap();
            if run.status.is_terminal() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("MCP run did not settle")
}

async fn observed_call(directory: &Directory) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while directory.call_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("MCP peer never received tools/call");
}

async fn children_exited(directory: &Directory) {
    let pids: Vec<_> = directory
        .entries()
        .iter()
        .filter(|entry| entry["method"] == "fixture.started")
        .map(|entry| entry["pid"].as_u64().unwrap() as libc::pid_t)
        .collect();
    assert!(!pids.is_empty());
    #[cfg(unix)]
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // PIDs come only from our just-started synthetic child journal.
            if pids.iter().all(|pid| unsafe {libc::kill(*pid, 0)} == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)) {return}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned stdio child was not terminated and reaped");
}

#[tokio::test]
async fn stdio_tool_receipt_is_durable_and_duplicate_send_does_not_dispatch_again() {
    let directory = Directory::new();
    let db = Database::new().await;
    let mut connections = connect(&directory, "echo_note").await;
    let name = tool_name(&connections, "echo_note");
    let model = Arc::new(FixtureModel::new(
        name.clone(),
        json!({"text":"日本語のMCP確認"}),
        "確認しました",
    ));
    let runner = start(db.store.clone(), &connections, model.clone()).await;
    let conversation = db.store.create_conversation("MCP保存確認").await.unwrap();
    let sent = db
        .store
        .enqueue(conversation.id, "same-input", "メモを確認して")
        .await
        .unwrap();
    runner.notifier().notify_one();
    let finished = terminal(&db.store, sent.run.id).await;
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.output_text, "確認しました");
    let tools = db.store.list_tools(sent.run.id).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].status, "completed");
    assert_eq!(tools[0].name, name);
    assert_eq!(tools[0].arguments, json!({"text":"日本語のMCP確認"}));
    let receipt: Value = serde_json::from_str(tools[0].output.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["structuredContent"]["echo"], "日本語のMCP確認");
    assert_eq!(
        receipt["structuredContent"]["inherited_credentials"],
        json!([])
    );
    assert_eq!(model.receipts.lock().unwrap().as_slice(), &[receipt]);
    let canonical_output = finished
        .output_items
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(
        canonical_output["output"],
        tools[0].output.as_deref().unwrap()
    );
    let messages = db.store.list_messages(conversation.id, 100).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].text, "確認しました");
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("structuredContent")
    );
    let retried = db
        .store
        .enqueue(conversation.id, "same-input", "メモを確認して")
        .await
        .unwrap();
    assert!(!retried.created);
    assert_eq!(retried.run.id, sent.run.id);
    assert_eq!(directory.call_count(), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    runner.shutdown().await.unwrap();
    connections.shutdown().await.unwrap();
    children_exited(&directory).await;
    let mut reopened = connect(&directory, "echo_note").await;
    assert_eq!(tool_name(&reopened, "echo_note"), name);
    assert_eq!(directory.call_count(), 1);
    reopened.shutdown().await.unwrap();
    children_exited(&directory).await;
    db.cleanup().await;
}

#[tokio::test]
async fn stop_after_mcp_dispatch_keeps_unknown_outcome_and_never_replays_it() {
    let directory = Directory::new();
    let db = Database::new().await;
    let mut connections = connect(&directory, "hang_note").await;
    let model = Arc::new(FixtureModel::new(
        tool_name(&connections, "hang_note"),
        json!({}),
        "must not finish",
    ));
    let runner = start(db.store.clone(), &connections, model.clone()).await;
    let conversation = db.store.create_conversation("MCP停止確認").await.unwrap();
    let sent = db
        .store
        .enqueue(conversation.id, "uncertain-input", "完了を待つ")
        .await
        .unwrap();
    runner.notifier().notify_one();
    observed_call(&directory).await;
    db.store.request_cancel(sent.run.id).await.unwrap();
    let finished = terminal(&db.store, sent.run.id).await;
    assert_eq!(finished.status, RunStatus::Uncertain);
    assert_eq!(finished.error_code.as_deref(), Some("TOOL_OUTCOME_UNKNOWN"));
    let tools = db.store.list_tools(sent.run.id).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].status, "uncertain");
    assert!(tools[0].output.is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    runner.shutdown().await.unwrap();
    connections.shutdown().await.unwrap();
    children_exited(&directory).await;

    let mut reopened = connect(&directory, "hang_note").await;
    let replacement = Arc::new(FixtureModel::new(
        tool_name(&reopened, "hang_note"),
        json!({}),
        "must not replay",
    ));
    let runner = start(db.store.clone(), &reopened, replacement.clone()).await;
    let retried = db
        .store
        .enqueue(conversation.id, "uncertain-input", "完了を待つ")
        .await
        .unwrap();
    assert!(!retried.created);
    assert_eq!(retried.run.id, sent.run.id);
    assert_eq!(retried.run.status, RunStatus::Uncertain);
    assert!(
        db.store
            .enqueue(conversation.id, "different-key", "次の作業")
            .await
            .is_err()
    );
    assert_eq!(
        db.store.get_run(sent.run.id).await.unwrap().status,
        RunStatus::Uncertain
    );
    runner.shutdown().await.unwrap();
    assert_eq!(replacement.calls.load(Ordering::SeqCst), 0);
    assert_eq!(directory.call_count(), 1);
    reopened.shutdown().await.unwrap();
    children_exited(&directory).await;
    db.cleanup().await;
}

#[tokio::test]
async fn remote_tool_error_is_a_definitive_receipt_that_the_model_can_explain() {
    let directory = Directory::new();
    let db = Database::new().await;
    let mut connections = connect(&directory, "fail_note").await;
    let model = Arc::new(FixtureModel::new(
        tool_name(&connections, "fail_note"),
        json!({}),
        "ツールが操作を拒否しました",
    ));
    let runner = start(db.store.clone(), &connections, model.clone()).await;
    let conversation = db.store.create_conversation("MCPエラー確認").await.unwrap();
    let sent = db
        .store
        .enqueue(conversation.id, "error-input", "結果を説明して")
        .await
        .unwrap();
    runner.notifier().notify_one();
    let finished = terminal(&db.store, sent.run.id).await;
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.output_text, "ツールが操作を拒否しました");
    let tools = db.store.list_tools(sent.run.id).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].status, "completed");
    let receipt: Value = serde_json::from_str(tools[0].output.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["isError"], true);
    assert_eq!(receipt["structuredContent"]["code"], "FIXTURE_REFUSED");
    assert_eq!(model.receipts.lock().unwrap().as_slice(), &[receipt]);
    assert_eq!(directory.call_count(), 1);
    runner.shutdown().await.unwrap();
    connections.shutdown().await.unwrap();
    children_exited(&directory).await;
    db.cleanup().await;
}

#[tokio::test]
async fn mcp_list_cli_needs_no_database_or_openai_and_closes_its_child() {
    let directory = Directory::new();
    let config_path = directory.0.join("mcp.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&directory.config("echo_note")).unwrap(),
    )
    .unwrap();
    let output = tokio::task::spawn_blocking({
        let config_path = config_path.clone();
        move || {
            Command::new(env!("CARGO_BIN_EXE_by-human"))
                .args(["mcp", "list"])
                .env("BY_HUMAN_MCP_CONFIG", config_path)
                .env("DATABASE_URL", "synthetic-database-value-not-a-url")
                .env("OPENAI_API_KEY", "synthetic-openai-secret-do-not-forward")
                .env("BY_HUMAN_API_TOKEN", "synthetic-api-secret-do-not-forward")
                .stdin(Stdio::null())
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "mcp list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let tools = result["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["server"], "fixture");
    assert_eq!(tools[0]["remote_name"], "echo_note");
    assert!(tools[0]["name"].as_str().unwrap().starts_with("mcp_"));
    let stdout = String::from_utf8(output.stdout).unwrap();
    for forbidden in [
        "synthetic-openai-secret-do-not-forward",
        "synthetic-api-secret-do-not-forward",
        "synthetic-database-value-not-a-url",
        "tool_not_allowed",
    ] {
        assert!(!stdout.contains(forbidden));
    }
    let started = directory
        .entries()
        .into_iter()
        .find(|entry| entry["method"] == "fixture.started")
        .unwrap();
    assert_eq!(started["inherited_credentials"], json!([]));
    assert_eq!(directory.call_count(), 0);
    children_exited(&directory).await;
    assert!(Path::new(FIXTURE).is_absolute());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_discovery_and_sigterm_during_initialization_leave_no_child() {
    let directory = Directory::new();
    let invalid =
        McpConfig::from_json(&serde_json::to_vec(&directory.config("not_advertised")).unwrap())
            .unwrap();
    assert!(
        McpConnections::connect(&invalid, &CancellationToken::new())
            .await
            .is_err()
    );
    children_exited(&directory).await;
    assert_eq!(directory.call_count(), 0);

    let mut config = directory.config("echo_note");
    config["servers"][0]["transport"]["args"] =
        json!(["--journal", directory.journal(), "--hang-initialize"]);
    let config_path = directory.0.join("hanging-mcp.json");
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_by-human"))
        .args(["mcp", "list"])
        .env("BY_HUMAN_MCP_CONFIG", config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if directory
                .entries()
                .iter()
                .filter(|entry| entry["method"] == "initialize")
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("CLI never entered its second initialization");
    // Only the CLI process just spawned by this test receives SIGTERM.
    assert_eq!(
        unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("MCP discovery was cancelled"));
    children_exited(&directory).await;
    assert_eq!(directory.call_count(), 0);
}
