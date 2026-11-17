use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use tokio::task::JoinHandle;

use super::*;

#[derive(Default)]
struct Sink {
    events: Mutex<Vec<RuntimeEvent>>,
    before: AtomicUsize,
    after: AtomicUsize,
    fail_before: bool,
    fail_after: bool,
    stop_after_intent: Option<CancellationToken>,
    stop_after_receipt: Option<CancellationToken>,
    reject_checkpoint: bool,
}

#[async_trait]
impl RuntimeSink for Sink {
    async fn checkpoint(&self) -> Result<()> {
        if self.reject_checkpoint {
            Err(EngineError::cancelled())
        } else {
            Ok(())
        }
    }
    async fn emit(&self, event: RuntimeEvent) -> Result<()> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn before_tool(&self, _: &ToolCall) -> Result<()> {
        self.before.fetch_add(1, Ordering::SeqCst);
        if let Some(token) = &self.stop_after_intent {
            token.cancel();
        }
        if self.fail_before {
            Err(EngineError::persistence())
        } else {
            Ok(())
        }
    }
    async fn after_tool(&self, _: &ToolCall, result: &str) -> Result<()> {
        self.after.fetch_add(1, Ordering::SeqCst);
        let result: Value = serde_json::from_str(result).unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(result["utc"].as_str().unwrap()).is_ok());
        if let Some(token) = &self.stop_after_receipt {
            token.cancel();
        }
        if self.fail_after {
            Err(EngineError::persistence())
        } else {
            Ok(())
        }
    }
}

struct FixtureState {
    bodies: Mutex<VecDeque<Vec<Vec<u8>>>>,
    requests: Mutex<Vec<Value>>,
    authorizations: Mutex<Vec<String>>,
    status: StatusCode,
    hang: bool,
    content_type: Option<&'static str>,
}

struct Fixture {
    state: Arc<FixtureState>,
    url: String,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn start(bodies: Vec<Vec<Vec<u8>>>) -> Self {
        Self::with_options(bodies, StatusCode::OK, false).await
    }

    async fn with_options(bodies: Vec<Vec<Vec<u8>>>, status: StatusCode, hang: bool) -> Self {
        Self::with_content_type(bodies, status, hang, Some("text/event-stream")).await
    }

    async fn with_content_type(
        bodies: Vec<Vec<Vec<u8>>>,
        status: StatusCode,
        hang: bool,
        content_type: Option<&'static str>,
    ) -> Self {
        let state = Arc::new(FixtureState {
            bodies: Mutex::new(bodies.into()),
            requests: Mutex::new(vec![]),
            authorizations: Mutex::new(vec![]),
            status,
            hang,
            content_type,
        });
        let app = Router::new()
            .route("/v1/responses", post(respond))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/responses", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { state, url, task }
    }

    fn provider(&self, mode: AuthMode) -> OpenAIProvider {
        let mut config = ProviderConfig::new("fixture-model".into(), "fixture-secret".into());
        config.auth_mode = mode;
        OpenAIProvider::for_loopback_test(config, &self.url).unwrap()
    }
}

async fn respond(
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    state.authorizations.lock().unwrap().push(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    );
    state.requests.lock().unwrap().push(body);
    let chunks = state.bodies.lock().unwrap().pop_front().unwrap_or_default();
    let hang = state.hang;
    let stream = async_stream::stream! {
        for chunk in chunks { yield Ok::<Vec<u8>, Infallible>(chunk); }
        if hang { std::future::pending::<()>().await; }
    };
    let mut headers = HeaderMap::new();
    if let Some(content_type) = state.content_type {
        headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    }
    (state.status, headers, Body::from_stream(stream))
}

#[tokio::test]
async fn refreshed_token_preserves_plan_wire_options_and_original_provider() {
    let fixture = Fixture::start(vec![
        vec![complete(vec![message("first")])],
        vec![complete(vec![message("second")])],
    ])
    .await;
    let original = fixture.provider(AuthMode::ChatGptPlan);
    assert!(original.with_access_token("  ").is_err());
    let refreshed = original.with_access_token("fixture-refreshed").unwrap();
    let sink = Sink::default();
    let cancel = CancellationToken::new();
    refreshed.turn(&input(), &sink, &cancel).await.unwrap();
    original.turn(&input(), &sink, &cancel).await.unwrap();

    assert_eq!(
        *fixture.state.authorizations.lock().unwrap(),
        ["Bearer fixture-refreshed", "Bearer fixture-secret"]
    );
    let requests = fixture.state.requests.lock().unwrap();
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[0]["tools"][0]["type"], "namespace");
    assert!(requests[0].get("max_output_tokens").is_none());
}

