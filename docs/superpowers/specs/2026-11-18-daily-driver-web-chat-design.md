# Daily-driver browser chat

The embedded browser client works, but its owner cannot use it as their main
chat yet. Four gaps were confirmed with the owner on 2026-11-18, and they are
delivered as four separate pull requests in this order:

1. Persistent sign-in (this document's detailed design).
2. Readable replies and visible progress.
3. Conversation organisation.
4. iPhone use.

Each change is test-first, reviewed, merged and then installed on the owner's
machine before the next one starts. The visual direction follows the owner's
reference app (Muse): a calm light surface, a status pill under the title, a
typing indicator before the first token, and conversations grouped by date.

## 1. Persistent sign-in

### Problem

The server token is kept only in page memory. Every reload, new tab or browser
restart shows the connection dialog, and the owner must fetch the token from a
terminal again. This is the largest daily friction, and it also blocks a
home-screen web app on iPhone (track 4).

### Decision

After the owner enters the server token once, the server issues a
database-backed browser session in an `HttpOnly` cookie. The token itself is
never stored by the browser. Bearer authentication for the CLI, tests and other
API clients is unchanged.

Alternatives considered:

- `localStorage` token: simplest, but any script injection would read the
  token, and track 2 adds a Markdown renderer. Rejected.
- Signed stateless cookie: no table, but a lost cookie cannot be revoked until
  the server token is rotated. Rejected.
- Device pairing with a QR code: better for multiple devices, but much larger
  than the problem. Deferred.

### HTTP contract

| Route | Auth | Behaviour |
| --- | --- | --- |
| `POST /v1/sessions` | none; body `{"token": "<server token>"}` | Requires an allowed `Origin`. Compares the token in constant time. On success returns `200 {"expires_at": ...}` and sets the session cookie. Wrong token: `401`, no cookie. Missing or foreign `Origin`: `403`. |
| `GET /v1/session` | cookie or bearer | `200 {"method": "browser" \| "bearer", "expires_at": ... \| null}`. Used by the client on page load. |
| `DELETE /v1/session` | cookie | Revokes the current browser session and expires the cookie. `204`. |

Cookie: `by_human_session=<43 base64url characters>; Path=/; HttpOnly;
SameSite=Strict; Max-Age=34560000`. The value is 32 random bytes. The cookie is only a
handle kept for the 400 days browsers allow; the server ends the session after
30 days without use (below). `Secure` is
added when the allowed origin is `https` (track 4); loopback `http` origins
cannot carry `Secure` in every browser.

### Authentication rules

- A request with an `Authorization` header is authenticated only as bearer. An
  invalid bearer never falls back to a cookie.
- Otherwise exactly one `by_human_session` cookie must be present and decode to
  32 bytes. Duplicate or malformed cookies are `401`.
- Cookie-authenticated requests other than `GET` and `HEAD` require an `Origin`
  header that exactly matches an allowed origin, else `403`. `SameSite=Strict`
  already blocks cross-site requests; the origin check also covers other origins
  on the same site (for example other machines under one tailnet domain).
- Allowed origins are derived from the listening address. For a loopback
  listener on port `P` they are `http://127.0.0.1:P` and `http://localhost:P`
  (and `http://[::1]:P` for an IPv6 listener). Track 4 adds explicit `https`
  origins through configuration. No allowed origin means browser sessions are
  unavailable.
- JSON bodies are already required, so a cross-site HTML form cannot submit.

### Storage

New table `browser_sessions(id, secret_hash, server_token_hash, created_at,
last_used_at, expires_at, revoked_at)`. Only SHA-256 hashes are stored.

- Sessions expire 30 days after the last recorded use. Use is recorded at most
  once per hour to avoid a write per request, so a session can end up to an hour
  before 30 days have passed since its very last request. The cookie itself
  lasts at most 400 days, so a sign-in is renewed at least that often.
- `server_token_hash` is the SHA-256 of the server token at issue time. Rotating
  `BY_HUMAN_API_TOKEN` therefore signs out every browser. At start-up the server
  revokes every session issued under another token, so restoring an earlier
  token does not revive them.
- Sign-out sets `revoked_at`. Expired and revoked rows older than seven days are
  pruned when a new session is created.

### Client behaviour

- All requests use same-origin credentials and no longer send the token.
- On load the client calls `GET /v1/session`. Signed in: open the conversation
  list directly. `401`: show the existing sign-in dialog.
- The dialog explains that sign-in lasts 30 days in this browser. After a
  successful `POST /v1/sessions` the token is cleared from the field and memory.
- The sidebar's 切断 becomes ログアウト (`DELETE /v1/session`).
- Any `401` returns to the dialog with a message that the sign-in expired or
  ended. A `403` explains that the page must be opened at the server's own URL.
- Transport or server failures, including the first list load right after a
  successful sign-in, keep the session and offer 再試行. 新しい会話 stays
  disabled until the conversation list has loaded.
- Drafts and the unacknowledged-send outbox keep their current tab storage.

### Tests

- Rust HTTP contracts: login success and cookie attributes, wrong token, missing
  and foreign `Origin`, cookie-authenticated reads, unsafe methods with and
  without an allowed `Origin`, bearer without `Origin`, sign-out, expiry, token
  rotation, duplicate and malformed cookies.
- Browser contracts: sign-in survives reload, sign-out returns to the dialog and
  survives reload, the cookie is `HttpOnly`/`SameSite=Strict` and absent from
  `document.cookie`, the token is never in storage or the URL, and an invalid
  cookie returns to the dialog. Existing flows keep passing.

### Out of scope

Listing or revoking other devices, QR pairing, notifying other open tabs of a
sign-out (they return to the dialog on their next request), rate limiting, and
remote `https` access (track 4). Skipping rate limiting relies on the server
token being a long secret from a secure random source: the server checks only
that it has 32 to 512 printable characters.

## 2. Readable replies and visible progress

- A dependency-free Markdown renderer (`/markdown.js`) that builds DOM nodes and
  never parses HTML: headings, lists, task items, quotes, tables, links limited
  to `http`, `https` and `mailto`, inline code and fenced code with a copy
  button. Single newlines stay visible line breaks.
- Only the streaming reply is re-rendered, at most once per animation frame.
- A status pill under the title: 考えています → <tool> を実行中 (from the stored
  `tool.started` event and tool name) → 応答中. A typing indicator appears
  before the first token.
- Copy for each reply, time separators between turns, a jump-to-latest button,
  and a dark theme that follows the system setting.

## 3. Conversation organisation

- Rename and delete from a per-conversation menu, with confirmation for delete.
- Search titles and message text.
- Order by last activity and group by 今日, 昨日, 過去 7 日間, then month.
- The required API and migration are designed in that change.

## 4. iPhone use

- A web app manifest, icons and standalone display so it can be added to the
  home screen.
- Touch composer rules: Return inserts a newline and the button sends; the
  composer follows the on-screen keyboard and safe areas.
- `https` access through Tailscale Serve, with explicit allowed origins and
  `Secure` cookies. Publishing the service on the tailnet is confirmed with the
  owner before it is enabled.
