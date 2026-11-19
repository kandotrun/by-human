//! Single-owner dispatch, durable checkpoints and conservative crash recovery.
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    engine::{Engine, EngineError, EngineErrorKind, RuntimeEvent, RuntimeSink, ToolCall},
    store::{Run, RunCompletion, RunStatus, RunnerGuard, Store, StoreError},
};

const MAX_DISPLAY_BYTES: usize = 1_048_576;

pub struct Runner;

pub struct RunnerHandle {
    notifier: Arc<Notify>,
    shutdown: CancellationToken,
    fatal: CancellationToken,
    task: JoinHandle<Result<(), RunnerError>>,
}

#[derive(Debug)]
pub enum RunnerError {
    Storage,
    OwnershipLost,
    TaskFailed,
    ShutdownTimeout,
}

impl fmt::Display for RunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Storage => "runner storage unavailable",
            Self::OwnershipLost => "runner ownership lost",
            Self::TaskFailed => "runner task failed",
            Self::ShutdownTimeout => "runner shutdown could not be confirmed",
        })
    }
}
impl std::error::Error for RunnerError {}

impl Runner {
    pub async fn start(
        store: Store,
        engine: Arc<Engine>,
        shutdown: CancellationToken,
    ) -> Result<RunnerHandle, RunnerError> {
        let mut guard = store
            .acquire_owner()
            .await
            .map_err(|_| RunnerError::OwnershipLost)?;
        store
            .recover_interrupted(&mut guard)
            .await
            .map_err(|_| RunnerError::Storage)?;
        let notifier = Arc::new(Notify::new());
        let fatal = CancellationToken::new();
        let task = tokio::spawn(dispatch(
            store,
            engine,
            guard,
            notifier.clone(),
            shutdown.clone(),
            fatal.clone(),
        ));
        Ok(RunnerHandle {
            notifier,
            shutdown,
            fatal,
            task,
        })
    }
}

impl RunnerHandle {
    pub fn notifier(&self) -> Arc<Notify> {
        self.notifier.clone()
    }
    pub fn fatal_token(&self) -> CancellationToken {
        self.fatal.clone()
    }

    pub async fn shutdown(mut self) -> Result<(), RunnerError> {
        self.shutdown.cancel();
        match tokio::time::timeout(Duration::from_secs(8), &mut self.task).await {
            Ok(result) => result.map_err(|_| RunnerError::TaskFailed)?,
            Err(_) => {
                // Dropping this future releases the ownership guard. The old
                // running row stays uncertain on the next startup, never queued.
                self.task.abort();
                let _ = self.task.await;
                Err(RunnerError::ShutdownTimeout)
            }
        }
    }
}

async fn dispatch(
    store: Store,
    engine: Arc<Engine>,
    mut guard: RunnerGuard,
    notifier: Arc<Notify>,
    shutdown: CancellationToken,
    fatal: CancellationToken,
) -> Result<(), RunnerError> {
    let mut result = async {
        let owner = guard.owner_id();
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            check_owner(&mut guard).await?;
            let run = store
                .claim_next(owner)
                .await
                .map_err(|_| RunnerError::Storage)?;
            if let Some(run) = run {
                execute_one(&store, &engine, &mut guard, run, &shutdown).await?;
            } else {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = notifier.notified() => {},
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                }
            }
        }
        Ok(())
    }
    .await;
    if result.is_ok()
        && !matches!(
            tokio::time::timeout(Duration::from_secs(2), guard.release()).await,
            Ok(Ok(()))
        )
    {
        result = Err(RunnerError::OwnershipLost);
    }
    if result.is_err() {
        fatal.cancel();
    }
    result
}

async fn check_owner(guard: &mut RunnerGuard) -> Result<(), RunnerError> {
    match tokio::time::timeout(Duration::from_secs(2), guard.check()).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(RunnerError::OwnershipLost),
    }
}

enum MonitorEnd {
    Fatal(RunnerError),
    CancellationTimeout,
}

