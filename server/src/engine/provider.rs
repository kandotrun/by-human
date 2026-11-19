//! Responses HTTP/SSE only, with no automatic request replay.
//!
//! Protocol sources:
//! https://developers.openai.com/api/docs/guides/function-calling
//! https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations
//! https://developers.openai.com/api/reference/resources/responses/streaming-events

use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    EngineError, EngineErrorKind, Model, ModelTurn, Result, RuntimeEvent, RuntimeSink,
    ToolDefinition, checkpoint, protocol_error, sse::Decoder,
};

/// The tool name that run events report for OpenAI's hosted web search.
const WEB_SEARCH: &str = "web_search";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    ApiKey,
    ChatGptPlan,
}

/// Intentionally not Debug: credentials must not enter diagnostics.
pub struct ProviderConfig {
    pub model: String,
    pub api_key: String,
    pub auth_mode: AuthMode,
    pub max_output_tokens: u32,
    pub request_timeout: Duration,
    pub max_stream_bytes: usize,
    pub instructions: String,
}

impl ProviderConfig {
    pub fn new(model: String, api_key: String) -> Self {
        Self {
            model, api_key, auth_mode: AuthMode::ApiKey,
            max_output_tokens: 1024, request_timeout: Duration::from_secs(60),
            max_stream_bytes: 2 * 1024 * 1024,
            instructions: "You are by-human, the user's personal agent. Be clear and helpful. Use get_current_time when current UTC time is needed. Never claim that a tool ran unless its result is present.".into(),
        }
    }
}

pub struct OpenAIProvider {
    config: ProviderConfig,
    endpoint: Url,
    client: Client,
}

impl OpenAIProvider {
    pub fn new(config: ProviderConfig) -> Result<Self> {
        Self::build(
            config,
            Url::parse("https://api.openai.com/v1/responses").expect("static URL"),
        )
    }

    /// Replace the bearer token while sharing the existing connection pool.
    /// Authentication mode and all request options remain unchanged.
    pub fn with_access_token(&self, access_token: impl Into<String>) -> Result<Self> {
        let access_token = access_token.into();
        if access_token.trim().is_empty() {
            return Err(EngineError::new(
                EngineErrorKind::Protocol,
                "provider access token is empty",
            ));
        }
        Ok(Self {
            config: ProviderConfig {
                model: self.config.model.clone(),
                api_key: access_token,
                auth_mode: self.config.auth_mode,
                max_output_tokens: self.config.max_output_tokens,
                request_timeout: self.config.request_timeout,
                max_stream_bytes: self.config.max_stream_bytes,
                instructions: self.config.instructions.clone(),
            },
            endpoint: self.endpoint.clone(),
            client: self.client.clone(),
        })
    }

