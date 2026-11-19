//! Small, OpenAI-only execution loop. The host owns persistence and authority.
//!
//! The separation of model turns, tools, progress, and cancellation follows the
//! design in https://github.com/earendil-works/pi/tree/main/packages/agent .
//! No Pi code or runtime is embedded here.

mod provider;
mod sse;
mod tools;

use std::{collections::HashSet, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub use provider::{AuthMode, OpenAIProvider, ProviderConfig};
pub use tools::{
    MAX_TOOL_ARGUMENT_BYTES, MAX_TOOL_RESULT_BYTES, Tool, ToolDefinition, ToolRegistry,
};

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineErrorKind {
    Cancelled,
    Provider,
    Protocol,
    Tool,
    Persistence,
    Limit,
}

/// Safe diagnostics only: never retain a provider body, request, or credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub kind: EngineErrorKind,
    pub message: &'static str,
}

impl EngineError {
    pub const fn new(kind: EngineErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub const fn persistence() -> Self {
        Self::new(
            EngineErrorKind::Persistence,
            "durable state could not be confirmed",
        )
    }

    pub const fn cancelled() -> Self {
        Self::new(EngineErrorKind::Cancelled, "execution was cancelled")
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for EngineError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Only public-facing text and progress cross the UI boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    TextDelta { delta: String },
    ToolStarted { call_id: String, name: String },
    ToolCompleted { call_id: String, name: String },
    ModelStarted { request: usize },
}

#[async_trait]
pub trait RuntimeSink: Send + Sync {
    /// Recheck the durable run fence, owner, and cancellation state.
    async fn checkpoint(&self) -> Result<()>;
    async fn emit(&self, event: RuntimeEvent) -> Result<()>;
    /// Commit a unique execution intent before the tool can run.
    async fn before_tool(&self, call: &ToolCall) -> Result<()>;
    /// Commit the result even when Stop arrived while the tool was executing.
    async fn after_tool(&self, call: &ToolCall, result: &str) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct ModelTurn {
    /// Complete canonical response.output, including reasoning and call IDs.
    pub output: Vec<Value>,
    pub usage: Option<Value>,
}

#[async_trait]
pub trait Model: Send + Sync {
    async fn turn(
        &self,
        input: &[Value],
        tools: &[ToolDefinition],
        sink: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> Result<ModelTurn>;
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_model_requests: usize,
    pub max_duration: Duration,
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_model_requests: 8,
            max_duration: Duration::from_secs(120),
            max_output_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineResult {
    pub new_items: Vec<Value>,
    pub text: String,
    /// Provider-reported usage per request. Missing usage remains missing.
    pub usage: Vec<Value>,
}

pub struct Engine {
    model: Arc<dyn Model>,
    limits: Limits,
    tools: Arc<ToolRegistry>,
}

impl Engine {
    pub fn new(model: Arc<dyn Model>, limits: Limits) -> Self {
        Self::with_tools(model, limits, Arc::new(ToolRegistry::default()))
    }

    pub fn with_tools(model: Arc<dyn Model>, limits: Limits, tools: Arc<ToolRegistry>) -> Self {
        Self {
            model,
            limits,
            tools,
        }
    }

    pub async fn run(
        &self,
        input: Vec<Value>,
        sink: &dyn RuntimeSink,
        cancel: CancellationToken,
    ) -> Result<EngineResult> {
        if self.limits.max_model_requests == 0
            || self.limits.max_duration.is_zero()
            || self.limits.max_output_bytes == 0
        {
            return Err(EngineError::new(
                EngineErrorKind::Limit,
                "execution limits are empty",
            ));
        }
        // A deadline can interrupt a database receipt. It is not proof that the
        // tool did not run, so the host must conservatively reconcile this run.
        tokio::time::timeout(
            self.limits.max_duration,
            self.run_inner(input, sink, &cancel),
        )
        .await
        .map_err(|_| {
            EngineError::new(
                EngineErrorKind::Persistence,
                "execution deadline exceeded before settlement",
            )
        })?
    }

    async fn run_inner(
        &self,
        mut input: Vec<Value>,
        sink: &dyn RuntimeSink,
        cancel: &CancellationToken,
    ) -> Result<EngineResult> {
        let mut result = EngineResult {
            new_items: vec![],
            text: String::new(),
            usage: vec![],
        };
        let mut calls_seen = HashSet::new();
        let definitions = self.tools.definitions();
        for request in 1..=self.limits.max_model_requests {
            checkpoint(sink, cancel).await?;
            sink.emit(RuntimeEvent::ModelStarted { request }).await?;
            checkpoint(sink, cancel).await?;
            let turn = self.model.turn(&input, &definitions, sink, cancel).await?;
            checkpoint(sink, cancel).await?;
            // Validate the entire batch before any tool, including duplicate IDs.
            let calls = parse_calls(&turn.output, &calls_seen, &self.tools)?;
            append_bounded(
                &mut result.new_items,
                &turn.output,
                self.limits.max_output_bytes,
            )?;
            input.extend(turn.output.iter().cloned());
            result.text.push_str(&public_text(&turn.output));
            if let Some(usage) = turn.usage {
                result.usage.push(usage);
            }
            if calls.is_empty() && public_text(&turn.output).is_empty() {
                return Err(protocol_error());
            }
            if calls.is_empty() {
                return Ok(result);
            }
            // Do not execute a tool if no model request remains to consume its
            // receipt. The final allowed request can still produce a final answer.
            if request == self.limits.max_model_requests {
                return Err(EngineError::new(
                    EngineErrorKind::Limit,
                    "model request limit reached",
                ));
            }
            for call in calls {
                checkpoint(sink, cancel).await?;
                sink.emit(RuntimeEvent::ToolStarted {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                })
                .await?;
                checkpoint(sink, cancel).await?;
                // An accepted intent is the dispatch reservation. A definitive
                // result is persisted even when Stop races it; an error leaves
                // the intent unsettled for conservative recovery by the host.
                sink.before_tool(&call).await?;
                let tool_result = self.tools.execute(&call, cancel).await?;
                sink.after_tool(&call, &tool_result).await?;
                calls_seen.insert(call.call_id.clone());
                sink.emit(RuntimeEvent::ToolCompleted {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                })
                .await?;
                let mut output = json!({
                    "type": "function_call_output", "call_id": call.call_id, "output": tool_result,
                });
                if let Some(namespace) = call.namespace {
                    output["namespace"] = json!(namespace);
                }
                append_bounded(
                    &mut result.new_items,
                    &[output.clone()],
                    self.limits.max_output_bytes,
                )?;
                input.push(output);
                checkpoint(sink, cancel).await?;
            }
        }
        Err(EngineError::new(
            EngineErrorKind::Limit,
            "model request limit reached",
        ))
    }
}

pub(crate) async fn checkpoint(sink: &dyn RuntimeSink, cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(EngineError::cancelled());
    }
    sink.checkpoint().await?;
    if cancel.is_cancelled() {
        return Err(EngineError::cancelled());
    }
    Ok(())
}

fn append_bounded(target: &mut Vec<Value>, values: &[Value], limit: usize) -> Result<()> {
    let current = serde_json::to_vec(target)
        .map_err(|_| protocol_error())?
        .len();
    let added = serde_json::to_vec(values)
        .map_err(|_| protocol_error())?
        .len();
    if current.saturating_add(added) > limit {
        return Err(EngineError::new(
            EngineErrorKind::Limit,
            "model output is too large",
        ));
    }
    target.extend_from_slice(values);
    Ok(())
}

fn parse_calls(
    output: &[Value],
    previous: &HashSet<String>,
    tools: &ToolRegistry,
) -> Result<Vec<ToolCall>> {
    let mut batch = HashSet::new();
    let mut calls = Vec::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .ok_or_else(protocol_error)?;
                if previous.contains(id) || !batch.insert(id.to_owned()) {
                    return Err(EngineError::new(
                        EngineErrorKind::Tool,
                        "duplicate tool call was refused",
                    ));
                }
                let namespace = match item.get("namespace") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) if value == "by_human" => Some(value.clone()),
                    _ => {
                        return Err(EngineError::new(
                            EngineErrorKind::Tool,
                            "unknown tool namespace",
                        ));
                    }
                };
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(protocol_error)?;
                if item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s != "completed")
                {
                    return Err(protocol_error());
                }
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(protocol_error)?;
                if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                    return Err(EngineError::new(
                        EngineErrorKind::Tool,
                        "tool arguments failed validation",
                    ));
                }
                let arguments: Value =
                    serde_json::from_str(arguments).map_err(|_| protocol_error())?;
                let call = ToolCall {
                    call_id: id.to_owned(),
                    name: name.into(),
                    arguments,
                    namespace,
                };
                tools.validate_call(&call)?;
                calls.push(call);
            }
            Some("message") => {
                if item["role"] != "assistant"
                    || !item["content"].is_array()
                    || item
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s != "completed")
                {
                    return Err(protocol_error());
                }
            }
            // A hosted web search already ran at OpenAI inside this response.
            // Nothing runs locally, so it needs no intent or receipt. It stays
            // in the replayed input: stateless requests resend every item.
            Some("reasoning" | "web_search_call") => {}
            // Never silently complete a run containing an unsupported action.
            _ => return Err(protocol_error()),
        }
    }
    Ok(calls)
}

fn public_text(output: &[Value]) -> String {
    output
        .iter()
        .filter(|item| item["type"] == "message" && item["role"] == "assistant")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|content| match content["type"].as_str() {
            Some("output_text") => content["text"].as_str(),
            Some("refusal") => content["refusal"].as_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn protocol_error() -> EngineError {
    EngineError::new(
        EngineErrorKind::Protocol,
        "provider stream was not a complete supported response",
    )
}

#[cfg(test)]
mod tests;
