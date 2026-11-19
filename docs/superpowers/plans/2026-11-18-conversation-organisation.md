# Conversation Organisation (3a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The owner can rename and permanently delete conversations, and the sidebar lists them by last activity under date headings.

**Architecture:** A new `last_activity_at` column, raised by `enqueue` in its existing locked transaction, orders the list. `PATCH` and `DELETE /v1/conversations/{id}` reuse the conversation row lock that `enqueue` takes, so a manual name always beats the automatic title and a delete never races new work. The browser groups rows by local date and adds a ⋯ menu with rename and delete dialogs.

**Tech Stack:** Rust (axum 0.8, sqlx 0.9/PostgreSQL 17), vanilla ES modules, Playwright.

Design: [daily-driver browser chat, track 3](../specs/2026-11-18-daily-driver-web-chat-design.md#3-conversation-organisation). Search is the next pull request (3b).

---

## File structure

| File | Responsibility |
| --- | --- |
| `server/migrations/202611180004_conversation_activity.sql` (new) | `last_activity_at`, backfill and index. |
| `server/src/store.rs` | `Conversation.last_activity_at`, list order, `rename_conversation`, `delete_conversation`, shared blocking-work predicate, activity update in `enqueue`. |
| `server/src/api.rs` | `PATCH`/`DELETE /v1/conversations/{id}`; snapshot reserve for the timestamp. |
| `server/tests/postgres_store.rs`, `server/tests/http_runtime.rs` | Store and HTTP contracts. |
| `server/web/index.html`, `server/web/chat.js`, `server/web/chat.css` | Date groups, ⋯ menu, rename and delete dialogs, 404 handling, reorder on send. |
| `web-tests/tests/chat.spec.js` | Browser contracts. |
| `docs/http-api.md`, `docs/web-chat.md` | Behaviour and limits. |

Commands (with an isolated PostgreSQL 17 in `DATABASE_URL`):

```sh
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
(cd web-tests && npm test)
```

## Task 1: Order by last activity

**Files:** Create the migration; modify `server/src/store.rs`; test in `server/tests/postgres_store.rs`.

- [ ] **Step 1: Failing store test** `conversations_are_listed_by_last_activity`: create A, B, C (in that order); the list is C, B, A. Enqueue a message in A: A's `last_activity_at` equals the run's `created_at` and the list is A, C, B. An exact retry of that message, a cancel and a rename leave the order and A's timestamp unchanged.
- [ ] **Step 2: Run it** and see it fail to compile (`last_activity_at` missing).
- [ ] **Step 3: Migration**

```sql
ALTER TABLE conversations ADD COLUMN last_activity_at TIMESTAMPTZ;
UPDATE conversations c SET last_activity_at = GREATEST(
    c.created_at,
    (SELECT max(r.created_at) FROM runs r WHERE r.conversation_id = c.id));
ALTER TABLE conversations
    ALTER COLUMN last_activity_at SET DEFAULT clock_timestamp(),
    ALTER COLUMN last_activity_at SET NOT NULL;
CREATE INDEX conversations_by_activity ON conversations (last_activity_at DESC, id DESC);
```

- [ ] **Step 4: Store.** Add `pub last_activity_at: DateTime<Utc>` to `Conversation`. `list_conversations` orders by `last_activity_at DESC, id DESC`. In `enqueue`, after inserting a new run: `UPDATE conversations SET last_activity_at = GREATEST(last_activity_at, $2) WHERE id = $1` with the run's `created_at`. Snapshot: reserve the 7 bytes a timestamp's fraction can add between the two conversation reads (`.123456`).
- [ ] **Step 5: Run** the store tests; commit `feat: order conversations by last activity`.

## Task 2: Rename

**Files:** `server/src/store.rs`, `server/src/api.rs`; tests in both suites.

- [ ] **Step 1: Failing tests**
  - Store `rename_sets_a_manual_title_that_the_first_message_keeps`: an untitled conversation renamed to ` 旅の計画 ` is `旅の計画`; its first message keeps that title (`conversation_title` too). Renaming after an automatic title replaces it. Blank, 201 characters and an unknown id fail with `InvalidInput`/`NotFound`.
  - HTTP (in the browser-session test file section): `PATCH` returns `200` with the conversation, `400` for blank, `404` for an unknown id, `422` for `{"title":"x","extra":1}`; with a session cookie it needs an allowed `Origin` (`403` otherwise).
- [ ] **Step 2: Run** and see them fail (no method/route).
- [ ] **Step 3: Implement.** Extract the trim and 1–200 character check from `create_conversation` into `fn normalized_title(&str) -> Result<&str>`. `rename_conversation(id, title)` runs `UPDATE conversations SET title = $2, title_pending = FALSE WHERE id = $1 RETURNING *` (`NotFound` when no row). The handler takes `#[serde(deny_unknown_fields)] struct RenameConversation { title: String }`, checks `nonempty(&title, MAX_TITLE_BYTES)` and returns `Json(conversation)`.
- [ ] **Step 4: Run** both suites; commit `feat: rename conversations`.

## Task 3: Delete

**Files:** `server/src/store.rs`, `server/src/api.rs`; tests in both suites.

- [ ] **Step 1: Failing tests**
  - Store `delete_removes_only_that_conversation_and_its_records`: two conversations with completed runs, events and a tool execution; deleting one leaves zero rows for it in `conversations`, `runs`, `run_events`, `tool_executions` and every row of the other. Deleting again is `NotFound`.
  - Store `delete_is_refused_while_work_is_queued_running_or_unreviewed`: `Conflict` for a queued run, a running run (including one with `cancel_requested`) and an unreviewed uncertain run; succeeds after the uncertain run is reviewed.
  - Store `delete_rechecks_blocking_work_after_waiting_for_the_lock`: a raw transaction holds `SELECT ... FOR UPDATE` on the conversation; a spawned delete waits; the transaction inserts a queued run and commits; the delete returns `Conflict`.
  - HTTP: `DELETE` returns `204` with no body, then `404`; `409` while a run is active; a cookie request without an allowed `Origin` is `403`; the runner still completes another conversation's run afterwards.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.** Move the blocking predicate into `const BLOCKING_WORK: &str = "SELECT EXISTS (SELECT 1 FROM runs WHERE conversation_id = $1 AND (status IN ('queued', 'running') OR (status = 'uncertain' AND reviewed_at IS NULL)))";` and use it in `enqueue` and:

```rust
pub async fn delete_conversation(&self, id: Uuid) -> Result<()> {
    let mut tx = self.pool.begin().await?;
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM conversations WHERE id = $1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?.ok_or(StoreError::NotFound)?;
    if sqlx::query_scalar::<_, bool>(BLOCKING_WORK).bind(id).fetch_one(&mut *tx).await? {
        return Err(StoreError::Conflict("conversation has unfinished or uncertain work"));
    }
    for statement in [
        "DELETE FROM run_events WHERE run_id IN (SELECT id FROM runs WHERE conversation_id = $1)",
        "DELETE FROM tool_executions WHERE run_id IN (SELECT id FROM runs WHERE conversation_id = $1)",
        "DELETE FROM runs WHERE conversation_id = $1",
        "DELETE FROM conversations WHERE id = $1",
    ] {
        sqlx::query(statement).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
```

  The handler returns `StatusCode::NO_CONTENT`.
- [ ] **Step 4: Run** both suites; commit `feat: delete conversations permanently`.

## Task 4: Date groups and activity order in the browser

**Files:** `server/web/chat.js`, `server/web/chat.css`; tests in `web-tests/tests/chat.spec.js`.

- [ ] **Step 1: Failing browser tests**
  - `the sidebar groups conversations by local date`: `page.clock.setFixedTime('2026-11-18T12:00:00+09:00')`, `timezoneId: 'Asia/Tokyo'`; route `GET /v1/conversations` to return six conversations with `last_activity_at` at 11-18 00:00, 11-17 23:59:59, 11-11 00:00, 11-10 23:59:59, 10-31 and 2025-12-31 (+09:00). Headings read 今日, 昨日, 過去 7 日間, 11月, 10月, 2025年12月, each above its row.
  - `sending moves a conversation to the top and it stays there after reload`.
  - Fix `#conversation-list button` counts to `[data-conversation-id]`; the outside-the-window title test filters its conversation out of the routed list instead of relying on creation order.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.** `activityGroup(at, now)` compares with local midnights (`new Date(y, m, d - n)`); `renderSidebar` emits `h2.conversation-group` + `ul` per group and `li.conversation-item` rows, restoring focus to the same row control after rebuilding. A successful `dispatch` moves the row to the top with the run's `created_at`. `refreshConversations` applies only its newest response and is skipped after a local change (`listGeneration`); `visibilitychange` to visible reloads the list.
- [ ] **Step 4: Run**; commit `feat: group conversations by last activity`.

## Task 5: Menu and rename

**Files:** `server/web/index.html`, `server/web/chat.js`, `server/web/chat.css`; browser tests.

- [ ] **Step 1: Failing browser tests**
  - `a conversation can be renamed from its menu by keyboard`: focus the row's ⋯ (`「title」の操作`), Enter opens the menu, 名前を変更 opens the dialog with the current name selected; saving updates the row and header and survives reload; Escape closes the dialog and returns focus to ⋯ without closing anything else.
  - `renaming before the first message keeps the name`: rename 新しい会話 to 手動の名前, send a message, the title stays.
  - `an unchanged name sends no request` and `an empty name shows an error`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.** `#conversation-menu[popover]` holds two buttons. Each row's `button.conversation-more` has `popovertarget`, `aria-label` and `data-menu-for`; a click records the row before the toggle and the `toggle` event positions the menu below the button (CSSOM `top`/`left`, clamped to the viewport), focuses the first item and, on close, returns focus to the row's ⋯. `#rename-dialog` submits `PATCH` (disabled while renaming); `404` forgets the conversation; `400` shows 名前を1〜200文字で入力してください。. The document Escape handler ignores events while a dialog or the menu is open.
- [ ] **Step 4: Run**; commit `feat: rename conversations from the sidebar`.

## Task 6: Delete and missing conversations

**Files:** as Task 5; browser tests.

- [ ] **Step 1: Failing browser tests**
  - `deleting a conversation asks first and removes it everywhere in the tab`: キャンセル keeps it; 削除 removes the row, returns the open conversation to the empty state, clears its sessionStorage draft and outbox, makes the API return `404`, and focuses the next row.
  - `a busy conversation cannot be deleted`: for the open conversation running 停止テスト, 削除 is disabled with the busy copy; for another open one, the server's `409` shows the same copy and the row stays.
  - `a conversation deleted elsewhere disappears when opened`: `page.request.delete` then selecting it removes the row and shows この会話は削除されています。.
  - `sending to a conversation deleted elsewhere offers the text back`: the `404` marks the message not accepted and 入力欄に戻す restores it.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.** `forgetConversation(id)` removes the row and its draft, outbox and sending entries, saves the tab state and, when open, resets the selection to the empty state. `#delete-dialog` (キャンセル autofocused, 削除 as `.danger-button`) calls `DELETE`; `204`/`404` forget; `409` shows 実行中の処理か、確認が必要な実行結果があるため削除できません。. A snapshot `404` forgets the conversation with the notice; an enqueue `404` counts as rejected.
- [ ] **Step 4: Run**; commit `feat: delete conversations from the sidebar`.

## Task 7: Docs

- [ ] `docs/http-api.md`: the two routes, list order and `last_activity_at`, delete semantics and refusal. `docs/web-chat.md` (+ 日本語): grouping, rename, delete is permanent, what a tab keeps. Commit `docs: describe conversation rename, delete and activity order`.