    /// Explicit fixture transport; no DNS names or remote credential destinations.
    pub fn for_loopback_test(config: ProviderConfig, endpoint: &str) -> Result<Self> {
        let url = Url::parse(endpoint).map_err(|_| protocol_error())?;
        let loopback = url
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|address| address.is_loopback());
        if url.scheme() != "http"
            || !loopback
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(EngineError::new(
                EngineErrorKind::Protocol,
                "test endpoint must be a literal loopback HTTP URL",
            ));
        }
        Self::build(config, url)
    }

    fn build(config: ProviderConfig, endpoint: Url) -> Result<Self> {
        if config.model.trim().is_empty()
            || config.api_key.trim().is_empty()
            || config.max_output_tokens == 0
            || config.request_timeout.is_zero()
            || config.max_stream_bytes == 0
        {
            return Err(EngineError::new(
                EngineErrorKind::Protocol,
                "provider configuration is incomplete",
            ));
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(config.request_timeout)
            .user_agent(concat!("by-human/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| {
                EngineError::new(
                    EngineErrorKind::Provider,
                    "HTTP client could not be initialized",
                )
            })?;
        Ok(Self {
            config,
            endpoint,
            client,
        })
    }

    fn body(&self, input: &[Value], definitions: &[ToolDefinition]) -> Value {
        let functions: Vec<_> = definitions
            .iter()
            .map(|definition| {
                json!({
                    "type": "function", "name": definition.name,
                    "description": definition.description,
                    "strict": definition.strict, "parameters": definition.parameters,
                })
            })
            .collect();
        let tools = match self.config.auth_mode {
            AuthMode::ApiKey => functions,
            AuthMode::ChatGptPlan => vec![json!({
                "type": "namespace", "name": "by_human", "description": "Configured by-human tools", "tools": functions
            })],
        };
        let mut body = json!({
            "model": self.config.model, "instructions": self.config.instructions,
            "input": input, "tools": tools, "store": false, "stream": true,
            "include": ["reasoning.encrypted_content"], "parallel_tool_calls": false
        });
        if self.config.auth_mode == AuthMode::ApiKey {
            body["max_output_tokens"] = json!(self.config.max_output_tokens);
        }
        body
    }

    async fn request(
        &self,
        input: &[Value],
        tools: &[ToolDefinition],
        sink: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> Result<ModelTurn> {
        checkpoint(sink, cancel).await?;
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(EngineError::cancelled()),
            result = self.client.post(self.endpoint.clone()).bearer_auth(&self.config.api_key).json(&self.body(input, tools)).send() =>
                result.map_err(|_| EngineError::new(EngineErrorKind::Provider, "provider request failed"))?,
        };
        if !response.status().is_success() {
            // Do not retain error bodies: they may echo prompts or credentials.
            return Err(EngineError::new(
                EngineErrorKind::Provider,
                "provider refused the request",
            ));
        }
        // The plan endpoint can omit Content-Type on a valid SSE response.
        // In that case the bounded decoder and completed-response checks still
        // validate the full protocol. A declared incompatible format is refused.
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .is_none_or(|header| {
                header.to_str().ok().is_some_and(|value| {
                    value
                        .split(';')
                        .next()
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
                })
            })
        {
            return Err(protocol_error());
        }
        let mut stream = response.bytes_stream();
        let mut decoder = Decoder::default();
        let mut completed_items = BTreeMap::new();
        let mut searches = HostedSearches::default();
        let mut received = 0usize;
        loop {
            let bytes = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(EngineError::cancelled()),
                next = stream.next() => match next {
                    Some(Ok(bytes)) => bytes,
                    Some(Err(_)) => return Err(EngineError::new(EngineErrorKind::Provider, "provider stream failed")),
                    None => return Err(protocol_error()),
                },
            };
            received = received.saturating_add(bytes.len());
            if received > self.config.max_stream_bytes {
                return Err(EngineError::new(
                    EngineErrorKind::Limit,
                    "provider stream is too large",
                ));
            }
            for event in decoder.feed(&bytes)? {
                if event.data == "[DONE]" {
                    return Err(protocol_error());
                }
                let value: Value =
                    serde_json::from_str(&event.data).map_err(|_| protocol_error())?;
                let kind = value["type"].as_str().ok_or_else(protocol_error)?;
                if !event.event.is_empty() && event.event != kind {
                    return Err(protocol_error());
                }
                match kind {
                    "response.output_item.done" => {
                        let index = value["output_index"]
                            .as_u64()
                            .and_then(|index| usize::try_from(index).ok())
                            .ok_or_else(protocol_error)?;
                        let item = value
                            .get("item")
                            .filter(|item| item.is_object())
                            .cloned()
                            .ok_or_else(protocol_error)?;
                        let finished = if item["type"] == "web_search_call" {
                            searches.complete(&item["id"])?
                        } else {
                            None
                        };
                        if completed_items.insert(index, item).is_some() {
                            return Err(protocol_error());
                        }
                        report(sink, cancel, finished).await?;
                    }
                    "response.output_item.added" if value["item"]["type"] == "web_search_call" => {
                        report(sink, cancel, searches.start(&value["item"]["id"])?).await?;
                    }
                    "response.web_search_call.in_progress"
                    | "response.web_search_call.searching" => {
                        report(sink, cancel, searches.start(&value["item_id"])?).await?;
                    }
                    "response.web_search_call.completed" => {
                        report(sink, cancel, searches.complete(&value["item_id"])?).await?;
                    }
                    "response.output_text.delta" | "response.refusal.delta" => {
                        let delta = value["delta"].as_str().ok_or_else(protocol_error)?;
                        checkpoint(sink, cancel).await?;
                        sink.emit(RuntimeEvent::TextDelta {
                            delta: delta.into(),
                        })
                        .await?;
                    }
                    "response.completed" => {
                        let response = &value["response"];
                        if response["status"] != "completed" {
                            return Err(protocol_error());
                        }
                        let mut output = response["output"]
                            .as_array()
                            .ok_or_else(protocol_error)?
                            .clone();
                        // Stateless plan streams may leave the terminal output
                        // array empty. Use only completed canonical items, never
                        // partial added items or reconstructed text deltas.
                        if output.is_empty() && !completed_items.is_empty() {
                            if !completed_items.keys().copied().eq(0..completed_items.len()) {
                                return Err(protocol_error());
                            }
                            output = completed_items.into_values().collect();
                        }
                        return Ok(ModelTurn {
                            output,
                            usage: response.get("usage").filter(|v| !v.is_null()).cloned(),
                        });
                    }
                    "response.failed" | "error" => {
                        return Err(EngineError::new(
                            EngineErrorKind::Provider,
                            "provider could not complete the response",
                        ));
                    }
                    "response.incomplete" => {
                        return Err(EngineError::new(
                            EngineErrorKind::Provider,
                            "provider response was incomplete",
                        ));
                    }
                    _ => {} // Non-public reasoning/argument/metadata deltas are not chat text.
                }
            }
        }
    }
}

