//! Bounded transports. Protocol/session handling stays in the official SDK.
use std::{
    collections::HashMap,
    fmt, io,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    Client, Response, StatusCode,
    header::{HeaderName, HeaderValue},
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientConfig, ClientJsonRpcMessage, ServerJsonRpcMessage},
    service::{RunningService, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{
        Transport,
        async_rw::AsyncRwTransport,
        common::client_side_sse::{BoxedSseResponse, NeverRetry},
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpClientTransport,
            StreamableHttpClientTransportConfig, StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::sync::CancellationToken;

use super::{McpError, Result, config::McpTransportConfig};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub(super) async fn connect(
    config: &McpTransportConfig,
    client: ClientConfig,
) -> Result<(RunningService<RoleClient, ClientConfig>, Option<ChildStop>)> {
    match config {
        McpTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut command = Command::new(command);
            command
                .args(args)
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            if let Some(path) = std::env::var_os("PATH") {
                command.env("PATH", path);
            }
            for name in env {
                let value = std::env::var_os(name)
                    .ok_or_else(|| McpError::new("an MCP environment variable is unavailable"))?;
                command.env(name, value);
            }
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            #[cfg(unix)]
            command.process_group(0);
            let transport = StdioTransport::spawn(command)?;
            let stop = transport.child.stop.clone();
            let mut startup = StartupChild {
                stop: stop.clone(),
                disarmed: false,
            };
            let service = client
                .serve_with_ct(transport, CancellationToken::new())
                .await
                .map_err(|_| McpError::new("MCP initialization failed"))?;
            startup.disarmed = true;
            Ok((service, Some(stop)))
        }
        McpTransportConfig::Http {
            url,
            bearer_token_env,
        } => {
            let token = bearer_token_env
                .as_ref()
                .map(|name| {
                    std::env::var(name).map_err(|_| {
                        McpError::new("an MCP bearer environment variable is unavailable")
                    })
                })
                .transpose()?;
            if token.as_ref().is_some_and(|value| {
                value.is_empty()
                    || value.len() > 4096
                    || !value.bytes().all(|byte| byte.is_ascii_graphic())
            }) {
                return Err(McpError::new("MCP bearer credential is invalid"));
            }
            let http = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(125))
                .user_agent(concat!("by-human/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|_| McpError::new("MCP HTTP client could not be initialized"))?;
            let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone())
                .reinit_on_expired_session(false)
                .max_concurrent_requests(1)
                .max_sse_event_size(MAX_MESSAGE_BYTES)
                .control_request_timeout(Duration::from_secs(1));
            config.retry_config = Arc::new(NeverRetry::default());
            config.auth_header = token;
            let transport = StreamableHttpClientTransport::with_client(
                BoundedHttp {
                    client: http,
                    initial_get_started: Arc::new(AtomicBool::new(false)),
                },
                config,
            );
            let service = client
                .serve_with_ct(transport, CancellationToken::new())
                .await
                .map_err(|_| McpError::new("MCP initialization failed"))?;
            Ok((service, None))
        }
    }
}

struct StartupChild {
    stop: ChildStop,
    disarmed: bool,
}
impl Drop for StartupChild {
    fn drop(&mut self) {
        if !self.disarmed {
            self.stop.kill();
        }
    }
}

#[derive(Clone)]
pub(super) struct ChildStop(Arc<AtomicU32>);
impl ChildStop {
    pub(super) fn kill(&self) {
        self.signal(libc::SIGKILL);
    }
    fn signal(&self, signal: i32) {
        let id = self.0.load(Ordering::SeqCst);
        if id == 0 {
            return;
        }
        #[cfg(unix)]
        // The child started a new process group; never target the application's group.
        unsafe {
            libc::kill(-(id as i32), signal);
        }
        #[cfg(not(unix))]
        let _ = signal;
    }
}

struct ChildGuard {
    child: Option<Child>,
    stop: ChildStop,
}
impl ChildGuard {
    async fn shutdown(&mut self) -> io::Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        self.stop.signal(libc::SIGTERM);
        let result = tokio::time::timeout(Duration::from_millis(300), child.wait()).await;
        // Also terminate descendants left behind by a server that already exited.
        self.stop.kill();
        if !matches!(result, Ok(Ok(_))) {
            child.start_kill()?;
            tokio::time::timeout(Duration::from_secs(1), child.wait())
                .await
                .map_err(|_| io::Error::other("MCP child did not exit"))??;
        }
        self.stop.0.store(0, Ordering::SeqCst);
        self.child.take();
        Ok(())
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop.kill();
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let stop = self.stop.clone();
                handle.spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                    stop.0.store(0, Ordering::SeqCst);
                });
            }
        }
    }
}

struct StdioTransport {
    child: ChildGuard,
    transport: AsyncRwTransport<RoleClient, ChildStdout, ChildStdin>,
}
impl StdioTransport {
    fn spawn(mut command: Command) -> Result<Self> {
        let mut child = command
            .spawn()
            .map_err(|_| McpError::new("MCP child could not be started"))?;
        let stop = ChildStop(Arc::new(AtomicU32::new(child.id().unwrap_or(0))));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::new("MCP child output is unavailable"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::new("MCP child input is unavailable"))?;
        Ok(Self {
            child: ChildGuard {
                child: Some(child),
                stop,
            },
            transport: AsyncRwTransport::new(stdout, stdin).with_max_line_length(MAX_MESSAGE_BYTES),
        })
    }
}
impl Transport<RoleClient> for StdioTransport {
    type Error = io::Error;
    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.transport.send(item)
    }
    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.transport.receive()
    }
    async fn close(&mut self) -> io::Result<()> {
        self.transport.close().await?;
        self.child.shutdown().await
    }
}

