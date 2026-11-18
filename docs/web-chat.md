# Browser chat

The Rust server embeds a small same-origin browser client at `/`. There is no
separate frontend server or production Node.js dependency. Start the server as
described in the README, then open `http://127.0.0.1:4310/` on the same computer.

Sign in once with the server's `BY_HUMAN_API_TOKEN`. This is separate from
ChatGPT login: the browser never receives the OpenAI credentials. The server
exchanges the token for a browser session and the page forgets the token
immediately; it is never placed in a URL, localStorage or sessionStorage. The
session lives in an `HttpOnly`, `SameSite=Strict` cookie that page scripts cannot
read, so reloads, new tabs and browser restarts stay signed in. A session ends
about 30 days after its last use (use is recorded at most hourly), when you
choose **ログアウト**, or for every browser when the server token is rotated, and
a sign-in lasts at most 400 days. If loading fails while the session is valid,
the page keeps it and offers 再試行 instead of asking for the token. Requests stay on the page's origin, and the
server accepts browser changes only from its own loopback address. Use HTTPS for
any non-loopback access; this change does not configure a public endpoint, TLS,
CORS, device pairing or an iPhone adapter. See [browser sessions](http-api.md#browser-sessions).

The client supports new/existing conversations, Japanese multiline input,
streamed replies, Stop, event-stream reconnection and per-conversation drafts.
Choose **新しい会話** to start writing immediately. The server saves a short
title from the first accepted message and displays it in the conversation list.
Tool activity appears as progress; tool arguments, results and reasoning do not
appear as chat messages. Replies render a safe Markdown subset: headings, lists
and task items, quotes, tables, inline and fenced code, and links limited to
`http`, `https` and `mailto`. The renderer builds DOM nodes directly and never
parses HTML, so HTML in a reply stays text, images appear as links and other
link schemes as plain text. Each code block and reply has a copy button. Your
own messages, and replies longer than 65,536 characters, stay plain text.

While a reply runs, a status pill under the title shows 考えています, the tool in
use (時刻を確認中, or `<server> のツールを実行中` for MCP tools) or 応答中, and
typing dots fill the gap before the first token. A time label opens each
conversation and marks gaps of 30 minutes or a new day. When you scroll up, a
button returns to the latest reply. The layout follows the system light or dark
theme.
The browser loads the latest 100 runs that fit in an 8 MiB response. It marks
omitted history at the top of the conversation. Older runs remain stored on the
server. Paging remains outside this slice.

Drafts and unacknowledged submissions are kept in this tab's sessionStorage.
These contain conversation text. They are scoped to this server origin and tab,
not synchronized between browsers, and are cleared when the tab session ends.
Do not use a shared browser profile for private conversations. The server is
still the authority for accepted runs and completed messages.

Before sending, the client reserves the submitted text and a random idempotency
key in tab storage. The composer clears immediately and accepts the next draft.
If the acknowledgment is lost, **送信結果を確認** checks acceptance by sending the
same text and key. After a reload, an exact match in history confirms acceptance
without another submission. Switching conversations, losing the event stream or
closing the page does not cancel execution. Use **停止** to request cancellation.

## Reviewing an uncertain run

Interrupted external work can already have happened even when no receipt is
available. The client therefore blocks further sends in that conversation and
offers an explicit review panel. Inspect the recorded tools and, where needed,
check the external service yourself. An unknown outcome is not proof of failure.

To continue, enter a note and explicitly acknowledge the remaining uncertainty.
This records the note and a review event, releases the conversation's reservation
and allows a **new** message. It never retries the old run or fabricates a tool
receipt. The old run remains `uncertain`, including its original error and tool
records. The next model turn receives the reviewed request and owner note as
user context, with a warning against repeating the old external work; partial
provider/tool-call sequences are excluded.

Review is available through the authenticated owner API. It is an acknowledgment,
not an independent verification of external effects or a per-tool approval system.

## 日本語

`serve`で起動したサーバーのURLをブラウザで開くと、チャット画面を使えます。
ログインに使うのは`BY_HUMAN_API_TOKEN`です。ChatGPTのパスワードやトークンを入力する
必要はありません。一度ログインすると、再読み込みやブラウザの再起動後もログインが続きます。
ログイン状態はスクリプトから読めないCookieで保持し、トークン自体はブラウザに保存しません。
最後に使ってから30日たつか、**ログアウト**するか、サーバー用トークンを変更すると終了します。

**新しい会話**を開くと、名前の入力なしですぐにメッセージを書けます。
最初のメッセージが受け付けられると、その内容をもとにタイトルが付きます。

返信は見出し・リスト・表・コードなどのMarkdownで表示し、コードと返信本文はコピーできます。
返信内のHTMLは実行せずに文字として表示し、画像は読み込まずリンクとして表示します。
65,536文字を超える長い返信は、Markdownにせずそのまま文字で表示します。
応答中はタイトルの下に「考えています」「時刻を確認中」「応答中」などの状態を表示し、
画面はシステムのライト／ダーク設定に合わせます。

下書きと送信結果の確認待ちは、このタブのsessionStorageに保存します。
送信結果が不明なときは、同じ送信IDで確認し、新しいIDで送り直しません。
復旧パネルで実行記録を確認し、メモと確認操作を残すと、同じ会話を続けられます。
古い実行の結果は不明のまま保存し、自動で再実行しません。
