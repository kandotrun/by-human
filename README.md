# by-human

An independent, self-hosted personal agent, built in Rust with OpenAI and
PostgreSQL. The server is being developed in small, tested changes. Pi's design
is a reference; running Pi, OpenClaw or Mukumo is not required.

独立してセルフホストできる個人用エージェントを、Rust・OpenAI・PostgreSQLで開発しています。
小さな変更ごとに検証して追加します。Piは設計の参考で、本体の実行依存には含めません。

## Current delivery

The Rust server now provides its own ChatGPT-plan login, PostgreSQL conversation
and run storage, an authenticated HTTP API, streamed text, a bounded model/tool
loop, Stop, durable event replay and conservative restart recovery. The initial
tool reads the current UTC time. Owner-configured MCP tools can be added through
stdio or Streamable HTTP, with explicit tool selection and durable receipts.
An embedded browser client supports chat, Stop and explicit review of uncertain
work. This initial runtime does not claim feature parity with OpenClaw or a
measured performance improvement.

ChatGPTプランでの独自ログイン、会話・実行状態の保存、HTTP API、テキスト配信、
停止、イベント再取得、再起動後の復旧を実装しています。時刻取得に加え、設定した
MCPサーバーのツールをstdio／Streamable HTTPで接続できます。
同じサーバーから配信するWeb画面で、会話・停止・結果不明の実行の確認もできます。
OpenClawの全面置換や性能改善の実証は、今後の接続・運用検証として扱います。

See [architecture](docs/architecture.md), [login](docs/openai-login.md) and
[HTTP API](docs/http-api.md), [MCP tools](docs/mcp.md) and
[browser chat](docs/web-chat.md) for the current behavior and limits.

## Development

Requirements: Rust 1.96+ and an isolated PostgreSQL 17+ database.

```sh
export DATABASE_URL='postgres://byhuman:local-test-only@127.0.0.1:5432/byhuman'
cargo run -p by-human -- check
cargo run -p by-human -- login
cargo run -p by-human -- auth-status
```

Use a database created for by-human; `check` applies its migrations. The password
above is only a local test example. Runtime credentials belong in the environment,
not in Git. `.env.example` is a template, not an automatically loaded config file.

by-human専用のDBを用意してください。`check`はマイグレーションを適用します。
上記のパスワードはローカル試験用です。秘密値は環境変数で渡し、Gitに保存しません。

Sign in with your ChatGPT plan in a browser on the same computer. Then supply a
separate random server token (32–512 ASCII bytes) as `BY_HUMAN_API_TOKEN` and run:

```sh
cargo run -p by-human -- serve
```

The default API address is `127.0.0.1:4310`; `/v1` routes require the server
Bearer token or a browser session created from it (see [HTTP API](docs/http-api.md)).
`OPENAI_API_KEY` is not required and is not silently used. API-key
billing is an explicit opt-in with `BY_HUMAN_OPENAI_AUTH=api-key`.
ChatGPT credentials are stored in the dedicated owner-only `~/.config/by-human`
directory; they are never read from another application.

Open `http://127.0.0.1:4310/` to use the embedded chat client. Sign in once
with the server token; that browser then stays signed in for 30 days after its
last use. The browser supports streamed replies, Stop and explicit review of
uncertain work. No separate frontend build or server is required. See
[browser chat](docs/web-chat.md) for sign-in, tab storage and recovery behavior.

同じPCのブラウザでログインした後、OpenAI認証とは別のサーバー用トークンを
`BY_HUMAN_API_TOKEN`で渡し、`serve`を実行します。APIキーへの自動切替はありません。
認証情報は専用の`~/.config/by-human`へ保存します。

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
```

The `postgres-tests` feature requires `DATABASE_URL` and runs real database
contracts in generated test schemas. CI enables it. Tests never use an existing
OpenClaw, Every or Mukumo service.

`postgres-tests`を有効にした試験には`DATABASE_URL`が必要です。
生成した試験用schemaで実際のPostgreSQLを使い、既存サービスへ接続しません。

Browser contracts use the same Rust API/runner with a synthetic provider and an
isolated generated PostgreSQL schema. Node.js is needed only for these tests:

```sh
cd web-tests
npm ci
npx playwright install chromium
npm test
```

Keep the isolated `DATABASE_URL` exported. The browser fixture binds loopback
port 43219 and is not part of a normal application build.

## Earlier experiments

`core/` contains the earlier Python prototype; `by-human-app/` contains the Expo
experiment. They are preserved separately and are not dependencies of the Rust
server.
