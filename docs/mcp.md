# MCP tools

by-human connects to owner-configured MCP servers through stdio or Streamable
HTTP. It uses the [official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)
inside its existing tool execution
boundary; MCP tools do not require OpenAI-hosted MCP or an OpenAI API key.
Every/Zenkioku is not an integration requirement or a built-in connection.

## Configure and inspect

Copy [mcp.example.json](../mcp.example.json) to your local configuration directory,
replace the example connections, and keep only the servers and tools you want.
No MCP connection is made when `BY_HUMAN_MCP_CONFIG` is unset.

```sh
export BY_HUMAN_MCP_CONFIG=/absolute/path/to/mcp.json
cargo run -p by-human -- mcp list
```

`mcp list` connects, initializes the configured servers, lists the enabled tools
as JSON, then disconnects. It does not call tools and needs neither PostgreSQL
nor a ChatGPT login. Output contains the server ID, remote tool name, stable
model-facing name, and description. Endpoints, command arguments and credential
values are omitted.

The same configuration is used by `serve`. Start it with the database, server
authentication and ChatGPT login described in the [README](../README.md).
The built-in clock remains available alongside configured MCP tools.

Each server must have:

- A unique ID of 1–32 ASCII letters, digits or underscores.
- An explicit `tools` list containing exact remote tool names. Unlisted tools
  are not exposed; a listed tool missing from the server fails startup.
- A `transport` object with `type: "stdio"` or `type: "http"`.
- An optional `timeout_seconds` between 1 and 120; the default is 30.

For stdio, `command` and optional `cwd` are absolute paths. `args` are passed
directly without a shell. The child inherits `PATH` and only the additional
environment variables named in `env`; application and model credentials are not
automatically forwarded. Supply any required values in the server's environment.
The configured program runs as the same OS user as by-human, so choose a program
you trust. This transport is not an OS sandbox.

For HTTP, use an HTTPS MCP endpoint or literal loopback HTTP for local tools.
`bearer_token_env` names the environment variable containing that server's bearer
token. There is no fallback to the ChatGPT token, OpenAI API key, database
credentials or by-human API token. Redirects are refused. OAuth login for MCP
servers and the older HTTP+SSE transport are not implemented by this slice.

The catalog is fixed for the current process. Restart after changing the
configuration or enabled tool definitions. Tool names are mapped to deterministic
per-server aliases, preventing identical remote names from colliding. Tool input
schemas are validated locally before dispatch, without fetching external schema
references. A whole model-proposed batch is checked before its first tool runs.

## Results, Stop and recovery

Each call first records an execution intent in PostgreSQL. A received MCP result,
including `isError: true`, is stored as its actual receipt and returned to the
model. Tool content and structured results are not ordinary chat messages.
Arguments are limited to 16 KiB and stored results to 256 KiB. Server messages
and tool catalogs are bounded, as are connection and call timeouts.

Stop requests cancellation and prevents the next model/tool dispatch. Cancellation
does not prove that a remote operation stopped. If a call times out, disconnects,
or is interrupted after dispatch without a definitive result, its durable run
becomes `uncertain`; it is not blindly replayed or declared completed. As in the
initial backend, uncertain work blocks new input in that conversation until its
outcome is reviewed; a reconciliation UI is not implemented yet.

## 日本語

MCPサーバーを設定ファイルで指定し、使わせるツールを`tools`へ明示します。
ローカルのstdio接続とStreamable HTTPに対応し、Every／Zenkiokuへの接続は組み込みません。
`BY_HUMAN_MCP_CONFIG`を指定しなければ、MCPプロセスの起動や外部への接続は行いません。

```sh
export BY_HUMAN_MCP_CONFIG=/absolute/path/to/mcp.json
cargo run -p by-human -- mcp list
```

このコマンドで接続と有効なツール一覧を確認できます。ツール自体は実行せず、
DBやChatGPT認証も不要です。同じ設定を`serve`が読み込みます。
stdioの実行ファイルは絶対パスを指定し、追加の環境変数は`env`で名前だけ指定します。
HTTPの認証も`bearer_token_env`で指定した環境変数だけを使用します。
エンドポイントに秘密値が含まれる場合も、設定ファイルを公開しないでください。

実行前に引数を検証し、PostgreSQLへ開始を記録します。結果が届いた場合は、
MCPのエラー結果も含めて記録してからモデルへ渡します。停止・切断・タイムアウトにより
実行結果が分からない場合は`uncertain`として残し、自動で同じ操作を繰り返しません。
ツールの出力は吹き出しの本文へ混ぜず、会話にはモデルの説明と進捗だけを表示します。