/// No provider body, URL, header, or reqwest source is retained by an error.
#[derive(Clone, Copy, Debug)]
struct HttpError;
impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCP HTTP transport failed")
    }
}
impl std::error::Error for HttpError {}
type HttpResult<T> = std::result::Result<T, StreamableHttpError<HttpError>>;
fn http_error() -> StreamableHttpError<HttpError> {
    StreamableHttpError::Client(HttpError)
}

#[derive(Clone)]
struct BoundedHttp {
    client: Client,
    initial_get_started: Arc<AtomicBool>,
}

impl BoundedHttp {
    fn request(
        &self,
        method: reqwest::Method,
        uri: &str,
        session: Option<&str>,
        token: Option<&str>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<reqwest::RequestBuilder> {
        let mut request = self.client.request(method, uri);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        for (name, value) in headers {
            if matches!(
                name.as_str(),
                "authorization"
                    | "proxy-authorization"
                    | "host"
                    | "cookie"
                    | "connection"
                    | "content-type"
                    | "content-length"
                    | "accept"
                    | "mcp-session-id"
                    | "last-event-id"
            ) {
                return Err(http_error());
            }
            request = request.header(name, value);
        }
        Ok(request)
    }

    async fn bounded_body(response: Response) -> HttpResult<Vec<u8>> {
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MESSAGE_BYTES as u64)
        {
            return Err(http_error());
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| http_error())?;
            if chunk.len() > MAX_MESSAGE_BYTES - body.len() {
                return Err(http_error());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn bounded_sse(response: Response) -> BoxedSseResponse {
        let bounded = response
            .bytes_stream()
            .scan((MAX_MESSAGE_BYTES, false), |state, chunk| {
                let result = if state.1 {
                    None
                } else {
                    match chunk {
                        Ok(chunk) if chunk.len() <= state.0 => {
                            state.0 -= chunk.len();
                            Some(Ok(chunk))
                        }
                        _ => {
                            state.1 = true;
                            Some(Err(HttpError))
                        }
                    }
                };
                std::future::ready(result)
            });
        sse_stream::SseStream::from_bytes_stream(bounded)
            .map(|event| {
                event.map_err(|_| {
                    rmcp::transport::streamable_http_client::SseError::Body(Box::new(HttpError))
                })
            })
            .boxed()
    }
}

impl StreamableHttpClient for BoundedHttp {
    type Error = HttpError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let response = self
            .request(
                reqwest::Method::POST,
                &uri,
                session_id.as_deref(),
                auth_header.as_deref(),
                headers,
            )?
            .header("accept", "application/json, text/event-stream")
            .json(&message)
            .send()
            .await
            .map_err(|_| http_error())?;
        if !response.status().is_success() {
            return Err(http_error());
        }
        if matches!(
            response.status(),
            StatusCode::ACCEPTED | StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| http_error())?;
        if session.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 1024
                || !value.bytes().all(|byte| byte.is_ascii_graphic())
        }) {
            return Err(http_error());
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match mime.as_str() {
            "text/event-stream" => Ok(StreamableHttpPostResponse::Sse(
                Self::bounded_sse(response),
                session,
            )),
            "application/json" => {
                let body = Self::bounded_body(response).await?;
                if body.is_empty() && !matches!(message, ClientJsonRpcMessage::Request(_)) {
                    return Ok(StreamableHttpPostResponse::Accepted);
                }
                let message: ServerJsonRpcMessage =
                    serde_json::from_slice(&body).map_err(|_| http_error())?;
                Ok(StreamableHttpPostResponse::Json(message, session))
            }
            _ if response.content_length() == Some(0)
                && !matches!(message, ClientJsonRpcMessage::Request(_)) =>
            {
                Ok(StreamableHttpPostResponse::Accepted)
            }
            _ => Err(http_error()),
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<()> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                &uri,
                Some(&session_id),
                auth_header.as_deref(),
                headers,
            )?
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .map_err(|_| http_error())?;
        if response.status().is_success()
            || matches!(
                response.status(),
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            )
        {
            Ok(())
        } else {
            Err(http_error())
        }
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<BoxedSseResponse> {
        // rmcp may request resumption before consulting NeverRetry, or honor a
        // server-supplied SSE retry interval. Permit only the initial standalone
        // notifications stream; refuse every resume/reconnect before any HTTP.
        if last_event_id.is_some() || self.initial_get_started.swap(true, Ordering::SeqCst) {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let request = self
            .request(
                reqwest::Method::GET,
                &uri,
                session_id.as_deref(),
                auth_header.as_deref(),
                headers,
            )?
            .header("accept", "text/event-stream");
        let response = request.send().await.map_err(|_| http_error())?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(http_error());
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if !mime.eq_ignore_ascii_case("text/event-stream") {
            return Err(http_error());
        }
        Ok(Self::bounded_sse(response))
    }
}
