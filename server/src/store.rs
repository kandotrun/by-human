//! Durable state for one backend instance. The caller must retain and monitor
//! `RunnerGuard` throughout the runner lifetime, stopping work if `check` fails.
//! Uncertain runs are deliberately never replayed automatically.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use futures_util::{Stream, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{
    Connection, FromRow, PgConnection, PgPool, Postgres, Transaction, postgres::PgPoolOptions,
};
use uuid::Uuid;

type OwnerConnection = Arc<tokio::sync::Mutex<PgConnection>>;

struct OwnerState {
    id: Uuid,
    connection: OwnerConnection,
}

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
const UNTITLED_CONVERSATION: &str = "新しい会話";
pub(crate) const GENERATED_TITLE_CHARS: usize = 40;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Uncertain,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Uncertain
        )
    }
}

#[derive(Clone, Debug, Deserialize, FromRow, Serialize)]
pub struct Conversation {
    pub id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub(crate) title_pending: bool,
}

#[derive(Clone, Debug, Deserialize, FromRow, Serialize)]
pub struct Run {
    pub id: Uuid,
    pub ordinal: i64,
    pub conversation_id: Uuid,
    pub idempotency_key: String,
    pub input_text: String,
    pub status: RunStatus,
    pub cancel_requested: bool,
    pub owner_id: Option<Uuid>,
    pub output_text: String,
    pub output_items: Value,
    pub error_code: Option<String>,
    pub event_seq: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
    pub reviewed_event_seq: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Enqueued {
    pub run: Run,
    pub created: bool,
    pub conversation_title: String,
}

#[derive(Clone, Debug, Deserialize, FromRow, Serialize)]
pub struct Event {
    pub run_id: Uuid,
    pub seq: i64,
    pub kind: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub id: String,
    pub run_id: Uuid,
    pub role: &'static str,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, FromRow, Serialize)]
pub struct ToolExecution {
    pub run_id: Uuid,
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub status: String,
    pub output: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct RunCompletion {
    pub status: RunStatus,
    /// The original Responses output items and function_call_output items,
    /// including call IDs and reasoning items. Never flatten these into text.
    pub output_items: Vec<Value>,
    pub output_text: String,
    pub error_code: Option<String>,
}

#[derive(Clone, Eq, PartialEq)]
pub enum StoreError {
    InvalidInput(&'static str),
    NotFound,
    Conflict(&'static str),
    Cancelled,
    OwnershipLost,
    Storage,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(f, "{reason}"),
            Self::NotFound => f.write_str("not found"),
            Self::Conflict(reason) => write!(f, "{reason}"),
            Self::Cancelled => f.write_str("cancellation requested"),
            Self::OwnershipLost => f.write_str("runner ownership lost"),
            Self::Storage => f.write_str("database operation failed"),
        }
    }
}

impl fmt::Debug for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for StoreError {}

// SQL errors can contain connection details or user input in constraint detail.
// Neither Display nor Debug exposes that material to API responses or logs.
impl From<sqlx::Error> for StoreError {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
    active_owner: Arc<Mutex<Option<OwnerState>>>,
}

/// A dedicated PostgreSQL session owns the advisory lock. Dropping it closes
/// that session rather than returning a locked connection to the pool.
pub struct RunnerGuard {
    connection: OwnerConnection,
    owner_id: Uuid,
    active_owner: Arc<Mutex<Option<OwnerState>>>,
}

impl RunnerGuard {
    pub fn owner_id(&self) -> Uuid {
        self.owner_id
    }

    /// The host must cancel and stop its runner if this fails. SQLx does not
    /// reconnect this dedicated session or silently regain ownership.
    pub async fn check(&mut self) -> Result<()> {
        let mut connection = self.connection.lock().await;
        if sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut *connection)
            .await
            .is_err()
        {
            self.invalidate();
            return Err(StoreError::OwnershipLost);
        }
        if self
            .active_owner
            .lock()
            .map_err(|_| StoreError::OwnershipLost)?
            .as_ref()
            .map(|owner| owner.id)
            != Some(self.owner_id)
        {
            return Err(StoreError::OwnershipLost);
        }
        Ok(())
    }