async fn execute_one(
    store: &Store,
    engine: &Engine,
    guard: &mut RunnerGuard,
    run: Run,
    shutdown: &CancellationToken,
) -> Result<(), RunnerError> {
    let owner = guard.owner_id();
    let cancel = shutdown.child_token();
    let sink = DurableSink {
        store: store.clone(),
        run_id: run.id,
        owner,
        cancel: cancel.clone(),
        text: Mutex::new(String::new()),
    };
    let context = match store.context_for(run.id).await {
        Ok(context) => context,
        Err(StoreError::InvalidInput(_)) => {
            store
                .finish(
                    run.id,
                    owner,
                    RunCompletion {
                        status: RunStatus::Failed,
                        output_items: Vec::new(),
                        output_text: String::new(),
                        error_code: Some("CONTEXT_LIMIT_REACHED".to_owned()),
                    },
                )
                .await
                .map_err(|_| RunnerError::Storage)?;
            return Ok(());
        }
        Err(_) => return Err(RunnerError::Storage),
    };
    let mut future = Box::pin(engine.run(context, &sink, cancel.clone()));
    // Poll monitoring and execution concurrently. Guard checks share the
    // dedicated owner connection with durable checkpoints; awaiting a guard
    // check inside a selected tick branch would stop polling the engine while
    // it still held that connection's mutex.
    let mut monitor = Box::pin(async {
        let mut poll = tokio::time::interval(Duration::from_millis(100));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut cancellation_started = None;
        loop {
            poll.tick().await;
            if check_owner(guard).await.is_err() {
                return MonitorEnd::Fatal(RunnerError::OwnershipLost);
            }
            match tokio::time::timeout(Duration::from_secs(2), store.get_run(run.id)).await {
                Ok(Ok(current)) => {
                    if current.cancel_requested {
                        cancel.cancel();
                    }
                }
                _ => return MonitorEnd::Fatal(RunnerError::Storage),
            }
            if cancel.is_cancelled() {
                let started = cancellation_started.get_or_insert_with(tokio::time::Instant::now);
                if started.elapsed() >= Duration::from_secs(2) {
                    return MonitorEnd::CancellationTimeout;
                }
            }
        }
    });
    let outcome = tokio::select! {
        outcome = &mut future => outcome,
        stopped = &mut monitor => match stopped {
            MonitorEnd::CancellationTimeout => Err(EngineError::persistence()),
            MonitorEnd::Fatal(error) => {
                cancel.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(2), &mut future).await;
                return Err(error);
            }
        }
    };
    // Drop any unfinished provider/checkpoint future before taking the owner
    // connection to persist uncertainty. No cancelled future keeps its lock.
    drop(future);
    drop(monitor);
    let completion = match outcome {
        Ok(result) if result.text.len() > MAX_DISPLAY_BYTES => RunCompletion {
            status: RunStatus::Failed,
            output_items: Vec::new(),
            output_text: sink
                .text
                .lock()
                .map_err(|_| RunnerError::TaskFailed)?
                .clone(),
            error_code: Some("RUN_LIMIT_REACHED".to_owned()),
        },
        Ok(result) => {
            if !result.usage.is_empty() {
                store
                    .append_event(
                        run.id,
                        owner,
                        "model.usage",
                        json!({"requests":result.usage}),
                    )
                    .await
                    .map_err(|_| RunnerError::Storage)?;
            }
            RunCompletion {
                status: RunStatus::Completed,
                output_items: result.new_items,
                output_text: result.text,
                error_code: None,
            }
        }
        Err(error) => {
            let (status, code) = match error.kind {
                EngineErrorKind::Cancelled => (RunStatus::Cancelled, "CANCELLED"),
                EngineErrorKind::Persistence => (RunStatus::Uncertain, "PERSISTENCE_UNCERTAIN"),
                EngineErrorKind::Provider => (RunStatus::Failed, "PROVIDER_FAILED"),
                EngineErrorKind::Protocol => (RunStatus::Failed, "PROVIDER_PROTOCOL_FAILED"),
                EngineErrorKind::Tool => (RunStatus::Failed, "TOOL_FAILED"),
                EngineErrorKind::Limit => (RunStatus::Failed, "RUN_LIMIT_REACHED"),
            };
            RunCompletion {
                status,
                output_items: Vec::new(),
                output_text: sink
                    .text
                    .lock()
                    .map_err(|_| RunnerError::TaskFailed)?
                    .clone(),
                error_code: Some(code.to_owned()),
            }
        }
    };
    store
        .finish(run.id, owner, completion)
        .await
        .map_err(|_| RunnerError::Storage)?;
    Ok(())
}