/// Progress of OpenAI-hosted web searches within one response. The search
/// runs at OpenAI, so these events describe it and authorize nothing.
#[derive(Default)]
struct HostedSearches {
    started: HashSet<String>,
    completed: HashSet<String>,
}

impl HostedSearches {
    /// Report a search once, from whichever stream event shows it first.
    fn start(&mut self, id: &Value) -> Result<Option<RuntimeEvent>> {
        let id = hosted_item_id(id)?;
        Ok(self
            .started
            .insert(id.clone())
            .then(|| RuntimeEvent::HostedToolStarted {
                call_id: id,
                name: WEB_SEARCH.into(),
            }))
    }

    /// End a started search once; its completed event and item both end it.
    fn complete(&mut self, id: &Value) -> Result<Option<RuntimeEvent>> {
        let id = hosted_item_id(id)?;
        Ok(
            (self.started.contains(&id) && self.completed.insert(id.clone())).then(|| {
                RuntimeEvent::HostedToolCompleted {
                    call_id: id,
                    name: WEB_SEARCH.into(),
                }
            }),
        )
    }
}

/// Progress is stored as a run event, so its item ID is bounded like a call ID.
fn hosted_item_id(value: &Value) -> Result<String> {
    value
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .map(str::to_owned)
        .ok_or_else(protocol_error)
}

async fn report(
    sink: &dyn RuntimeSink,
    cancel: &CancellationToken,
    event: Option<RuntimeEvent>,
) -> Result<()> {
    if let Some(event) = event {
        checkpoint(sink, cancel).await?;
        sink.emit(event).await?;
    }
    Ok(())
}

#[async_trait]
impl Model for OpenAIProvider {
    async fn turn(
        &self,
        input: &[Value],
        tools: &[ToolDefinition],
        sink: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> Result<ModelTurn> {
        self.request(input, tools, sink, cancel).await
    }
}