    /// Explicit release makes shutdown and a subsequent owner deterministic.
    /// Dropping the guard also closes the dedicated session and invalidates it.
    pub async fn release(self) -> Result<()> {
        let mut connection = self.connection.lock().await;
        let result = sqlx::query("SELECT pg_advisory_unlock_all()")
            .execute(&mut *connection)
            .await;
        self.invalidate();
        result.map_err(|_| StoreError::OwnershipLost)?;
        Ok(())
    }

    fn invalidate(&self) {
        if let Ok(mut owner) = self.active_owner.lock()
            && owner.as_ref().map(|owner| owner.id) == Some(self.owner_id)
        {
            *owner = None;
        }
    }
}

impl Drop for RunnerGuard {
    fn drop(&mut self) {
        self.invalidate();
    }
}

impl Store {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
        Self::from_pool(pool).await
    }

    pub async fn from_pool(pool: PgPool) -> Result<Self> {
        MIGRATOR.run(&pool).await.map_err(|_| StoreError::Storage)?;
        Ok(Self {
            pool,
            active_owner: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn health(&self) -> Result<()> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn acquire_owner(&self) -> Result<RunnerGuard> {
        let mut connection =
            PgConnection::connect_with(self.pool.connect_options().as_ref()).await?;
        let acquired: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtextextended(current_database() || ':' || current_schema() || ':by-human-runner', 0))"
        ).fetch_one(&mut connection).await?;
        if !acquired {
            return Err(StoreError::Conflict("another backend owns this store"));
        }
        let owner_id = Uuid::new_v4();
        let connection = Arc::new(tokio::sync::Mutex::new(connection));
        *self
            .active_owner
            .lock()
            .map_err(|_| StoreError::OwnershipLost)? = Some(OwnerState {
            id: owner_id,
            connection: connection.clone(),
        });
        Ok(RunnerGuard {
            connection,
            owner_id,
            active_owner: self.active_owner.clone(),
        })
    }

    fn assert_owner(&self, owner_id: Uuid) -> Result<()> {
        self.owner_connection(owner_id).map(|_| ())
    }

    fn owner_connection(&self, owner_id: Uuid) -> Result<OwnerConnection> {
        let owner = self
            .active_owner
            .lock()
            .map_err(|_| StoreError::OwnershipLost)?;
        owner
            .as_ref()
            .filter(|owner| owner.id == owner_id)
            .map(|owner| owner.connection.clone())
            .ok_or(StoreError::OwnershipLost)
    }

    pub async fn create_conversation(&self, title: &str) -> Result<Conversation> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 {
            return Err(StoreError::InvalidInput(
                "title must contain 1 to 200 characters",
            ));
        }
        self.insert_conversation(title, false).await
    }

    pub async fn create_untitled_conversation(&self) -> Result<Conversation> {
        self.insert_conversation(UNTITLED_CONVERSATION, true).await
    }

