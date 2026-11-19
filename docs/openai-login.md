# ChatGPT-plan login

by-human uses its own OpenAI registration. It does not read Codex, OpenClaw or
another application's login files. ChatGPT-plan access is the default; no
Platform API key is required. There is no automatic fallback to API-key billing.

Run `cargo run -p by-human -- login` on the same computer as your browser. Open
the displayed OpenAI authorization URL, sign in and review the consent. The
callback listens only on a dynamically selected `127.0.0.1` port at
`/auth/callback`; a browser on a different computer cannot complete this local
callback directly.

The app requests `openid profile email offline_access resource.invoke
chatgpt.tokens.use.direct` for the official `https://api.openai.com/v1` resource.
OpenAI may require an eligible account or additional consent. A saved login that
lacks direct-token permission cannot start inference.

If the saved sign-in has `plan_enabled=false`, run `login --enable-plan` to
explicitly request consent using the same issued client ID and complete scopes.
This sends OAuth `prompt=consent`; ordinary `login` does not force consent.

Credentials and the stable app host ID live in the dedicated
`~/.config/by-human` directory by default. `BY_HUMAN_AUTH_DIR` selects another
dedicated local directory. The directory must be owner-only (`0700`) and files
owner-only (`0600`); symlink targets are refused. Do not commit, print, attach or
share that directory. The server/client bearer token is separate from this
OpenAI login.

OAuth uses PKCE, state and nonce. The ID token is checked against OpenAI's JWKS,
issuer, issued client ID, expiry and expected identity. Returning login reuses
the issued client ID. Access tokens refresh close to expiry; refreshes are
serialized and replacement credentials are saved atomically. Requests use the
official HTTPS endpoints with redirects and automatic replay disabled.

A successful refresh is saved as pending before identity validation makes any
further network requests. If discovery or JWKS is temporarily unavailable, a
restart retries validation from that record without reusing the old refresh
token. Pending credentials cannot be used for inference, and delayed validation
does not extend the access-token expiry.
For a delayed pending response, ID-token time validity is checked at its recorded
receipt time along with its signature and identity. An expired access token is
renewed using the verified replacement refresh token before inference resumes.
Interactive sign-in still requires an ID token valid at verification time.

`cargo run -p by-human -- auth-status` reports only safe readiness metadata.
To explicitly replace the selected registration, use `login --new-registration`
and complete a new consent flow. Replacement happens after validation succeeds.
This first slice has no logout/revocation command, remote import UI or device
code flow.

## Inference boundary

ChatGPT-plan requests go to the public Responses API, use streaming and
`store: false`, and carry canonical conversation context from PostgreSQL.
Functions are namespaced and unsupported generation parameters are omitted.
The initial built-in function only reads the current UTC time.

OpenAI's hosted web search is offered as a top-level tool beside the `by_human`
namespace unless `BY_HUMAN_WEB_SEARCH=off`. It remains subject to model and
account/workspace policy
([inference limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)).
Other hosted OpenAI tools such as remote MCP, computer use, file search and audio
transcription are not enabled by this route. Future by-human tools can implement
local MCP/browser connectors through its own execution and permission layer.

No live account test is implied by the local OAuth/JWKS contract tests. Confirm a
real login and successful inference separately before switching daily use away
from OpenClaw.

## 日本語

普段のChatGPTプランでログインできます。APIキーは必須ではなく、従量課金へ自動で
切り替えることもありません。`login`が表示するOpenAIのURLを、同じPCのブラウザで
開いて認証・同意してください。

認証情報は既定で`~/.config/by-human`に保存します。他アプリのトークンは読みません。
プラン利用を許可しなかった場合は、`login --enable-plan`で同じ登録のまま再同意できます。
`auth-status`では秘密値や個人情報を出さず、利用準備の状態だけを確認できます。
`BY_HUMAN_WEB_SEARCH=off`にしない限り、OpenAIのWeb検索を`by_human`の名前空間と並ぶ
トップレベルのツールとして提供します。使えるかどうかは、モデルやアカウント／
ワークスペースのポリシーにもよります。
ローカル試験と、実アカウントでのログイン・応答確認は別の検証として扱います。

Sources:
[Open-source token sharing](https://developers.openai.com/siwc/token-sharing-open-source),
[Sign-in flow](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
[Profiles and refresh](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions),
[Token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference),
[Inference limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
