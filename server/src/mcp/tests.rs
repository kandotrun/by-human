use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::*;

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    IsError,
    Expired,
    Malformed,
    Huge,
    Sse,
    SseDisconnect,
    InputRequired,
    DuplicateNames,
    CyclicCursor,
    RemoteRef,
}
struct StateData {
    mode: Mode,
    init: AtomicUsize,
    calls: AtomicUsize,
    gets: AtomicUsize,
    resume_ids: Mutex<Vec<Option<String>>>,
    auth: Mutex<Vec<Option<String>>>,
    requests: Mutex<Vec<Value>>,
}
struct Fixture {
    state: Arc<StateData>,
    url: String,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(mode: Mode) -> Self {
        let state = Arc::new(StateData {
            mode,
            init: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            resume_ids: Mutex::new(vec![]),
            auth: Mutex::new(vec![]),
            requests: Mutex::new(vec![]),
        });
        let router = Router::new()
            .route(
                "/mcp",
                post(respond)
                    .get(get_events)
                    .delete(|| async { StatusCode::NO_CONTENT }),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, url, task }
    }
    fn config(&self, id: &str) -> McpConfig {
        McpConfig::from_json(&serde_json::to_vec(&json!({"servers":[{
            "id":id, "transport":{"type":"http","url":self.url}, "tools":["echo"], "timeout_seconds":1
        }]})).unwrap()).unwrap()
    }
    async fn connected(&self) -> McpConnections {
        McpConnections::connect(&self.config("fixture"), &CancellationToken::new())
            .await
            .unwrap()
    }
}

fn descriptor(name: &str) -> Value {
    json!({"name":name,"description":"Return owner-selected text", "inputSchema":{
        "type":"object", "properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false
    }})
}

async fn get_events(State(state): State<Arc<StateData>>, headers: HeaderMap) -> Response {
    state.gets.fetch_add(1, Ordering::SeqCst);
    let last = headers
        .get("last-event-id")
        .map(|value| value.to_str().unwrap().to_owned());
    state.resume_ids.lock().unwrap().push(last.clone());
    if matches!(state.mode, Mode::SseDisconnect) && last.is_some() {
        let id = state
            .requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|request| request["method"] == "tools/call")
            .unwrap()["id"]
            .clone();
        let message = format!(
            "data: {}\n\n",
            json!({"jsonrpc":"2.0", "id":id,
            "result":{"content":[{"type":"text", "text":"must not resume"}]}})
        );
        return ([("content-type", "text/event-stream")], Body::from(message)).into_response();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}
async fn respond(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    state.auth.lock().unwrap().push(
        headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
    );
    state.requests.lock().unwrap().push(request.clone());
    let id = request["id"].clone();
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => {
            state.init.fetch_add(1, Ordering::SeqCst);
            return ([("mcp-session-id", "fixture-session")], Json(json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}
            }}))).into_response();
        }
        "tools/list" => {
            let mut tool = descriptor("echo");
            if matches!(state.mode, Mode::RemoteRef) {
                tool["inputSchema"]["properties"]["text"] =
                    json!({"$ref":"https://must-not-fetch.invalid/schema"});
            }
            if matches!(state.mode, Mode::DuplicateNames) {
                json!({"tools":[tool.clone(),tool]})
            } else if matches!(state.mode, Mode::CyclicCursor) {
                json!({"tools":[],"nextCursor":"same"})
            } else {
                json!({"tools":[tool,descriptor("not_allowed")]})
            }
        }
        "tools/call" => {
            state.calls.fetch_add(1, Ordering::SeqCst);
            match state.mode {
                Mode::Expired => return StatusCode::NOT_FOUND.into_response(),
                Mode::Malformed => return ([("content-type","application/json")], "private-error-body-not-json").into_response(),
                Mode::Huge => return Json(json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"x".repeat(1024*1024+10)}]}})).into_response(),
                Mode::InputRequired => return Json(json!({"jsonrpc":"2.0","id":id,"result":{"resultType":"input_required","requestState":"opaque-state","inputRequests":[]}})).into_response(),
                Mode::SseDisconnect => return ([("content-type", "text/event-stream")], Body::from(
                    "id: interrupted-call\nretry: 0\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":\"fixture\",\"progress\":1}}\n\n"
                )).into_response(),
                _ => {}
            }
            let result = json!({"content":[{"type":"text","text":request["params"]["arguments"]["text"]}],
                "structuredContent":{"accepted":true},"isError":matches!(state.mode,Mode::IsError)});
            if matches!(state.mode, Mode::Sse) {
                let message = format!(
                    "data: {}\n\n",
                    json!({"jsonrpc":"2.0","id":id,"result":result})
                );
                return ([("content-type", "text/event-stream")], Body::from(message))
                    .into_response();
            }
            result
        }
        _ => return StatusCode::ACCEPTED.into_response(),
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}