fn frame(value: Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

fn complete(output: Vec<Value>) -> Vec<u8> {
    frame(json!({ "type": "response.completed", "response": {
        "status": "completed", "output": output,
        "usage": { "input_tokens": 4, "output_tokens": 2 }
    }}))
}

fn call(id: &str) -> Value {
    json!({ "type": "function_call", "id": format!("fc_{id}"), "call_id": id,
        "name": "get_current_time", "arguments": "{}", "status": "completed" })
}

fn message(text: &str) -> Value {
    json!({ "type": "message", "id": "m_1", "role": "assistant", "status": "completed",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }] })
}

fn input() -> Vec<Value> {
    vec![json!({ "role": "user", "content": "今の時刻を教えて" })]
}

#[tokio::test]
async fn empty_terminal_output_keeps_completed_stream_items_and_tool_context() {
    let reasoning = json!({"type":"reasoning","id":"reasoning_1","summary":[],"encrypted_content":"opaque-fixture"});
    let item_done = |index: usize, item: Value| {
        frame(json!({"type":"response.output_item.done","output_index":index,"item":item}))
    };
    let fixture = Fixture::start(vec![
        vec![
            item_done(1, call("clock")),
            item_done(0, reasoning.clone()),
            complete(vec![]),
        ],
        vec![
            frame(json!({"type":"response.output_text.delta","delta":"日本語です"})),
            item_done(0, message("日本語です")),
            complete(vec![]),
        ],
    ])
    .await;
    let sink = Sink::default();
    let result = Engine::new(
        Arc::new(fixture.provider(AuthMode::ChatGptPlan)),
        Limits::default(),
    )
    .run(input(), &sink, CancellationToken::new())
    .await
    .unwrap();
    assert_eq!(result.text, "日本語です");
    assert_eq!(result.new_items[0], reasoning);
    assert_eq!(result.new_items[1]["call_id"], "clock");
    assert_eq!(result.new_items[2]["type"], "function_call_output");
    assert_eq!(result.new_items[3], message("日本語です"));
    let requests = fixture.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["input"][1], reasoning);
    assert_eq!(requests[1]["input"][2]["call_id"], "clock");
    assert_eq!(requests[1]["input"][3]["call_id"], "clock");
    assert_eq!(sink.before.load(Ordering::SeqCst), 1);
    assert_eq!(sink.after.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unfinished_or_ambiguous_stream_items_never_authorize_a_tool() {
    let done = |index: usize| {
        frame(json!({"type":"response.output_item.done","output_index":index,"item":call("clock")}))
    };
    for chunks in [
        vec![done(0)],
        vec![done(0), frame(json!({"type":"response.incomplete"}))],
        vec![done(1), complete(vec![])],
        vec![done(0), done(0), complete(vec![])],
    ] {
        let fixture = Fixture::start(vec![chunks]).await;
        let sink = Sink::default();
        assert!(
            Engine::new(
                Arc::new(fixture.provider(AuthMode::ChatGptPlan)),
                Limits::default()
            )
            .run(input(), &sink, CancellationToken::new())
            .await
            .is_err()
        );
        assert_eq!(sink.before.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn missing_mime_still_requires_a_valid_completed_event_stream() {
    for content_type in [None, Some("Text/Event-Stream; charset=utf-8")] {
        let fixture = Fixture::with_content_type(
            vec![vec![complete(vec![message("日本語です")])]],
            StatusCode::OK,
            false,
            content_type,
        )
        .await;
        let result = Engine::new(
            Arc::new(fixture.provider(AuthMode::ChatGptPlan)),
            Limits::default(),
        )
        .run(input(), &Sink::default(), CancellationToken::new())
        .await
        .unwrap();
        assert_eq!(result.text, "日本語です");
    }
    for (content_type, body) in [
        (None, b"<html>not a stream</html>".to_vec()),
        (
            Some("application/json"),
            complete(vec![message("wrong declared format")]),
        ),
    ] {
        let fixture =
            Fixture::with_content_type(vec![vec![body]], StatusCode::OK, false, content_type).await;
        let sink = Sink::default();
        let error = Engine::new(
            Arc::new(fixture.provider(AuthMode::ChatGptPlan)),
            Limits::default(),
        )
        .run(input(), &sink, CancellationToken::new())
        .await
        .unwrap_err();
        assert_eq!(error.kind, EngineErrorKind::Protocol);
        assert_eq!(sink.before.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn refusal_explanation_is_streamed_as_public_text() {
    let text = "その依頼には対応できません。";
    let refusal = json!({"type":"message","id":"m_refusal","role":"assistant",
        "status":"completed","content":[{"type":"refusal","refusal":text}]});
    let fixture = Fixture::start(vec![vec![
        frame(json!({"type":"response.reasoning_text.delta","delta":"private reasoning"})),
        frame(json!({"type":"response.refusal.delta","delta":"その依頼には"})),
        frame(json!({"type":"response.refusal.delta","delta":"対応できません。"})),
        complete(vec![refusal]),
    ]])
    .await;
    let sink = Sink::default();
    let result = Engine::new(
        Arc::new(fixture.provider(AuthMode::ChatGptPlan)),
        Limits::default(),
    )
    .run(input(), &sink, CancellationToken::new())
    .await
    .unwrap();
    let streamed: String = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, text);
    assert_eq!(result.text, text);
}

#[tokio::test]
async fn http_turn_keeps_reasoning_call_identity_and_receipt_in_context() {
    let reasoning =
        json!({ "type": "reasoning", "id": "r_1", "summary": [], "encrypted_content": "opaque" });
    let fixture = Fixture::start(vec![
        vec![frame(json!({ "type": "response.function_call_arguments.delta", "delta": "{tool JSON is not chat}" })), complete(vec![reasoning.clone(), call("c_1")])],
        vec![frame(json!({ "type": "response.output_text.delta", "delta": "日本語です" })), complete(vec![message("日本語です")])],
    ]).await;
    let sink = Sink::default();
    let engine = Engine::new(
        Arc::new(fixture.provider(AuthMode::ApiKey)),
        Limits::default(),
    );
    let result = engine
        .run(input(), &sink, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.text, "日本語です");
    assert_eq!(result.new_items[0], reasoning);
    assert_eq!(result.new_items[1]["call_id"], "c_1");
    assert_eq!(result.new_items[2]["type"], "function_call_output");
    assert_eq!(result.new_items[2]["call_id"], "c_1");
    assert_eq!(result.usage.len(), 2);
    let requests = fixture.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["input"][1], reasoning);
    assert_eq!(requests[1]["input"][2]["call_id"], "c_1");
    assert_eq!(requests[1]["input"][3], result.new_items[2]);
    assert!(requests[0]["previous_response_id"].is_null());
    assert_eq!(requests[0]["store"], false);
    assert_eq!(requests[0]["stream"], true);
    assert_eq!(sink.before.load(Ordering::SeqCst), 1);
    assert_eq!(sink.after.load(Ordering::SeqCst), 1);
    let text: String = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            RuntimeEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "日本語です");
}

#[tokio::test]
async fn plan_auth_uses_namespaces_and_omits_unsupported_parameters() {
    let fixture = Fixture::start(vec![vec![complete(vec![message("ok")])]]).await;
    Engine::new(
        Arc::new(fixture.provider(AuthMode::ChatGptPlan)),
        Limits::default(),
    )
    .run(input(), &Sink::default(), CancellationToken::new())
    .await
    .unwrap();
    let requests = fixture.state.requests.lock().unwrap();
    let request = &requests[0];
    assert_eq!(request["tools"][0]["type"], "namespace");
    assert_eq!(request["tools"][0]["tools"][0]["name"], "get_current_time");
    assert!(request.get("max_output_tokens").is_none());
    assert!(request.get("previous_response_id").is_none());
    assert!(request.get("conversation").is_none());
    assert!(request.get("background").is_none());
    assert!(request.get("instructions").is_some());
}

#[test]
fn sse_handles_each_utf8_byte_crlf_and_multiline_data() {
    let bytes = "event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\r\ndata: \"delta\":\"日本語\"}\r\n\r\n".as_bytes();
    let mut decoder = sse::Decoder::default();
    let mut result = vec![];
    for byte in bytes {
        result.extend(decoder.feed(&[*byte]).unwrap());
    }
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].event, "response.output_text.delta");
    let value: Value = serde_json::from_str(&result[0].data).unwrap();
    assert_eq!(value["delta"], "日本語");
    let mut decoder = sse::Decoder::default();
    assert_eq!(decoder.feed(b"data: {}\r\r").unwrap().len(), 1);
}

#[tokio::test]
async fn incomplete_failed_and_unterminated_streams_do_not_execute_tools() {
    for (body, expected_kind) in [
        (
            frame(
                json!({ "type": "response.incomplete", "response": { "output": [call("c_1")] } }),
            ),
            EngineErrorKind::Provider,
        ),
        (
            frame(
                json!({ "type": "response.failed", "response": { "error": { "message": "fixture-secret" }, "output": [call("c_1")] } }),
            ),
            EngineErrorKind::Provider,
        ),
        (
            frame(json!({ "type": "response.output_item.done", "item": call("c_1") })),
            EngineErrorKind::Protocol,
        ),
        (
            b"data: {\"type\":\"response.completed\",\"response\":".to_vec(),
            EngineErrorKind::Protocol,
        ),
    ] {
        let fixture = Fixture::start(vec![vec![body]]).await;
        let sink = Sink::default();
        let error = Engine::new(
            Arc::new(fixture.provider(AuthMode::ApiKey)),
            Limits::default(),
        )
        .run(input(), &sink, CancellationToken::new())
        .await
        .unwrap_err();
        assert_eq!(error.kind, expected_kind);
        assert!(!error.to_string().contains("fixture-secret"));
        assert_eq!(sink.before.load(Ordering::SeqCst), 0);
        assert_eq!(sink.after.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn batch_validation_precedes_all_execution() {
    for invalid in [
        json!({ "type": "function_call", "call_id": "c_2", "name": "get_current_time", "arguments": "{broken" }),
        json!({ "type": "function_call", "call_id": "c_2", "name": "get_current_time", "arguments": "{\"extra\":true}" }),
        json!({ "type": "function_call", "call_id": "c_2", "name": "delete_everything", "arguments": "{}" }),
        call("c_1"),
    ] {
        let fixture = Fixture::start(vec![vec![complete(vec![call("c_1"), invalid])]]).await;
        let sink = Sink::default();
        assert!(
            Engine::new(
                Arc::new(fixture.provider(AuthMode::ApiKey)),
                Limits::default()
            )
            .run(input(), &sink, CancellationToken::new())
            .await
            .is_err()
        );
        assert_eq!(sink.before.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn duplicate_call_id_on_later_turn_is_not_replayed() {
    let fixture = Fixture::start(vec![
        vec![complete(vec![call("c_1")])],
        vec![complete(vec![call("c_1")])],
    ])
    .await;
    let sink = Sink::default();
    let error = Engine::new(
        Arc::new(fixture.provider(AuthMode::ApiKey)),
        Limits::default(),
    )
    .run(input(), &sink, CancellationToken::new())
    .await
    .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Tool);
    assert_eq!(sink.before.load(Ordering::SeqCst), 1);
    assert_eq!(sink.after.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn durable_intent_and_receipt_failures_stop_the_loop() {
    for fail_before in [true, false] {
        let fixture = Fixture::start(vec![vec![complete(vec![call("c_1")])]]).await;
        let sink = Sink {
            fail_before,
            fail_after: !fail_before,
            ..Sink::default()
        };
        let error = Engine::new(
            Arc::new(fixture.provider(AuthMode::ApiKey)),
            Limits::default(),
        )
        .run(input(), &sink, CancellationToken::new())
        .await
        .unwrap_err();
        assert_eq!(error.kind, EngineErrorKind::Persistence);
        assert_eq!(sink.after.load(Ordering::SeqCst), usize::from(!fail_before));
        assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn pre_cancel_and_durable_cancel_never_send_a_request() {
    for durable in [false, true] {
        let fixture = Fixture::start(vec![]).await;
        let token = CancellationToken::new();
        if !durable {
            token.cancel();
        }
        let sink = Sink {
            reject_checkpoint: durable,
            ..Sink::default()
        };
        let error = Engine::new(
            Arc::new(fixture.provider(AuthMode::ApiKey)),
            Limits::default(),
        )
        .run(input(), &sink, token)
        .await
        .unwrap_err();
        assert_eq!(error.kind, EngineErrorKind::Cancelled);
        assert!(fixture.state.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn cancelled_after_tool_keeps_receipt_and_stops_next_model() {
    let fixture = Fixture::start(vec![vec![complete(vec![call("c_1")])]]).await;
    let token = CancellationToken::new();
    let sink = Sink {
        stop_after_receipt: Some(token.clone()),
        ..Sink::default()
    };
    let error = Engine::new(
        Arc::new(fixture.provider(AuthMode::ApiKey)),
        Limits::default(),
    )
    .run(input(), &sink, token)
    .await
    .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Cancelled);
    assert_eq!(sink.after.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn accepted_readonly_intent_is_settled_when_stop_races_dispatch() {
    let fixture = Fixture::start(vec![vec![complete(vec![call("c_1")])]]).await;
    let token = CancellationToken::new();
    let sink = Sink {
        stop_after_intent: Some(token.clone()),
        ..Sink::default()
    };
    let error = Engine::new(
        Arc::new(fixture.provider(AuthMode::ApiKey)),
        Limits::default(),
    )
    .run(input(), &sink, token)
    .await
    .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Cancelled);
    assert_eq!(sink.after.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn stream_and_total_deadlines_are_bounded_without_replay() {
    let fixture = Fixture::start(vec![vec![complete(vec![message("too large")])]]).await;
    let mut config = ProviderConfig::new("fixture".into(), "fixture-secret".into());
    config.max_stream_bytes = 8;
    let provider = OpenAIProvider::for_loopback_test(config, &fixture.url).unwrap();
    let error = Engine::new(Arc::new(provider), Limits::default())
        .run(input(), &Sink::default(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Limit);
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);

    let fixture = Fixture::with_options(
        vec![vec![frame(json!({"type":"response.created"}))]],
        StatusCode::OK,
        true,
    )
    .await;
    let limits = Limits {
        max_duration: Duration::from_millis(50),
        ..Limits::default()
    };
    let error = Engine::new(Arc::new(fixture.provider(AuthMode::ApiKey)), limits)
        .run(input(), &Sink::default(), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Persistence);
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn completed_response_without_public_answer_is_not_success() {
    let fixture = Fixture::start(vec![vec![complete(vec![
        json!({"type":"reasoning","summary":[]}),
    ])]])
    .await;
    let error = Engine::new(
        Arc::new(fixture.provider(AuthMode::ApiKey)),
        Limits::default(),
    )
    .run(input(), &Sink::default(), CancellationToken::new())
    .await
    .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Protocol);
}

#[tokio::test]
async fn cancellation_interrupts_an_inflight_network_stream() {
    let fixture = Fixture::with_options(
        vec![vec![frame(json!({ "type": "response.created" }))]],
        StatusCode::OK,
        true,
    )
    .await;
    let provider = fixture.provider(AuthMode::ApiKey);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let state = fixture.state.clone();
    tokio::spawn(async move {
        while state.requests.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
    });
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        Engine::new(Arc::new(provider), Limits::default()).run(input(), &Sink::default(), token),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Cancelled);
}

#[tokio::test]
async fn limits_prevent_tool_dispatch_without_room_to_finish() {
    let fixture = Fixture::start(vec![vec![complete(vec![call("c_1")])]]).await;
    let sink = Sink::default();
    let limits = Limits {
        max_model_requests: 1,
        ..Limits::default()
    };
    let error = Engine::new(Arc::new(fixture.provider(AuthMode::ApiKey)), limits)
        .run(input(), &sink, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Limit);
    assert_eq!(sink.before.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refused_http_request_is_not_retried_and_error_is_sanitized() {
    let fixture = Fixture::with_options(
        vec![vec![b"fixture-secret".to_vec()]],
        StatusCode::TOO_MANY_REQUESTS,
        false,
    )
    .await;
    let error = Engine::new(
        Arc::new(fixture.provider(AuthMode::ApiKey)),
        Limits::default(),
    )
    .run(input(), &Sink::default(), CancellationToken::new())
    .await
    .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Provider);
    assert!(!error.to_string().contains("fixture-secret"));
    assert_eq!(fixture.state.requests.lock().unwrap().len(), 1);
}

#[test]
fn fixture_endpoint_cannot_send_credentials_to_remote_or_named_hosts() {
    for endpoint in [
        "http://localhost:123/v1/responses",
        "https://example.com/v1/responses",
        "http://127.0.0.1.evil/v1/responses",
        "http://user@127.0.0.1:123/v1/responses",
        "http://127.0.0.1:123/v1/responses?token=x",
    ] {
        assert!(
            OpenAIProvider::for_loopback_test(
                ProviderConfig::new("fixture".into(), "fixture-secret".into()),
                endpoint
            )
            .is_err()
        );
    }
}