    async fn insert_conversation(&self, title: &str, title_pending: bool) -> Result<Conversation> {
        Ok(sqlx::query_as(
            "INSERT INTO conversations (id, title, title_pending) VALUES ($1, $2, $3) RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(title)
        .bind(title_pending)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn get_conversation(&self, id: Uuid) -> Result<Conversation> {
        sqlx::query_as("SELECT * FROM conversations WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)
    }

    pub async fn list_conversations(&self, limit: i64) -> Result<Vec<Conversation>> {
        validate_limit(limit)?;
        Ok(
            sqlx::query_as(
                "SELECT * FROM conversations ORDER BY created_at DESC, id DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await?,
        )
    }

    pub async fn enqueue(&self, conversation_id: Uuid, key: &str, text: &str) -> Result<Enqueued> {
        if key.is_empty() || key.len() > 128 {
            return Err(StoreError::InvalidInput(
                "idempotency key must contain 1 to 128 bytes",
            ));
        }
        if text.trim().is_empty() || text.len() > 65_536 {
            return Err(StoreError::InvalidInput(
                "message must contain 1 to 65536 bytes",
            ));
        }
        let mut tx = self.pool.begin().await?;
        // Lock the conversation before inspecting idempotency and unfinished
        // state. This serializes even two different keys arriving together.
        let (mut conversation_title, title_pending): (String, bool) = sqlx::query_as(
            "SELECT title, title_pending FROM conversations WHERE id = $1 FOR UPDATE",
        )
        .bind(conversation_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;
        let prior: Option<Run> = sqlx::query_as(
            "SELECT * FROM runs WHERE conversation_id = $1 AND idempotency_key = $2",
        )
        .bind(conversation_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(run) = prior {
            if run.input_text != text {
                return Err(StoreError::Conflict(
                    "idempotency key already has different input",
                ));
            }
            return Ok(Enqueued {
                run,
                created: false,
                conversation_title,
            });
        }
        let unfinished: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM runs WHERE conversation_id = $1 AND (status IN ('queued', 'running') OR (status = 'uncertain' AND reviewed_at IS NULL)))")
            .bind(conversation_id).fetch_one(&mut *tx).await?;
        if unfinished {
            return Err(StoreError::Conflict(
                "conversation has unfinished or uncertain work",
            ));
        }
        let run: Run = sqlx::query_as("INSERT INTO runs (id, conversation_id, idempotency_key, input_text, status) VALUES ($1, $2, $3, $4, 'queued') RETURNING *")
            .bind(Uuid::new_v4()).bind(conversation_id).bind(key).bind(text).fetch_one(&mut *tx).await?;
        if title_pending {
            conversation_title = title_from_message(text);
            sqlx::query("UPDATE conversations SET title = $2, title_pending = FALSE WHERE id = $1")
                .bind(conversation_id)
                .bind(&conversation_title)
                .execute(&mut *tx)
                .await?;
        }
        append_system_event(&mut tx, run.id, "run.queued", json!({"status": "queued"})).await?;
        let run = fetch_run_tx(&mut tx, run.id).await?;
        tx.commit().await?;
        Ok(Enqueued {
            run,
            created: true,
            conversation_title,
        })
    }

    pub async fn get_run(&self, id: Uuid) -> Result<Run> {
        sqlx::query_as("SELECT * FROM runs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)
    }

    /// Acknowledge an unknown outcome without changing it or replaying work.
    /// The caller must have inspected this run and explicitly accepted that
    /// external effects may already have happened. Notes are owner input, never
    /// a tool receipt or a claim that execution completed.
    pub async fn resolve_uncertain(
        &self,
        id: Uuid,
        expected_event_seq: i64,
        note: &str,
    ) -> Result<Run> {
        if expected_event_seq < 0
            || note.trim().is_empty()
            || note.len() > 4096
            || note.contains('\0')
        {
            return Err(StoreError::InvalidInput("invalid outcome review"));
        }
        let mut tx = self.pool.begin().await?;
        let conversation_id: Uuid =
            sqlx::query_scalar("SELECT conversation_id FROM runs WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound)?;
        // Match enqueue's lock order, so new work cannot race the review's
        // durable note and its release of the conversation reservation.
        sqlx::query("SELECT id FROM conversations WHERE id = $1 FOR UPDATE")
            .bind(conversation_id)
            .execute(&mut *tx)
            .await?;
        let run = lock_run(&mut tx, id).await?;
        if run.status != RunStatus::Uncertain {
            return Err(StoreError::Conflict("only uncertain work can be reviewed"));
        }
        if run.reviewed_at.is_some() {
            if run.reviewed_event_seq == Some(expected_event_seq)
                && run.review_note.as_deref() == Some(note)
            {
                return Ok(run);
            }
            return Err(StoreError::Conflict("outcome review already recorded"));
        }
        if run.event_seq != expected_event_seq {
            return Err(StoreError::Conflict("outcome review is stale"));
        }
        sqlx::query("UPDATE runs SET reviewed_at = clock_timestamp(), review_note = $2, reviewed_event_seq = $3 WHERE id = $1")
            .bind(id).bind(note).bind(expected_event_seq).execute(&mut *tx).await?;
        // Public events carry no tool payloads or owner note. Explicit review
        // inspection and the public run view expose the separate note.
        append_system_event(
            &mut tx,
            id,
            "run.reviewed",
            json!({"status": "uncertain", "reviewed": true}),
        )
        .await?;
        let result = fetch_run_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn list_runs(&self, conversation_id: Uuid, limit: i64) -> Result<Vec<Run>> {
        validate_limit(limit)?;
        self.get_conversation(conversation_id).await?;
        self.recent_runs(conversation_id, limit).try_collect().await
    }

    pub(crate) fn recent_runs(
        &self,
        conversation_id: Uuid,
        limit: i64,
    ) -> impl Stream<Item = Result<Run>> + '_ {
        sqlx::query_as(
            "SELECT * FROM runs WHERE conversation_id = $1 ORDER BY ordinal DESC LIMIT $2",
        )
        .bind(conversation_id)
        .bind(limit)
        .fetch(&self.pool)
        .map_err(StoreError::from)
    }

    pub async fn claim_next(&self, owner_id: Uuid) -> Result<Option<Run>> {
        let connection = self.owner_connection(owner_id)?;
        let mut connection = connection.lock().await;
        self.assert_owner(owner_id)?;
        let mut tx = connection.begin().await?;
        let next: Option<Uuid> = sqlx::query_scalar("SELECT id FROM runs WHERE status = 'queued' ORDER BY ordinal FOR UPDATE SKIP LOCKED LIMIT 1")
            .fetch_optional(&mut *tx).await?;
        let Some(id) = next else { return Ok(None) };
        self.assert_owner(owner_id)?;
        sqlx::query("UPDATE runs SET status = 'running', owner_id = $2, started_at = clock_timestamp() WHERE id = $1")
            .bind(id).bind(owner_id).execute(&mut *tx).await?;
        append_system_event(&mut tx, id, "run.running", json!({"status": "running"})).await?;
        let run = fetch_run_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(Some(run))
    }

    pub async fn ensure_running(&self, id: Uuid, owner_id: Uuid) -> Result<()> {
        let connection = self.owner_connection(owner_id)?;
        let mut connection = connection.lock().await;
        self.assert_owner(owner_id)?;
        let run: Run = sqlx::query_as("SELECT * FROM runs WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *connection)
            .await?
            .ok_or(StoreError::NotFound)?;
        validate_running(&run, owner_id, false)
    }

    pub async fn request_cancel(&self, id: Uuid) -> Result<Run> {
        let mut tx = self.pool.begin().await?;
        let run = lock_run(&mut tx, id).await?;
        match run.status {
            RunStatus::Queued => {
                sqlx::query("UPDATE runs SET status = 'cancelled', cancel_requested = TRUE, finished_at = clock_timestamp() WHERE id = $1")
                    .bind(id).execute(&mut *tx).await?;
                append_system_event(&mut tx, id, "run.cancelled", json!({"status": "cancelled"}))
                    .await?;
            }
            RunStatus::Running if !run.cancel_requested => {
                sqlx::query("UPDATE runs SET cancel_requested = TRUE WHERE id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                append_system_event(
                    &mut tx,
                    id,
                    "run.cancel_requested",
                    json!({"status": "running", "cancel_requested": true}),
                )
                .await?;
            }
            RunStatus::Uncertain => {
                return Err(StoreError::Conflict(
                    "uncertain work requires outcome review",
                ));
            }
            _ => {}
        }
        let result = fetch_run_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn append_event(
        &self,
        id: Uuid,
        owner_id: Uuid,
        kind: &str,
        payload: Value,
    ) -> Result<Event> {
        validate_event(kind, &payload)?;
        let connection = self.owner_connection(owner_id)?;
        let mut connection = connection.lock().await;
        self.assert_owner(owner_id)?;
        let mut tx = connection.begin().await?;
        validate_running(&lock_run(&mut tx, id).await?, owner_id, true)?;
        let event = append_system_event(&mut tx, id, kind, payload).await?;
        tx.commit().await?;
        Ok(event)
    }

    pub async fn list_events(&self, id: Uuid, after: i64, limit: i64) -> Result<Vec<Event>> {
        validate_limit(limit)?;
        let run = self.get_run(id).await?;
        if after < 0 || after > run.event_seq {
            return Err(StoreError::InvalidInput("invalid event cursor"));
        }
        Ok(sqlx::query_as(
            "SELECT * FROM run_events WHERE run_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
        )
        .bind(id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Persist a unique intent before a tool may execute. Even a completed
    /// duplicate is rejected; a caller must never interpret it as permission
    /// to execute the operation again.
    pub async fn before_tool(
        &self,
        id: Uuid,
        owner_id: Uuid,
        call_id: &str,
        name: &str,
        arguments: Value,
    ) -> Result<()> {
        if call_id.is_empty() || call_id.len() > 256 || name.is_empty() || name.len() > 128 {
            return Err(StoreError::InvalidInput("invalid tool identity"));
        }
        if serialized_size(&arguments)? > 16_384 {
            return Err(StoreError::InvalidInput("tool arguments too large"));
        }
        let connection = self.owner_connection(owner_id)?;
        let mut connection = connection.lock().await;
        self.assert_owner(owner_id)?;
        let mut tx = connection.begin().await?;
        validate_running(&lock_run(&mut tx, id).await?, owner_id, false)?;
        let inserted = sqlx::query("INSERT INTO tool_executions (run_id, call_id, name, arguments, status) VALUES ($1, $2, $3, $4, 'started') ON CONFLICT (run_id, call_id) DO NOTHING")
            .bind(id).bind(call_id).bind(name).bind(&arguments).execute(&mut *tx).await?;
        if inserted.rows_affected() != 1 {
            return Err(StoreError::Conflict(
                "tool call already has a durable intent",
            ));
        }
        append_system_event(
            &mut tx,
            id,
            "tool.started",
            json!({"call_id": call_id, "name": name}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Store the actual receipt even if Stop raced the completed tool. The
    /// cancellation flag blocks new work, not recording work already done.
    pub async fn after_tool(
        &self,
        id: Uuid,
        owner_id: Uuid,
        call_id: &str,
        output: &str,
    ) -> Result<()> {
        if output.len() > 262_144 {
            return Err(StoreError::InvalidInput("tool output too large"));
        }
        let connection = self.owner_connection(owner_id)?;
        let mut connection = connection.lock().await;
        self.assert_owner(owner_id)?;
        let mut tx = connection.begin().await?;
        validate_running(&lock_run(&mut tx, id).await?, owner_id, true)?;
        let updated = sqlx::query("UPDATE tool_executions SET status = 'completed', output = $3, finished_at = clock_timestamp() WHERE run_id = $1 AND call_id = $2 AND status = 'started'")
            .bind(id).bind(call_id).bind(output).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::Conflict(
                "tool intent missing or already finalized",
            ));
        }
        append_system_event(&mut tx, id, "tool.completed", json!({"call_id": call_id})).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_tools(&self, id: Uuid) -> Result<Vec<ToolExecution>> {
        self.get_run(id).await?;
        Ok(sqlx::query_as(
            "SELECT * FROM tool_executions WHERE run_id = $1 ORDER BY created_at, call_id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn finish(&self, id: Uuid, owner_id: Uuid, completion: RunCompletion) -> Result<Run> {
        if !completion.status.is_terminal() {
            return Err(StoreError::InvalidInput("invalid completion status"));
        }
        if completion.output_text.len() > 1_048_576
            || serialized_size(&completion.output_items)? > 2_097_152
        {
            return Err(StoreError::InvalidInput("response too large"));
        }
        if completion.error_code.as_ref().is_some_and(|s| {
            s.len() > 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        }) {
            return Err(StoreError::InvalidInput("invalid error code"));
        }
        let connection = self.owner_connection(owner_id)?;
        let mut connection = connection.lock().await;
        self.assert_owner(owner_id)?;
        let mut tx = connection.begin().await?;
        let run = lock_run(&mut tx, id).await?;
        validate_running(&run, owner_id, true)?;
        let unknown_tool: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tool_executions WHERE run_id = $1 AND status != 'completed')")
            .bind(id).fetch_one(&mut *tx).await?;
        let status = if unknown_tool {
            RunStatus::Uncertain
        } else if run.cancel_requested {
            RunStatus::Cancelled
        } else {
            completion.status
        };
        let error_code = if unknown_tool {
            Some("TOOL_OUTCOME_UNKNOWN".to_owned())
        } else {
            completion.error_code
        };
        sqlx::query("UPDATE runs SET status = $2, output_text = $3, output_items = $4, error_code = $5, finished_at = clock_timestamp() WHERE id = $1")
            .bind(id).bind(status).bind(completion.output_text).bind(Value::Array(completion.output_items)).bind(error_code)
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE tool_executions SET status = 'uncertain' WHERE run_id = $1 AND status = 'started'")
            .bind(id).execute(&mut *tx).await?;
        append_system_event(&mut tx, id, "run.finished", json!({"status": status})).await?;
        let run = fetch_run_tx(&mut tx, id).await?;
        tx.commit().await?;
        Ok(run)
    }

    /// Call only after acquiring the singleton guard, before claiming work.
    /// Interrupted work remains inspectable and prevents an automatic retry.
    pub async fn recover_interrupted(&self, guard: &mut RunnerGuard) -> Result<u64> {
        if !Arc::ptr_eq(&self.active_owner, &guard.active_owner) {
            return Err(StoreError::OwnershipLost);
        }
        guard.check().await?;
        let mut connection = guard.connection.lock().await;
        self.assert_owner(guard.owner_id)?;
        let mut tx = connection.begin().await?;
        let ids: Vec<Uuid> = sqlx::query_scalar("UPDATE runs SET status = 'uncertain', error_code = 'INTERRUPTED', finished_at = clock_timestamp() WHERE status = 'running' RETURNING id")
            .fetch_all(&mut *tx).await?;
        for id in &ids {
            sqlx::query("UPDATE tool_executions SET status = 'uncertain' WHERE run_id = $1 AND status = 'started'")
                .bind(id).execute(&mut *tx).await?;
            append_system_event(
                &mut tx,
                *id,
                "run.finished",
                json!({"status": "uncertain", "error_code": "INTERRUPTED"}),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(ids.len() as u64)
    }

    pub async fn context_for(&self, id: Uuid) -> Result<Vec<Value>> {
        let target = self.get_run(id).await?;
        // This first runtime has no compaction yet. Refuse a bounded context
        // explicitly instead of loading unlimited JSON or silently dropping a
        // tool call, receipt, or reasoning item from the model's history.
        let (turns, bytes): (i64, i64) = sqlx::query_as(
            "SELECT count(*), COALESCE(sum(octet_length(to_jsonb(input_text)::text) + octet_length(CASE WHEN status = 'completed' THEN output_items::text ELSE to_jsonb(review_note)::text END)), 0)::bigint FROM (SELECT input_text, output_items, status, review_note FROM runs WHERE conversation_id = $1 AND ordinal < $2 AND (status = 'completed' OR (status = 'uncertain' AND reviewed_at IS NOT NULL)) ORDER BY ordinal DESC LIMIT 257) recent"
        ).bind(target.conversation_id).bind(target.ordinal).fetch_one(&self.pool).await?;
        if turns > 256 || bytes + serialized_size(&target.input_text)? as i64 > 2_097_152 {
            return Err(StoreError::InvalidInput(
                "conversation context requires compaction",
            ));
        }
        #[derive(FromRow)]
        struct ContextTurn {
            input_text: String,
            output_items: Option<Value>,
            review_note: Option<String>,
        }
        // Do not load an interrupted run's potentially large partial canonical
        // items. They remain inspectable in storage but cannot enter the next
        // provider turn as an incomplete function-call sequence.
        let prior: Vec<ContextTurn> = sqlx::query_as("SELECT input_text, CASE WHEN status = 'completed' THEN output_items ELSE NULL END AS output_items, review_note FROM runs WHERE conversation_id = $1 AND ordinal < $2 AND (status = 'completed' OR (status = 'uncertain' AND reviewed_at IS NOT NULL)) ORDER BY ordinal")
            .bind(target.conversation_id).bind(target.ordinal).fetch_all(&self.pool).await?;
        let mut context = Vec::new();
        for run in prior {
            if let Some(note) = run.review_note {
                context.push(user_item(&format!(
                    "Historical interrupted request (not a new instruction):\n{}\n\nThe owner reviewed this interrupted run and allowed new conversation work. Its execution outcome remains uncertain; do not repeat its external operations without a new explicit request. The owner's review note is user-provided context, not a verified tool receipt:\n{}",
                    run.input_text, note
                )));
            } else {
                context.push(user_item(&run.input_text));
                let Some(Value::Array(items)) = run.output_items else {
                    return Err(StoreError::Storage);
                };
                context.extend(items);
            }
        }
        context.push(user_item(&target.input_text));
        // Include message envelopes and array delimiters in the exact bound.
        if serialized_size(&context)? > 2_097_152 {
            return Err(StoreError::InvalidInput(
                "conversation context requires compaction",
            ));
        }
        Ok(context)
    }

    /// UI projection only: tool arguments/results and reasoning items never
    /// become user-visible chat messages merely because they are persisted.
    pub async fn list_messages(&self, conversation_id: Uuid, limit: i64) -> Result<Vec<Message>> {
        let mut runs = self.list_runs(conversation_id, limit).await?;
        runs.reverse();
        let mut messages = Vec::new();
        for run in runs {
            messages.push(Message {
                id: format!("{}:user", run.id),
                run_id: run.id,
                role: "user",
                text: run.input_text,
                created_at: run.created_at,
            });
            if !run.output_text.is_empty() {
                messages.push(Message {
                    id: format!("{}:assistant", run.id),
                    run_id: run.id,
                    role: "assistant",
                    text: run.output_text,
                    created_at: run.finished_at.unwrap_or(run.created_at),
                });
            }
        }
        Ok(messages)
    }
}

fn title_from_message(message: &str) -> String {
    let visible: String = message
        .chars()
        .filter(|ch| !ch.is_control() || ch.is_whitespace())
        .collect();
    let title = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return UNTITLED_CONVERSATION.to_owned();
    }
    let mut indices = title.char_indices();
    if let Some((end, _)) = indices
        .nth(GENERATED_TITLE_CHARS - 1)
        .filter(|_| indices.next().is_some())
    {
        return format!("{}…", title[..end].trim_end());
    }
    title
}

fn validate_running(run: &Run, owner_id: Uuid, allow_cancellation: bool) -> Result<()> {
    if run.status != RunStatus::Running || run.owner_id != Some(owner_id) {
        return Err(StoreError::Conflict("run is no longer owned and running"));
    }
    if run.cancel_requested && !allow_cancellation {
        return Err(StoreError::Cancelled);
    }
    Ok(())
}

fn validate_limit(limit: i64) -> Result<()> {
    if !(1..=1000).contains(&limit) {
        return Err(StoreError::InvalidInput("limit must be between 1 and 1000"));
    }
    Ok(())
}

fn serialized_size(value: &impl Serialize) -> Result<usize> {
    Ok(serde_json::to_vec(value)
        .map_err(|_| StoreError::InvalidInput("invalid JSON"))?
        .len())
}

fn validate_event(kind: &str, payload: &Value) -> Result<()> {
    if kind.is_empty() || kind.len() > 64 || serialized_size(payload)? > 65_536 {
        return Err(StoreError::InvalidInput("event exceeds limits"));
    }
    Ok(())
}

fn user_item(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "input_text", "text": text}]})
}

async fn fetch_run_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Run> {
    sqlx::query_as("SELECT * FROM runs WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::NotFound)
}

async fn lock_run(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Run> {
    sqlx::query_as("SELECT * FROM runs WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::NotFound)
}

async fn append_system_event(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    kind: &str,
    payload: Value,
) -> Result<Event> {
    let seq: i64 = sqlx::query_scalar(
        "UPDATE runs SET event_seq = event_seq + 1 WHERE id = $1 RETURNING event_seq",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(sqlx::query_as(
        "INSERT INTO run_events (run_id, seq, kind, payload) VALUES ($1, $2, $3, $4) RETURNING *",
    )
    .bind(id)
    .bind(seq)
    .bind(kind)
    .bind(payload)
    .fetch_one(&mut **tx)
    .await?)
}
