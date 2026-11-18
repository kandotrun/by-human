#![cfg(feature = "postgres-tests")]

use std::str::FromStr;

use by_human::store::{RunCompletion, RunStatus, Store, StoreError};
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    store: Store,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("DATABASE_URL")
            .expect("postgres-tests requires an isolated DATABASE_URL");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let schema = format!("byhuman_test_{}", Uuid::new_v4().simple());
        // SQL identifiers cannot use bind parameters. This identifier contains
        // only the fixed prefix and this test's generated UUID hex digits.
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
            store,
            schema,
        }
    }

    async fn cleanup(self) {
        self.pool.close().await;
        // The identifier is generated in this fixture, never accepted from a
        // connection string, environment variable, application, or user input.
        assert!(self.schema.starts_with("byhuman_test_"));
        assert!(
            self.schema
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        );
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

fn completed(items: Vec<Value>, text: &str) -> RunCompletion {
    RunCompletion {
        status: RunStatus::Completed,
        output_items: items,
        output_text: text.to_owned(),
        error_code: None,
    }
}

#[tokio::test]
async fn first_accepted_message_sets_automatic_title_once() {
    let f = Fixture::new().await;
    let automatic = f.store.create_untitled_conversation().await.unwrap();
    assert_eq!(automatic.title, "新しい会話");
    assert!(matches!(
        f.store.enqueue(automatic.id, "empty", "  \t ").await,
        Err(StoreError::InvalidInput(_))
    ));
    assert_eq!(
        f.store.get_conversation(automatic.id).await.unwrap().title,
        "新しい会話"
    );

    let message = format!(" \t日本語の\n タイトル\u{0007} {}", "井".repeat(50));
    let expected = format!("日本語の タイトル {}…", "井".repeat(29));
    let first = f
        .store
        .enqueue(automatic.id, "first", &message)
        .await
        .unwrap();
    assert!(first.created);
    assert_eq!(first.conversation_title, expected);
    let retry = f
        .store
        .enqueue(automatic.id, "first", &message)
        .await
        .unwrap();
    assert!(!retry.created);
    assert_eq!(retry.run.id, first.run.id);
    assert_eq!(retry.conversation_title, expected);

    f.store.request_cancel(first.run.id).await.unwrap();
    let next = f
        .store
        .enqueue(automatic.id, "next", "別の話題")
        .await
        .unwrap();
    assert_eq!(next.conversation_title, expected);
    assert_eq!(
        f.store.get_conversation(automatic.id).await.unwrap().title,
        expected
    );

    let manual = f.store.create_conversation("新しい会話").await.unwrap();
    let manual_run = f
        .store
        .enqueue(manual.id, "manual", "手動名は変更しない")
        .await
        .unwrap();
    assert_eq!(manual_run.conversation_title, "新しい会話");
    assert_eq!(
        f.store.get_conversation(manual.id).await.unwrap().title,
        "新しい会話"
    );
    f.cleanup().await;
}

#[tokio::test]
async fn concurrent_idempotency_and_conversation_reservation_are_atomic() {
    let f = Fixture::new().await;
    let conversation = f.store.create_conversation("日本語の会話").await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let store = f.store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .enqueue(conversation.id, "same-send", "こんにちは")
                .await
                .unwrap()
        }));
    }
    let mut first = None;
    let mut created = 0;
    for task in tasks {
        let result = task.await.unwrap();
        created += usize::from(result.created);
        assert_eq!(*first.get_or_insert(result.run.id), result.run.id);
    }
    assert_eq!(created, 1);
    assert_eq!(
        f.store.list_runs(conversation.id, 100).await.unwrap().len(),
        1
    );
    assert_eq!(
        f.store
            .list_events(first.unwrap(), 0, 100)
            .await
            .unwrap()
            .len(),
        1
    );

    let other = f.store.create_conversation("competing keys").await.unwrap();
    let a = f.store.clone();
    let b = f.store.clone();
    let (a, b) = tokio::join!(
        a.enqueue(other.id, "A", "first"),
        b.enqueue(other.id, "B", "second")
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(matches!(a.err().or(b.err()), Some(StoreError::Conflict(_))));
    f.cleanup().await;
}

