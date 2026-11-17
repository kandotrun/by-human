# by-human

An independent, self-hosted personal agent, built in Rust with OpenAI and
PostgreSQL. The server is being developed in small, tested changes. Pi's design
is a reference; running Pi, OpenClaw or Mukumo is not required.

独立してセルフホストできる個人用エージェントを、Rust・OpenAI・PostgreSQLで開発しています。
小さな変更ごとに検証して追加します。Piは設計の参考で、本体の実行依存には含めません。

## Current delivery

The first layer provides PostgreSQL conversation/run storage, idempotent
submission, cancellation state, durable tool receipts and explicit recovery of
interrupted work. It does not yet expose a chat API or perform live model calls.
See [architecture](docs/architecture.md) for the following delivery steps.

最初の層は、会話・実行状態の保存、送信の重複防止、停止状態、ツールの実行記録、
中断した実行の復旧を提供します。チャットAPIとモデル呼び出しは次の変更で追加します。

## Development

Requirements: Rust 1.96+ and an isolated PostgreSQL 17+ database.

```sh
export DATABASE_URL='postgres://byhuman:local-test-only@127.0.0.1:5432/byhuman'
cargo run -p by-human -- check
```

Use a database created for by-human; `check` applies its migrations. The password
above is only a local test example. Runtime credentials belong in the environment,
not in Git. `.env.example` is a template, not an automatically loaded config file.

by-human専用のDBを用意してください。`check`はマイグレーションを適用します。
上記のパスワードはローカル試験用です。秘密値は環境変数で渡し、Gitに保存しません。

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

## Earlier experiments

`core/` contains the earlier Python prototype; `by-human-app/` contains the Expo
experiment. They are preserved separately and are not dependencies of the Rust
server.
