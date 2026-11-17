//! One catalog controls both advertised tools and permitted execution.
use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{EngineError, EngineErrorKind, Result, ToolCall};

pub const MAX_TOOL_ARGUMENT_BYTES: usize = 16 * 1024;
pub const MAX_TOOL_RESULT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    /// Validate without dispatching or producing external effects.
    fn validate_arguments(&self, arguments: &Value) -> Result<()>;

    /// Return only a definitive result. Cancellation or transport uncertainty
    /// must return an error, leaving the durable intent without a receipt.
    /// Implementations must not detach unbounded work from this future.
    async fn execute(&self, arguments: &Value, cancel: &CancellationToken) -> Result<String>;
}

struct Entry {
    definition: ToolDefinition,
    tool: Arc<dyn Tool>,
}

pub struct ToolRegistry {
    entries: BTreeMap<String, Entry>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        let mut registry = Self {
            entries: BTreeMap::new(),
        };
        registry
            .register(Arc::new(Clock))
            .expect("built-in clock definition is valid");
        registry
    }
}

impl ToolRegistry {
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<()> {
        let definition = tool.definition();
        if definition.name.is_empty()
            || definition.name.len() > 64
            || !definition
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            || !definition.parameters.is_object()
            || self.entries.contains_key(&definition.name)
            || self.entries.len() >= 128
        {
            return Err(EngineError::new(
                EngineErrorKind::Tool,
                "tool definition is invalid or already registered",
            ));
        }
        self.entries
            .insert(definition.name.clone(), Entry { definition, tool });
        Ok(())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.entries
            .values()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn validate_call(&self, call: &ToolCall) -> Result<()> {
        if call
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace != "by_human")
        {
            return Err(EngineError::new(
                EngineErrorKind::Tool,
                "unknown tool namespace",
            ));
        }
        let entry = self
            .entries
            .get(&call.name)
            .ok_or_else(|| EngineError::new(EngineErrorKind::Tool, "unknown tool was refused"))?;
        if !call.arguments.is_object()
            || serde_json::to_vec(&call.arguments)
                .map_err(|_| argument_error())?
                .len()
                > MAX_TOOL_ARGUMENT_BYTES
        {
            return Err(argument_error());
        }
        entry.tool.validate_arguments(&call.arguments)
    }

    pub async fn execute(&self, call: &ToolCall, cancel: &CancellationToken) -> Result<String> {
        self.validate_call(call)?;
        let entry = self
            .entries
            .get(&call.name)
            .ok_or_else(|| EngineError::new(EngineErrorKind::Tool, "unknown tool was refused"))?;
        let result = entry.tool.execute(&call.arguments, cancel).await?;
        if result.len() > MAX_TOOL_RESULT_BYTES {
            return Err(EngineError::new(
                EngineErrorKind::Limit,
                "tool result exceeds the receipt limit",
            ));
        }
        Ok(result)
    }
}

fn argument_error() -> EngineError {
    EngineError::new(EngineErrorKind::Tool, "tool arguments failed validation")
}

struct Clock;

#[async_trait]
impl Tool for Clock {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_current_time".into(),
            description: "Read the current UTC time. This does not change anything.".into(),
            parameters: json!({
                "type": "object", "properties": {}, "required": [], "additionalProperties": false
            }),
            strict: true,
        }
    }

    fn validate_arguments(&self, arguments: &Value) -> Result<()> {
        if arguments
            .as_object()
            .is_some_and(|object| object.is_empty())
        {
            Ok(())
        } else {
            Err(argument_error())
        }
    }

    async fn execute(&self, _: &Value, _: &CancellationToken) -> Result<String> {
        // This synchronous read is settled even when Stop races an accepted
        // intent. No external operation is being cancelled here.
        Ok(json!({ "utc": chrono::Utc::now().to_rfc3339() }).to_string())
    }
}