#[tokio::test]
async fn only_allowed_tools_are_registered_and_definitive_content_is_preserved() {
    let fixture = Fixture::new(Mode::Normal).await;
    let mut connections = fixture.connected().await;
    assert_eq!(connections.tools().len(), 1);
    let mut registry = ToolRegistry::default();
    connections.register(&mut registry).unwrap();
    let definition = connections.tools().remove(0);
    assert_eq!(definition.remote_name, "echo");
    assert!(definition.name.len() <= 64);
    let result = connections.tools[0]
        .execute(&json!({"text":"日本語"}), &CancellationToken::new())
        .await
        .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["content"][0]["text"], "日本語");
    assert_eq!(result["structuredContent"]["accepted"], true);
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .state
            .auth
            .lock()
            .unwrap()
            .iter()
            .all(Option::is_none)
    );
    let initialize = fixture
        .state
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| r["method"] == "initialize")
        .unwrap()
        .clone();
    assert_eq!(initialize["params"]["capabilities"], json!({}));
    connections.shutdown().await.unwrap();
}

#[tokio::test]
async fn argument_validation_prevents_network_dispatch() {
    let fixture = Fixture::new(Mode::Normal).await;
    let mut connections = fixture.connected().await;
    for input in [
        json!({}),
        json!({"text":1}),
        json!({"text":"ok","extra":true}),
        json!([]),
    ] {
        assert!(
            connections.tools[0]
                .execute(&input, &CancellationToken::new())
                .await
                .is_err()
        );
    }
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    connections.shutdown().await.unwrap();
}

#[tokio::test]
async fn server_tool_error_remains_a_definitive_result() {
    let fixture = Fixture::new(Mode::IsError).await;
    let mut connections = fixture.connected().await;
    let result = connections.tools[0]
        .execute(&json!({"text":"failure"}), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&result).unwrap()["isError"],
        true
    );
    connections.shutdown().await.unwrap();
}

#[tokio::test]
async fn sse_results_use_the_same_bounded_protocol_path() {
    let fixture = Fixture::new(Mode::Sse).await;
    let mut connections = fixture.connected().await;
    let result = connections.tools[0]
        .execute(&json!({"text":"streamed"}), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&result).unwrap()["content"][0]["text"],
        "streamed"
    );
    connections.shutdown().await.unwrap();
}

#[tokio::test]
async fn sse_disconnect_does_not_resume_even_when_the_server_requests_retry() {
    let fixture = Fixture::new(Mode::SseDisconnect).await;
    let mut connections = fixture.connected().await;
    let result = connections.tools[0]
        .execute(&json!({"text":"once"}), &CancellationToken::new())
        .await;
    assert!(
        result.is_err(),
        "a disconnected call must not obtain a receipt by resuming"
    );
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    assert!(fixture.state.gets.load(Ordering::SeqCst) <= 1);
    assert!(
        fixture
            .state
            .resume_ids
            .lock()
            .unwrap()
            .iter()
            .all(Option::is_none)
    );
    connections.shutdown().await.unwrap();
}

#[tokio::test]
async fn transport_and_incomplete_failures_never_replay_or_fabricate_a_receipt() {
    for mode in [
        Mode::Expired,
        Mode::Malformed,
        Mode::Huge,
        Mode::InputRequired,
    ] {
        let fixture = Fixture::new(mode).await;
        let mut connections = fixture.connected().await;
        let error = connections.tools[0]
            .execute(&json!({"text":"once"}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-error-body"));
        assert!(!error.to_string().contains(&fixture.url));
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.state.init.load(Ordering::SeqCst), 1);
        connections.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn invalid_catalogs_fail_without_executing_tools() {
    for mode in [Mode::DuplicateNames, Mode::CyclicCursor, Mode::RemoteRef] {
        let fixture = Fixture::new(mode).await;
        assert!(
            McpConnections::connect(&fixture.config("fixture"), &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 0);
    }
    let fixture = Fixture::new(Mode::Normal).await;
    let mut config = fixture.config("fixture");
    config.servers[0].tools = vec!["absent".into()];
    assert!(
        McpConnections::connect(&config, &CancellationToken::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_setup_does_not_contact_the_server() {
    let fixture = Fixture::new(Mode::Normal).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        McpConnections::connect(&fixture.config("fixture"), &cancel)
            .await
            .is_err()
    );
    assert_eq!(fixture.state.init.load(Ordering::SeqCst), 0);
}

#[test]
fn aliases_are_stable_and_separate_servers_without_remote_name_syntax() {
    let remote = "a name.with/unsafe characters";
    assert_eq!(alias("one", remote), alias("one", remote));
    assert_ne!(alias("one", remote), alias("two", remote));
    assert_ne!(alias("one", "a"), alias("one", "b"));
    assert!(
        alias("one", remote)
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
}
