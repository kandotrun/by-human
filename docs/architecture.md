# Independent Rust agent

by-human is being built as an independent, self-hosted personal agent that can
replace the OpenClaw functions its owner uses. The initial server is Rust,
OpenAI-only, and PostgreSQL-backed. Pi is a design reference, not a runtime
dependency. Existing Python and Expo experiments remain separate.

## First delivery

Small Graphite changes build up the first runnable slice:

1. PostgreSQL conversations, durable runs, events and tool receipts.
2. The OpenAI Responses stream and a bounded model/tool loop.
3. An authenticated local HTTP API with stop, event replay and restart recovery.
4. The application's own ChatGPT-plan OAuth login and serialized token refresh.

The first tool only reads the current time. Shell execution, browser automation,
MCP, Every, scheduling, push notifications and the iOS adapter are later changes.
No claim of feature parity or performance improvement is made by this first slice.

## State ownership

PostgreSQL owns conversation input, run state, canonical model items, display
events and tool execution receipts. A model's reasoning/tool records are never
automatically presented as chat text. Requests reuse an idempotency key; a key
cannot silently acquire another payload. New work in a conversation waits for
its unresolved run to be dealt with.

A single server owns the initial execution loop. It holds a PostgreSQL ownership
lock while recovering and dispatching runs. A previously running job becomes
`uncertain` after a crash and is not automatically replayed. Queued work may
resume. An external operation's cancellation request is distinct from proof that
it stopped; future tools must preserve this distinction.

Tool intent is durably recorded before execution, and a receipt follows it.
There is no universal exactly-once guarantee for arbitrary external effects.
Unknown outcomes require reconciliation rather than blind retry. This slice
blocks that conversation until reconciliation is implemented; a new conversation
can still be used. Canonical context is bounded to 256 completed turns or 2 MiB;
exceeding the bound fails explicitly instead of silently dropping history.

## OpenAI boundary

The server talks directly to the public Responses API. API-key and ChatGPT-plan
wire requirements are kept in the provider. ChatGPT-plan login is the selected
default; by-human registers its own OAuth session and stores rotating credentials
locally. It does not borrow another application's credentials. API-key billing
will require an explicit configuration choice.

The Responses stream must end with a successful completed response. Truncated,
failed or interrupted arguments are never executed as tools. Canonical response
items, call IDs and tool outputs are preserved for subsequent model turns.

Sources: [Responses tool flow](https://developers.openai.com/api/docs/guides/function-calling),
[ChatGPT plan requirements](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations),
[Pi Agent Core](https://github.com/earendil-works/pi/blob/main/packages/agent/README.md).

## Deployment boundary

Development and contract tests use an isolated PostgreSQL instance and synthetic
model streams. Public deployment, existing OpenClaw data migration, private Every
access and production cutover are separate actions. Runtime secrets are supplied
through the environment and never committed or written to the execution log.