#[tokio::test]
async fn idempotency_never_adopts_an_edited_payload_or_creates_a_new_retry() {
    let f = Fixture::new().await;
    let conversation = f.store.create_conversation("retry").await.unwrap();
    let first = f
        .store
        .enqueue(conversation.id, "original", "text A")
        .await
        .unwrap()
        .run;
    assert!(matches!(
        f.store.enqueue(conversation.id, "original", "text B").await,
        Err(StoreError::Conflict(_))
    ));
    f.store.request_cancel(first.id).await.unwrap();
    let retry = f
        .store
        .enqueue(conversation.id, "original", "text A")
        .await
        .unwrap();
    assert!(!retry.created);
    assert_eq!(retry.run.id, first.id);
    assert_eq!(retry.run.status, RunStatus::Cancelled);
    assert!(
        f.store
            .enqueue(conversation.id, "new-input", "text B")
            .await
            .unwrap()
            .created
    );
    f.cleanup().await;
}

#[tokio::test]
async fn stop_before_dispatch_removes_the_run_from_the_queue() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let conversation = f
        .store
        .create_conversation("stop before dispatch")
        .await
        .unwrap();
    let run = f
        .store
        .enqueue(conversation.id, "send", "hello")
        .await
        .unwrap()
        .run;
    assert_eq!(
        f.store.request_cancel(run.id).await.unwrap().status,
        RunStatus::Cancelled
    );
    assert!(
        f.store
            .claim_next(owner.owner_id())
            .await
            .unwrap()
            .is_none()
    );
    f.store.request_cancel(run.id).await.unwrap();
    assert_eq!(f.store.list_events(run.id, 0, 100).await.unwrap().len(), 2);
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn stop_preserves_the_receipt_but_cannot_complete_the_run() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let id = owner.owner_id();
    let conversation = f
        .store
        .create_conversation("stop while tool completes")
        .await
        .unwrap();
    let run = f
        .store
        .enqueue(conversation.id, "send", "get current time")
        .await
        .unwrap()
        .run;
    f.store.claim_next(id).await.unwrap().unwrap();
    f.store
        .before_tool(run.id, id, "call_1", "current_time", json!({}))
        .await
        .unwrap();
    f.store.request_cancel(run.id).await.unwrap();
    assert_eq!(
        f.store.ensure_running(run.id, id).await,
        Err(StoreError::Cancelled)
    );
    assert_eq!(
        f.store
            .before_tool(run.id, id, "call_2", "current_time", json!({}))
            .await,
        Err(StoreError::Cancelled)
    );
    f.store
        .after_tool(run.id, id, "call_1", "2026-11-17T00:00:00Z")
        .await
        .unwrap();
    let finished = f
        .store
        .finish(run.id, id, completed(vec![], "confirmed result"))
        .await
        .unwrap();
    assert_eq!(finished.status, RunStatus::Cancelled);
    let tools = f.store.list_tools(run.id).await.unwrap();
    assert_eq!(tools[0].status, "completed");
    assert_eq!(tools[0].output.as_deref(), Some("2026-11-17T00:00:00Z"));
    assert!(matches!(
        f.store
            .finish(run.id, id, completed(vec![], "late result"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn restart_marks_interrupted_work_uncertain_and_only_resumes_queued_work() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let old_owner = owner.owner_id();
    let a = f.store.create_conversation("interrupted").await.unwrap();
    let b = f.store.create_conversation("still queued").await.unwrap();
    let interrupted = f.store.enqueue(a.id, "A", "work A").await.unwrap().run;
    f.store.claim_next(old_owner).await.unwrap().unwrap();
    f.store
        .before_tool(
            interrupted.id,
            old_owner,
            "call_before_restart",
            "external_operation",
            json!({"reference": "synthetic"}),
        )
        .await
        .unwrap();
    let queued = f.store.enqueue(b.id, "B", "work B").await.unwrap().run;
    owner.release().await.unwrap();
    assert_eq!(
        f.store.ensure_running(interrupted.id, old_owner).await,
        Err(StoreError::OwnershipLost)
    );

    let restarted = Store::from_pool(f.pool.clone()).await.unwrap();
    let mut new_owner = restarted.acquire_owner().await.unwrap();
    assert_eq!(
        restarted.recover_interrupted(&mut new_owner).await.unwrap(),
        1
    );
    assert_eq!(
        restarted.get_run(interrupted.id).await.unwrap().status,
        RunStatus::Uncertain
    );
    assert_eq!(
        restarted.list_tools(interrupted.id).await.unwrap()[0].status,
        "uncertain"
    );
    let retried = restarted.enqueue(a.id, "A", "work A").await.unwrap();
    assert_eq!(retried.run.id, interrupted.id);
    assert!(!retried.created);
    assert!(matches!(
        restarted.enqueue(a.id, "new", "different work").await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        restarted
            .claim_next(new_owner.owner_id())
            .await
            .unwrap()
            .unwrap()
            .id,
        queued.id
    );
    assert!(matches!(
        restarted
            .finish(
                interrupted.id,
                new_owner.owner_id(),
                completed(vec![], "late")
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    new_owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn canonical_context_preserves_openai_items_and_excludes_future_or_cancelled_input() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let id = owner.owner_id();
    let conversation = f
        .store
        .create_conversation("canonical context")
        .await
        .unwrap();
    let a = f
        .store
        .enqueue(conversation.id, "A", "time please")
        .await
        .unwrap()
        .run;
    f.store.claim_next(id).await.unwrap().unwrap();
    f.store
        .before_tool(a.id, id, "call_A", "current_time", json!({}))
        .await
        .unwrap();
    f.store
        .after_tool(a.id, id, "call_A", "noon")
        .await
        .unwrap();
    let items = vec![
        json!({"type": "reasoning", "id": "rs_A", "encrypted_content": "synthetic-only"}),
        json!({"type": "function_call", "id": "fc_A", "call_id": "call_A", "name": "current_time", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "call_A", "output": "noon"}),
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "It is noon."}]}),
    ];
    f.store
        .finish(a.id, id, completed(items.clone(), "It is noon."))
        .await
        .unwrap();
    let cancelled = f
        .store
        .enqueue(conversation.id, "B", "abandoned edit")
        .await
        .unwrap()
        .run;
    f.store.request_cancel(cancelled.id).await.unwrap();
    let c = f
        .store
        .enqueue(conversation.id, "C", "continue")
        .await
        .unwrap()
        .run;
    let context = f.store.context_for(c.id).await.unwrap();
    let mut expected =
        vec![json!({"role": "user", "content": [{"type": "input_text", "text": "time please"}]})];
    expected.extend(items);
    expected.push(json!({"role": "user", "content": [{"type": "input_text", "text": "continue"}]}));
    assert_eq!(context, expected);
    assert!(
        !serde_json::to_string(&f.store.context_for(a.id).await.unwrap())
            .unwrap()
            .contains("continue")
    );
    let messages = f.store.list_messages(conversation.id, 100).await.unwrap();
    assert_eq!(messages.iter().filter(|m| m.role == "assistant").count(), 1);
    assert_eq!(
        messages
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .text,
        "It is noon."
    );
    assert!(
        !serde_json::to_string(&messages)
            .unwrap()
            .contains("encrypted_content")
    );
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn owner_review_releases_new_work_without_changing_or_replaying_unknown_effects() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let owner_id = owner.owner_id();
    let conversation = f.store.create_conversation("review outcome").await.unwrap();
    let run = f
        .store
        .enqueue(conversation.id, "original", "perform external work")
        .await
        .unwrap()
        .run;
    f.store.claim_next(owner_id).await.unwrap().unwrap();
    f.store
        .before_tool(
            run.id,
            owner_id,
            "known",
            "read_only",
            json!({"private":"synthetic-arguments"}),
        )
        .await
        .unwrap();
    f.store
        .after_tool(run.id, owner_id, "known", "synthetic-receipt")
        .await
        .unwrap();
    f.store
        .before_tool(
            run.id,
            owner_id,
            "unknown",
            "external_write",
            json!({"effect":"unknown"}),
        )
        .await
        .unwrap();
    let partial = vec![
        json!({"type":"function_call","call_id":"unknown","name":"external_write","arguments":"{}"}),
    ];
    let uncertain = f
        .store
        .finish(run.id, owner_id, completed(partial.clone(), "partial text"))
        .await
        .unwrap();
    assert_eq!(uncertain.status, RunStatus::Uncertain);
    assert!(matches!(
        f.store.enqueue(conversation.id, "next", "new work").await,
        Err(StoreError::Conflict(_))
    ));
    let tools_before = serde_json::to_value(f.store.list_tools(run.id).await.unwrap()).unwrap();
    let note = "外部サービスを確認しました。前の操作は繰り返さず、別の作業を続けます。";
    let reviewed = f
        .store
        .resolve_uncertain(run.id, uncertain.event_seq, note)
        .await
        .unwrap();
    assert_eq!(reviewed.status, RunStatus::Uncertain);
    assert_eq!(reviewed.error_code, uncertain.error_code);
    assert_eq!(reviewed.output_text, uncertain.output_text);
    assert_eq!(reviewed.output_items, json!(partial));
    assert!(reviewed.reviewed_at.is_some());
    assert_eq!(reviewed.review_note.as_deref(), Some(note));
    assert_eq!(reviewed.reviewed_event_seq, Some(uncertain.event_seq));
    assert_eq!(reviewed.event_seq, uncertain.event_seq + 1);
    assert_eq!(
        serde_json::to_value(f.store.list_tools(run.id).await.unwrap()).unwrap(),
        tools_before
    );
    assert_eq!(
        f.store
            .list_events(run.id, uncertain.event_seq, 100)
            .await
            .unwrap()[0]
            .kind,
        "run.reviewed"
    );
    let repeat = f
        .store
        .resolve_uncertain(run.id, uncertain.event_seq, note)
        .await
        .unwrap();
    assert_eq!(repeat.reviewed_at, reviewed.reviewed_at);
    assert_eq!(repeat.event_seq, reviewed.event_seq);
    let original_retry = f
        .store
        .enqueue(conversation.id, "original", "perform external work")
        .await
        .unwrap();
    assert!(!original_retry.created);
    assert_eq!(original_retry.run.id, run.id);
    assert_eq!(original_retry.run.status, RunStatus::Uncertain);
    let next = f
        .store
        .enqueue(conversation.id, "next", "new work")
        .await
        .unwrap()
        .run;
    let context = f.store.context_for(next.id).await.unwrap();
    assert_eq!(context.len(), 2);
    assert_eq!(context[0]["role"], "user");
    let history = context[0]["content"][0]["text"].as_str().unwrap();
    assert!(history.contains("perform external work"));
    assert!(history.contains(note));
    assert!(history.contains("do not repeat its external operations"));
    assert!(!history.contains("synthetic-receipt"));
    assert!(!history.contains("synthetic-arguments"));
    assert!(!history.contains("function_call"));
    assert_eq!(
        f.store.claim_next(owner_id).await.unwrap().unwrap().id,
        next.id
    );
    assert_eq!(f.store.list_tools(run.id).await.unwrap().len(), 2);
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn outcome_review_is_fenced_idempotent_and_serialized_with_enqueue() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let conversation = f.store.create_conversation("review races").await.unwrap();
    let run = f
        .store
        .enqueue(conversation.id, "original", "unknown work")
        .await
        .unwrap()
        .run;
    assert!(matches!(
        f.store
            .resolve_uncertain(run.id, run.event_seq, "review")
            .await,
        Err(StoreError::Conflict(_))
    ));
    f.store.claim_next(owner.owner_id()).await.unwrap().unwrap();
    assert!(matches!(
        f.store
            .resolve_uncertain(run.id, run.event_seq, "review")
            .await,
        Err(StoreError::Conflict(_))
    ));
    let uncertain = f
        .store
        .finish(
            run.id,
            owner.owner_id(),
            RunCompletion {
                status: RunStatus::Uncertain,
                output_items: vec![],
                output_text: String::new(),
                error_code: Some("INTERRUPTED".into()),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .resolve_uncertain(run.id, uncertain.event_seq - 1, "review")
            .await,
        Err(StoreError::Conflict(_))
    ));
    for note in ["", "  ", "\0", &"x".repeat(4097)] {
        assert!(matches!(
            f.store
                .resolve_uncertain(run.id, uncertain.event_seq, note)
                .await,
            Err(StoreError::InvalidInput(_))
        ));
    }
    let (a, b, racing_enqueue) = tokio::join!(
        f.store
            .resolve_uncertain(run.id, uncertain.event_seq, "review A"),
        f.store
            .resolve_uncertain(run.id, uncertain.event_seq, "review A"),
        f.store.enqueue(conversation.id, "next", "new work"),
    );
    assert_eq!(a.unwrap().event_seq, b.unwrap().event_seq);
    if let Err(error) = racing_enqueue {
        assert!(matches!(error, StoreError::Conflict(_)));
    }
    let next = f
        .store
        .enqueue(conversation.id, "next", "new work")
        .await
        .unwrap()
        .run;
    assert_ne!(next.id, run.id);
    assert_eq!(
        f.store.list_runs(conversation.id, 100).await.unwrap().len(),
        2
    );
    assert!(matches!(
        f.store
            .resolve_uncertain(run.id, uncertain.event_seq, "review B")
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        f.store
            .resolve_uncertain(run.id, uncertain.event_seq + 1, "review A")
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(
        f.store
            .get_run(run.id)
            .await
            .unwrap()
            .review_note
            .as_deref(),
        Some("review A")
    );
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn reviewed_history_counts_escaped_notes_and_does_not_load_partial_tool_items() {
    let f = Fixture::new().await;
    let conversation = f
        .store
        .create_conversation("reviewed context")
        .await
        .unwrap();
    // The huge partial output is deliberately excluded. Only completed output
    // and the owner's bounded review note can enter the next provider context.
    sqlx::query("INSERT INTO runs (id, conversation_id, idempotency_key, input_text, status, output_items, reviewed_at, review_note, reviewed_event_seq) VALUES ($1, $2, 'partial', 'historical request', 'uncertain', $3, clock_timestamp(), 'reviewed without retry', 1)")
        .bind(Uuid::new_v4()).bind(conversation.id).bind(json!([{"type":"reasoning","text":"z".repeat(2_097_152)}])).execute(&f.pool).await.unwrap();
    let target = f
        .store
        .enqueue(conversation.id, "next", "continue")
        .await
        .unwrap()
        .run;
    let context = f.store.context_for(target.id).await.unwrap();
    assert!(serde_json::to_vec(&context).unwrap().len() < 1024);
    f.store.request_cancel(target.id).await.unwrap();
    // Quoted notes and inputs fit their own limits, but exceed the provider's
    // serialized 2 MiB budget together. Neither may bypass preflight accounting.
    sqlx::query("INSERT INTO runs (id, conversation_id, idempotency_key, input_text, status, reviewed_at, review_note, reviewed_event_seq) SELECT gen_random_uuid(), $1, 'reviewed-' || n, $2, 'uncertain', clock_timestamp(), $3, 1 FROM generate_series(1, 40) n")
        .bind(conversation.id).bind("\"".repeat(24*1024)).bind("\"".repeat(4096)).execute(&f.pool).await.unwrap();
    let target = f
        .store
        .enqueue(conversation.id, "last", "continue")
        .await
        .unwrap()
        .run;
    assert!(matches!(
        f.store.context_for(target.id).await,
        Err(StoreError::InvalidInput(
            "conversation context requires compaction"
        ))
    ));
    f.cleanup().await;
}

#[tokio::test]
async fn event_sequences_are_monotonic_under_concurrent_writers_and_replay() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let id = owner.owner_id();
    let conversation = f
        .store
        .create_conversation("concurrent progress")
        .await
        .unwrap();
    let run = f
        .store
        .enqueue(conversation.id, "A", "work")
        .await
        .unwrap()
        .run;
    f.store.claim_next(id).await.unwrap().unwrap();
    let mut tasks = Vec::new();
    for index in 0..24 {
        let store = f.store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .append_event(run.id, id, "text.delta", json!({"text": index.to_string()}))
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let events = f.store.list_events(run.id, 0, 100).await.unwrap();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        (1..=26).collect::<Vec<_>>()
    );
    let replay = f.store.list_events(run.id, 10, 100).await.unwrap();
    assert_eq!(replay.first().unwrap().seq, 11);
    assert_eq!(replay.last().unwrap().seq, 26);
    assert!(matches!(
        f.store.list_events(run.id, 27, 100).await,
        Err(StoreError::InvalidInput(_))
    ));
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn duplicate_tool_intents_never_authorize_another_execution() {
    let f = Fixture::new().await;
    let owner = f.store.acquire_owner().await.unwrap();
    let id = owner.owner_id();
    let conversation = f.store.create_conversation("tool journal").await.unwrap();
    let run = f
        .store
        .enqueue(conversation.id, "A", "work")
        .await
        .unwrap()
        .run;
    f.store.claim_next(id).await.unwrap().unwrap();
    f.store
        .before_tool(
            run.id,
            id,
            "call_1",
            "write_something",
            json!({"value": "A"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .before_tool(
                run.id,
                id,
                "call_1",
                "write_something",
                json!({"value": "B"})
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    f.store
        .after_tool(run.id, id, "call_1", "receipt")
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .before_tool(
                run.id,
                id,
                "call_1",
                "write_something",
                json!({"value": "A"})
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    f.store
        .before_tool(run.id, id, "call_2", "external_operation", json!({}))
        .await
        .unwrap();
    let finished = f
        .store
        .finish(
            run.id,
            id,
            completed(vec![], "claimed success without receipt"),
        )
        .await
        .unwrap();
    assert_eq!(finished.status, RunStatus::Uncertain);
    assert_eq!(finished.error_code.as_deref(), Some("TOOL_OUTCOME_UNKNOWN"));
    owner.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn singleton_owner_prevents_live_recovery_and_invalidates_old_writes() {
    let f = Fixture::new().await;
    let first = f.store.acquire_owner().await.unwrap();
    let id = first.owner_id();
    let second_store = Store::from_pool(f.pool.clone()).await.unwrap();
    assert!(matches!(
        second_store.acquire_owner().await,
        Err(StoreError::Conflict(_))
    ));
    first.release().await.unwrap();
    assert!(matches!(
        f.store.claim_next(id).await,
        Err(StoreError::OwnershipLost)
    ));
    let mut second = second_store.acquire_owner().await.unwrap();
    assert_eq!(
        f.store.recover_interrupted(&mut second).await,
        Err(StoreError::OwnershipLost)
    );
    second_store.recover_interrupted(&mut second).await.unwrap();
    second.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn large_context_requires_compaction_instead_of_silent_history_loss() {
    let f = Fixture::new().await;
    let conversation = f
        .store
        .create_conversation("long conversation")
        .await
        .unwrap();
    sqlx::query("INSERT INTO runs (id, conversation_id, idempotency_key, input_text, status, output_items) SELECT gen_random_uuid(), $1, 'historical-' || n, 'synthetic history', 'completed', '[]'::jsonb FROM generate_series(1, 257) n")
        .bind(conversation.id).execute(&f.pool).await.unwrap();
    let target = f
        .store
        .enqueue(conversation.id, "next", "please continue")
        .await
        .unwrap()
        .run;
    assert_eq!(
        f.store.context_for(target.id).await,
        Err(StoreError::InvalidInput(
            "conversation context requires compaction"
        ))
    );

    let large = f
        .store
        .create_conversation("large context bytes")
        .await
        .unwrap();
    let output = json!([{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "x".repeat(2_097_152)}]}]);
    sqlx::query("INSERT INTO runs (id, conversation_id, idempotency_key, input_text, status, output_items) VALUES ($1, $2, 'historical', 'synthetic history', 'completed', $3)")
        .bind(Uuid::new_v4()).bind(large.id).bind(output).execute(&f.pool).await.unwrap();
    let target = f
        .store
        .enqueue(large.id, "next", "please continue")
        .await
        .unwrap()
        .run;
    assert_eq!(
        f.store.context_for(target.id).await,
        Err(StoreError::InvalidInput(
            "conversation context requires compaction"
        ))
    );
    f.cleanup().await;
}

#[tokio::test]
async fn context_byte_limit_counts_json_escaping() {
    let f = Fixture::new().await;
    let conversation = f
        .store
        .create_conversation("escaped context bytes")
        .await
        .unwrap();
    // Each input fits the HTTP limit, but quote escaping doubles its wire size.
    sqlx::query("INSERT INTO runs (id, conversation_id, idempotency_key, input_text, status, output_items) SELECT gen_random_uuid(), $1, 'escaped-' || n, $2, 'completed', '[]'::jsonb FROM generate_series(1, 50) n")
        .bind(conversation.id).bind("\"".repeat(24*1024)).execute(&f.pool).await.unwrap();
    let target = f
        .store
        .enqueue(conversation.id, "next", "please continue")
        .await
        .unwrap()
        .run;
    assert!(matches!(
        f.store.context_for(target.id).await,
        Err(StoreError::InvalidInput(
            "conversation context requires compaction"
        ))
    ));
    f.cleanup().await;
}

#[tokio::test]
async fn lost_lock_session_cannot_claim_new_work_before_the_monitor_notices() {
    let f = Fixture::new().await;
    let mut old_guard = f.store.acquire_owner().await.unwrap();
    let old_id = old_guard.owner_id();
    let a = f
        .store
        .create_conversation("interrupted on connection loss")
        .await
        .unwrap();
    let interrupted = f.store.enqueue(a.id, "A", "old work").await.unwrap().run;
    f.store.claim_next(old_id).await.unwrap().unwrap();

    // Only this test's generated-schema application session with an advisory
    // lock is terminated. This never touches another fixture or any system DB.
    let terminated: Vec<bool> = sqlx::query_scalar("SELECT pg_terminate_backend(a.pid) FROM pg_stat_activity a WHERE a.application_name = $1 AND EXISTS (SELECT 1 FROM pg_locks l WHERE l.pid = a.pid AND l.locktype = 'advisory')")
        .bind(&f.schema).fetch_all(&f.admin).await.unwrap();
    assert_eq!(terminated, vec![true]);

    let replacement = Store::from_pool(f.pool.clone()).await.unwrap();
    let mut guard = replacement.acquire_owner().await.unwrap();
    replacement.recover_interrupted(&mut guard).await.unwrap();
    assert_eq!(
        replacement.get_run(interrupted.id).await.unwrap().status,
        RunStatus::Uncertain
    );
    let b = replacement.create_conversation("new work").await.unwrap();
    let queued = replacement
        .enqueue(b.id, "B", "new work")
        .await
        .unwrap()
        .run;
    // The old in-memory owner flag is still set: the monitor has not checked
    // it yet. A pool-backed write here would incorrectly claim the new work.
    assert!(f.store.claim_next(old_id).await.is_err());
    assert_eq!(
        replacement.get_run(queued.id).await.unwrap().status,
        RunStatus::Queued
    );
    assert_eq!(old_guard.check().await, Err(StoreError::OwnershipLost));
    assert_eq!(
        replacement
            .claim_next(guard.owner_id())
            .await
            .unwrap()
            .unwrap()
            .id,
        queued.id
    );
    guard.release().await.unwrap();
    f.cleanup().await;
}

#[tokio::test]
async fn browser_sessions_expire_revoke_and_follow_the_server_token() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let token = [7; 32];
    let secret = [1; 32];
    let created = store
        .create_browser_session(&secret, &token, 3_600)
        .await
        .unwrap();
    let found = store
        .authenticate_browser_session(&secret, &token, 3_600)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.expires_at, created.expires_at);
    assert!(
        store
            .authenticate_browser_session(&secret, &[8; 32], 3_600)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .authenticate_browser_session(&[2; 32], &token, 3_600)
            .await
            .unwrap()
            .is_none()
    );

    // Use extends an idle expiry at most once per hour.
    sqlx::query("UPDATE browser_sessions SET last_used_at = last_used_at - interval '2 hours'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    let refreshed = store
        .authenticate_browser_session(&secret, &token, 7_200)
        .await
        .unwrap()
        .unwrap();
    assert!(refreshed.expires_at > created.expires_at);
    let unchanged = store
        .authenticate_browser_session(&secret, &token, 86_400)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.expires_at, refreshed.expires_at);

    store.revoke_browser_session(&secret).await.unwrap();
    assert!(
        store
            .authenticate_browser_session(&secret, &token, 3_600)
            .await
            .unwrap()
            .is_none()
    );

    let expired = [3; 32];
    store
        .create_browser_session(&expired, &token, -60)
        .await
        .unwrap();
    assert!(
        store
            .authenticate_browser_session(&expired, &token, 3_600)
            .await
            .unwrap()
            .is_none()
    );
    fixture.cleanup().await;
}
