//! Owner-selected MCP tools, sharing the engine's durable dispatch boundary.
pub mod config;
mod transport;

use std::{collections::HashSet, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use rmcp::{
    Peer, RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientConfig, Implementation,
        PaginatedRequestParams,
    },
    service::RunningService,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::engine::{self, EngineError, EngineErrorKind, Tool, ToolDefinition, ToolRegistry};
use config::McpConfig;

pub type Result<T> = std::result::Result<T, McpError>;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_RECEIPT_BYTES: usize = 256 * 1024;

/// Only fixed diagnostics may cross the CLI/log boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct McpError(&'static str);

impl McpError {
    pub(crate) const fn new(message: &'static str) -> Self {
        Self(message)
    }
}
impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl fmt::Debug for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for McpError {}

#[derive(Clone, Serialize)]
pub struct McpToolInfo {
    pub server: String,
    pub remote_name: String,
    pub name: String,
    pub description: String,
}

/// Service ownership remains here; tools only hold peers, never spawn/reconnect.
#[derive(Default)]
pub struct McpConnections {
    services: Vec<RunningService<RoleClient, ClientConfig>>,
    tools: Vec<Arc<McpTool>>,
    children: Vec<transport::ChildStop>,
}

impl McpConnections {
    pub async fn connect(config: &McpConfig, cancel: &CancellationToken) -> Result<Self> {
        let mut connections = Self::default();
        for server in &config.servers {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let client = ClientConfig::new(
                ClientCapabilities::default(),
                Implementation::new("by-human", env!("CARGO_PKG_VERSION")),
            );
            let (service, child) =
                bounded_setup(transport::connect(&server.transport, client), cancel).await?;
            if let Some(child) = child {
                connections.children.push(child);
            }
            // Publish ownership before another await, so failure/cancellation
            // during catalog collection also closes the accepted connection.
            let peer = service.peer().clone();
            connections.services.push(service);
            let catalog = bounded_setup(discover(&peer), cancel).await?;
            for name in &server.tools {
                let tool = catalog
                    .iter()
                    .find(|tool| tool.name.as_ref() == name)
                    .ok_or_else(|| McpError::new("an allowed MCP tool was not advertised"))?;
                connections.tools.push(Arc::new(McpTool::new(
                    &server.id,
                    tool,
                    peer.clone(),
                    Duration::from_secs(server.timeout_seconds),
                )?));
            }
        }
        Ok(connections)
    }

    pub fn register(&self, registry: &mut ToolRegistry) -> Result<()> {
        for tool in &self.tools {
            registry
                .register(tool.clone())
                .map_err(|_| McpError::new("MCP tool registration failed"))?;
        }
        Ok(())
    }

    pub fn tools(&self) -> Vec<McpToolInfo> {
        self.tools.iter().map(|tool| tool.info.clone()).collect()
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        let mut failed = false;
        for service in &mut self.services {
            if !matches!(
                service.close_with_timeout(Duration::from_secs(2)).await,
                Ok(Some(_))
            ) {
                failed = true;
            }
        }
        for child in &self.children {
            child.kill();
        }
        if failed {
            Err(McpError::new(
                "MCP connection shutdown could not be confirmed",
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for McpConnections {
    fn drop(&mut self) {
        // Synchronous process-group termination also covers startup futures
        // abandoned by SIGTERM before a running service could be returned.
        for child in &self.children {
            child.kill();
        }
        for service in &self.services {
            service.cancellation_token().cancel();
        }
    }
}

async fn bounded_setup<T>(
    future: impl std::future::Future<Output = Result<T>>,
    cancel: &CancellationToken,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(cancelled()),
        result = tokio::time::timeout(SETUP_TIMEOUT, future) => result.map_err(|_| McpError::new("MCP setup timed out"))?,
    }
}

async fn discover(peer: &Peer<RoleClient>) -> Result<Vec<rmcp::model::Tool>> {
    let mut tools = Vec::new();
    let mut names = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor = None;
    let mut bytes = 0usize;
    for _ in 0..32 {
        let page = peer
            .list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor)))
            .await
            .map_err(|_| McpError::new("MCP tool discovery failed"))?;
        bytes = bytes.saturating_add(
            serde_json::to_vec(&page)
                .map_err(|_| invalid_catalog())?
                .len(),
        );
        if bytes > MAX_CATALOG_BYTES {
            return Err(invalid_catalog());
        }
        for tool in page.tools {
            if tool.name.is_empty()
                || tool.name.len() > 128
                || !names.insert(tool.name.to_string())
                || tools.len() >= 1024
            {
                return Err(invalid_catalog());
            }
            tools.push(tool);
        }
        match page.next_cursor {
            None => return Ok(tools),
            Some(next)
                if !next.is_empty() && next.len() <= 4096 && cursors.insert(next.clone()) =>
            {
                cursor = Some(next)
            }
            Some(_) => return Err(invalid_catalog()),
        }
    }
    Err(invalid_catalog())
}

struct McpTool {
    info: McpToolInfo,
    definition: ToolDefinition,
    validator: jsonschema::Validator,
    peer: Peer<RoleClient>,
    timeout: Duration,
}

impl McpTool {
    fn new(
        server: &str,
        tool: &rmcp::model::Tool,
        peer: Peer<RoleClient>,
        timeout: Duration,
    ) -> Result<Self> {
        let parameters = Value::Object(tool.input_schema.as_ref().clone());
        if parameters["type"] != "object"
            || serde_json::to_vec(&parameters)
                .map_err(|_| invalid_catalog())?
                .len()
                > 16 * 1024
        {
            return Err(invalid_catalog());
        }
        let validator = jsonschema::options()
            .with_retriever(NoExternalSchemas)
            .build(&parameters)
            .map_err(|_| invalid_catalog())?;
        let name = alias(server, tool.name.as_ref());
        let description = format!(
            "MCP tool {} on {}. {}",
            tool.name,
            server,
            tool.description.as_deref().unwrap_or_default()
        );
        if description.len() > 4096 {
            return Err(invalid_catalog());
        }
        let info = McpToolInfo {
            server: server.into(),
            remote_name: tool.name.to_string(),
            name: name.clone(),
            description: description.clone(),
        };
        Ok(Self {
            info,
            definition: ToolDefinition {
                name,
                description,
                parameters,
                strict: false,
            },
            validator,
            peer,
            timeout,
        })
    }
}

#[async_trait]
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn validate_arguments(&self, arguments: &Value) -> engine::Result<()> {
        if !arguments.is_object() || !self.validator.is_valid(arguments) {
            return Err(tool_error("MCP arguments failed validation"));
        }
        Ok(())
    }

    async fn execute(
        &self,
        arguments: &Value,
        cancel: &CancellationToken,
    ) -> engine::Result<String> {
        self.validate_arguments(arguments)?;
        let params = CallToolRequestParams::new(self.info.remote_name.clone())
            .with_arguments(arguments.as_object().expect("validated object").clone());
        // call_tool_once deliberately avoids the SDK's automatic MRTR rounds.
        // Dropping its request future sends cancellation, without replaying it.
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(EngineError::cancelled()),
            result = tokio::time::timeout(self.timeout, self.peer.call_tool_once(params)) => {
                result.map_err(|_| tool_error("MCP result timed out; outcome is unknown"))?
                    .map_err(|_| tool_error("MCP result is unavailable; outcome is unknown"))?
            }
        };
        let CallToolResponse::Complete(result) = response else {
            return Err(tool_error("MCP result is incomplete; outcome is unknown"));
        };
        // isError=true is a definitive server result, unlike a local timeout.
        let result = serde_json::to_string(&result)
            .map_err(|_| tool_error("MCP result could not be encoded"))?;
        if result.len() > MAX_RECEIPT_BYTES {
            return Err(tool_error("MCP result is too large; outcome is unknown"));
        }
        Ok(result)
    }
}

struct NoExternalSchemas;
impl jsonschema::Retrieve for NoExternalSchemas {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(std::io::Error::other("external MCP schemas are disabled").into())
    }
}

fn alias(server: &str, remote: &str) -> String {
    let digest = Sha256::digest(remote.as_bytes());
    let suffix: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("mcp_{server}_{suffix}")
}
fn invalid_catalog() -> McpError {
    McpError::new("MCP tool catalog is invalid or too large")
}
fn cancelled() -> McpError {
    McpError::new("MCP setup was cancelled")
}
fn tool_error(message: &'static str) -> EngineError {
    EngineError::new(EngineErrorKind::Tool, message)
}

#[cfg(test)]
mod tests;