struct DurableSink {
    store: Store,
    run_id: Uuid,
    owner: Uuid,
    cancel: CancellationToken,
    text: Mutex<String>,
}

fn storage_error(error: StoreError) -> EngineError {
    match error {
        StoreError::Cancelled => EngineError::cancelled(),
        _ => EngineError::persistence(),
    }
}

#[async_trait]
impl RuntimeSink for DurableSink {
    async fn checkpoint(&self) -> Result<(), EngineError> {
        if self.cancel.is_cancelled() {
            return Err(EngineError::cancelled());
        }
        self.store
            .ensure_running(self.run_id, self.owner)
            .await
            .map_err(storage_error)?;
        if self.cancel.is_cancelled() {
            return Err(EngineError::cancelled());
        }
        Ok(())
    }

    async fn emit(&self, event: RuntimeEvent) -> Result<(), EngineError> {
        let (kind, payload): (&str, Value) = match event {
            RuntimeEvent::TextDelta { delta } => {
                let size = self
                    .text
                    .lock()
                    .map_err(|_| EngineError::persistence())?
                    .len();
                if size.saturating_add(delta.len()) > MAX_DISPLAY_BYTES {
                    return Err(EngineError::new(
                        EngineErrorKind::Limit,
                        "display output limit reached",
                    ));
                }
                let mut start = 0;
                while start < delta.len() {
                    let mut end = (start + 8 * 1024).min(delta.len());
                    while !delta.is_char_boundary(end) {
                        end -= 1;
                    }
                    let chunk = &delta[start..end];
                    self.store
                        .append_event(
                            self.run_id,
                            self.owner,
                            "text_delta",
                            json!({"delta":chunk}),
                        )
                        .await
                        .map_err(storage_error)?;
                    self.text
                        .lock()
                        .map_err(|_| EngineError::persistence())?
                        .push_str(chunk);
                    start = end;
                }
                return Ok(());
            }
            // The intent/receipt transactions already emit authoritative tool
            // progress. Do not create a second UI row for the same operation.
            RuntimeEvent::ToolStarted { .. } | RuntimeEvent::ToolCompleted { .. } => return Ok(()),
            // A hosted tool runs at the provider. With no intent or receipt to
            // carry it, its progress is recorded as the same display events.
            RuntimeEvent::HostedToolStarted { call_id, name } => {
                ("tool.started", json!({"call_id":call_id,"name":name}))
            }
            RuntimeEvent::HostedToolCompleted { call_id, .. } => {
                ("tool.completed", json!({"call_id":call_id}))
            }
            RuntimeEvent::ModelStarted { request } => ("model_started", json!({"request":request})),
        };
        self.store
            .append_event(self.run_id, self.owner, kind, payload)
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    async fn before_tool(&self, call: &ToolCall) -> Result<(), EngineError> {
        self.checkpoint().await?;
        self.store
            .before_tool(
                self.run_id,
                self.owner,
                &call.call_id,
                &call.name,
                call.arguments.clone(),
            )
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    async fn after_tool(&self, call: &ToolCall, output: &str) -> Result<(), EngineError> {
        self.store
            .after_tool(self.run_id, self.owner, &call.call_id, output)
            .await
            .map_err(storage_error)
    }
}
