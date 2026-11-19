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
5. A same-origin browser client and explicit owner review of uncertain work.

The built-in tool reads the current time. A shared tool registry also accepts
owner-selected MCP tools through stdio or Streamable HTTP; its same definitions
are advertised to OpenAI and used for validation and dispatch. Every/Zenkioku is
not part of the integration scope. Shell execution, browser automation,
scheduling, push notifications and the iOS adapter are later changes.
No claim of feature parity or performance improvement is made by this first slice.

OpenAI's hosted web search is offered beside those tools unless
`BY_HUMAN_WEB_SEARCH=off`; for the ChatGPT plan it is a sibling of the `by_human`
namespace, never inside it. The search runs at OpenAI within a model response,
so nothing executes locally: it records no tool intent or receipt and never
leaves an unknown tool outcome. Stop during a search ends the run as cancelled,
and a lost stream ends it as failed. As during any model request, a server
crash, the 120-second run deadline or a storage failure still leaves the
running run uncertain. Its `web_search_call` items stay in the canonical
output, except a malformed item without a usable ID, which is left out because
it is not valid Responses input. While search is on they are replayed in later
requests, as stateless Responses requests require. While it is off, requests
leave them out:
a request that replays a search without offering the tool can fail, which would
stop a conversation that searched before. The model cites sources as Markdown
links in its text, so `url_citation` annotations need no separate storage.
Progress uses the same `tool.started` and `tool.completed` events as local
tools, named `web_search`.

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
Unknown outcomes require owner review rather than blind retry. An authenticated
owner can inspect the tool records and explicitly acknowledge the uncertainty
with a note. This preserves the uncertain result and receipts, records a review
event and allows new work; it does not replay the old run. Reviewed requests and
notes enter later context as user input, without incomplete provider items.
The embedded [browser client](web-chat.md) exposes this operation separately from
ordinary messages. Canonical context is bounded to 256 completed/reviewed turns or 2 MiB;
exceeding the bound fails explicitly instead of silently dropping history.

MCP connection configuration is local and explicit. Only listed tools are
registered; neither discovery nor model output expands that authority. A remote
tool's annotation is not treated as permission. The MCP client does not borrow
OpenAI credentials, provide model sampling, or expose filesystem roots. See
[MCP tools](mcp.md) for configuration, transport and cancellation limits.

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
[web search](https://developers.openai.com/api/docs/guides/tools-web-search),
[stateless conversation state](https://developers.openai.com/api/docs/guides/conversation-state),
[Pi Agent Core](https://github.com/earendil-works/pi/blob/main/packages/agent/README.md).

## Deployment boundary

Development and contract tests use an isolated PostgreSQL instance, synthetic
model streams and dedicated MCP fixtures. Public deployment, existing OpenClaw
data migration and production cutover are separate actions. Runtime secrets are supplied
through the environment or the dedicated protected OAuth store, and never
committed or written to the execution log.
