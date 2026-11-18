# Local HTTP API

The default listener is `127.0.0.1:4310`. All `/v1` routes require
`Authorization: Bearer <BY_HUMAN_API_TOKEN>`. The server token is independent of
the OpenAI login; it never travels to OpenAI. Responses use `Cache-Control:
no-store`. The same server serves an embedded [browser client](web-chat.md) at
`/`, with local CSS/JavaScript assets. This slice has no browser CORS policy,
public TLS endpoint or device pairing UI.

| Method | Route | Result |
| --- | --- | --- |
| GET | `/health` | Minimal PostgreSQL readiness; no authentication required |
| POST | `/v1/conversations` | Create with `{"title":"..."}` |
| GET | `/v1/conversations` | Latest 100 conversations |
| GET | `/v1/conversations/{id}` | Conversation, latest chat messages and public run states |
| GET | `/v1/conversations/{id}/snapshot` | Browser view with a bounded window of public runs |
| POST | `/v1/conversations/{id}/runs` | Submit `{"message":"...","idempotency_key":"..."}` |
| GET | `/v1/runs/{id}` | Public run state and settled text |
| POST | `/v1/runs/{id}/cancel` | Request Stop; repeated requests are safe |
| GET | `/v1/runs/{id}/review` | Explicit owner inspection of run and tool records |
| POST | `/v1/runs/{id}/resolve` | Acknowledge uncertain work with a note; never replay |
| GET | `/v1/runs/{id}/events` | Durable server-sent events |

A new submission returns `202` with its run ID. An exact retry of the same key
and text returns the same run with `200`. Changing the text behind that key, or
starting distinct work while the conversation has queued/running/unreviewed uncertain work,
returns `409`. A missing resource returns `404`.

For SSE, preserve each integer `id` and reconnect to the **same run** with
`Last-Event-ID`. Events are replayed from PostgreSQL; closing the stream does not
cancel execution. Event names include `run.queued`, `run.running`, `model_started`,
`text_delta`, `tool.started`, `tool.completed`, `model.usage` and `run.finished`.
The `text_delta` payload contains `{"delta":"..."}`. Use the settled run state
after completion rather than treating partial output as a successful reply.
Canonical reasoning and tool arguments/results are not chat messages and are
omitted from public run/history responses.

Only the latest 100 runs are projected into conversation history in this slice;
pagination is not implemented. Message input is limited to 24 KiB at the API.
The browser snapshot includes only `conversation`, `runs` (newest first), and
`older_runs_omitted`. It keeps a contiguous window of at most 100 runs within
8 MiB of serialized JSON, including escaped text. Omitted history stays stored.
Canonical context exceeding 256 completed turns or 2 MiB fails explicitly with
`CONTEXT_LIMIT_REACHED`; compaction is not implemented yet.

## Stop and recovery

Stopping a queued run cancels it before dispatch. Stopping an active run requests
cancellation and checks it before the next model request or tool. A tool receipt
that already happened is preserved. A server crash or an outcome that cannot be
confirmed becomes `uncertain`, never a permission to replay an external action.
The conversation is blocked until the owner explicitly reviews that outcome.
Other conversations can still run.

`GET /v1/runs/{id}/review` returns `{run, tools}`. Unlike ordinary chat/history,
this explicit inspection endpoint includes recorded tool arguments and outputs.
It still excludes raw provider reasoning/items. Treat tool text as untrusted.

To allow new work after reviewing an uncertain run, submit
`{"note":"...","expected_event_seq":N,"acknowledge_unknown":true}` to `/resolve`.
The note is required (up to 4096 UTF-8 bytes); `N` is the inspected run's event
sequence. Only uncertain runs can be reviewed. A stale sequence or different
repeat returns `409`; an exact repeat returns the same review. Unknown fields
and missing/false acknowledgment are rejected. The transaction records the note,
`reviewed_at` and a `run.reviewed` event before releasing the conversation gate.
The run status remains `uncertain`, and its error, output, tool intents and actual
receipts are preserved. No tool executes and no successful receipt is invented.
Public run responses include `reviewed_at`, `review_note` and the owner's
`idempotency_key`, enabling exact client matching after a lost acknowledgment.
The next message sees the reviewed request and note as user context, with an
explicit non-replay warning, rather than incomplete canonical tool items.

One backend owns dispatch for a database/schema. Startup refuses a second owner.
Database-ownership loss stops dispatch; readiness loss terminates the server.
Queued work can resume after restart, while interrupted active work is marked
uncertain. This does not claim exactly-once execution of arbitrary external work.

## 日本語

`/v1`には、OpenAIの認証とは別のサーバー用Bearerトークンが必要です。
送信時に作った`idempotency_key`は再試行でも再利用してください。
同じキーで本文を変更すると`409`になります。
SSEの接続を閉じても実行は止まりません。停止には`/cancel`を使います。

再起動時、処理中だった実行は結果未確認の`uncertain`として残し、自動再実行しません。
実行記録を確認してメモと明示的な確認操作を残すと、同じ会話を続けられます。
古い実行の結果を成功扱いに変更したり、自動で再実行したりはしません。
履歴ページング、長い会話の圧縮は未実装です。
PostgreSQLと合成モデルを使う契約テストは、実際のOpenAI接続や利用者の実機確認とは区別します。
