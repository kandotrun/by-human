<div align="center">

<img src="docs/images/readme-hero.ja.svg" width="100%" alt="by-human のイメージ。左に白い小文字の b が入ったオレンジ色のアプリアイコンと、「自分のコンピューターで動く、自分専用のAIエージェント。」の文字。中央のブラウザの窓は 127.0.0.1:4310 を開いたチャット画面で、サイドバーには今日・昨日・過去 7 日間に分けた会話一覧、本文には大阪出張の準備についての返信が届いている途中で、「応答中」の表示、予定の表、チェックリスト、停止ボタンがある。右の iPhone では、同じチャットが「いま何時？」に時計ツールで答えている。">

# by-human

**自分のコンピューターで動く、自分専用のAIエージェント。**<br>
ChatGPTプランでログインし、会話はすべて自分のPostgreSQLに保存。<br>
ブラウザからもiPhoneからも話しかけられます。

by-humanは、自分で動かすRust製のサーバーひとつで動きます。<br>
チャット画面を内蔵し、使えるツールは内蔵の時計と、自分で指定したものだけ。ツールが最後まで動いたか分からないときは、勝手にやり直さず、止まってあなたに確認を求めます。

<sub>持ち主が毎日使っているOpenClawの機能を置き換えるために開発しています。単体で動き、Piは設計の参考にしているだけです。Pi・OpenClaw・Mukumoを動かす必要はありません。</sub>

[![Rust server CI](https://github.com/kandotrun/by-human/actions/workflows/rust.yml/badge.svg)](https://github.com/kandotrun/by-human/actions/workflows/rust.yml)
[![Rust 1.96+](https://img.shields.io/badge/Rust-1.96%2B-C2410C?logo=rust&logoColor=white&labelColor=1F2328)](#-開発)
[![PostgreSQL 17+](https://img.shields.io/badge/PostgreSQL-17%2B-C2410C?logo=postgresql&logoColor=white&labelColor=1F2328)](#-はじめかた)
[![OpenAI: ChatGPTプラン](https://img.shields.io/badge/OpenAI-ChatGPT%E3%83%97%E3%83%A9%E3%83%B3-C2410C?labelColor=1F2328)](docs/openai-login.md)
[![MCP: stdio · HTTP](https://img.shields.io/badge/MCP-stdio%20%C2%B7%20HTTP-C2410C?labelColor=1F2328)](docs/mcp.md)
[![macOS · Linux](https://img.shields.io/badge/platform-macOS%20%C2%B7%20Linux-C2410C?labelColor=1F2328)](#-はじめかた)

**[はじめかた](#-はじめかた)**&emsp;**[できること](#-できること)**&emsp;**[iPhoneから使う](#-iphoneから使う)**&emsp;[しくみ](#-しくみ)&emsp;[確認済みのこと](#-確認済みのこととまだのこと)&emsp;[Read in English](README.md)

</div>

> [!NOTE]
> by-humanはまだ初期段階で、小さな変更ごとに検証しながら育てています。チャット画面は日本語です。`docs/`のガイドは英語で、ほとんどの末尾に短い日本語の説明があります。

## 🎬 画面

<div align="center">

<table>
  <tr>
    <td align="center" width="33%"><img src="docs/images/chat-light.webp" width="300" alt="ライトモードのブラウザ版チャット。サイドバーでは会話が今日・昨日・過去 7 日間・10月に分かれている。開いている会話は大阪出張の準備の相談で、返信には見出し、3日分の予定の表、チェックリストがあり、その下にコピーボタンがある。"></td>
    <td align="center" width="33%"><img src="docs/images/chat-dark-streaming.webp" width="300" alt="ダークモードで返信が届いている途中のチャット。タイトルの下に「応答中」の表示がある。Rustの所有権を説明する返信は、番号付きリストとRustのコードブロックの最初の数行まで届いていて、入力欄には停止ボタンがある。"></td>
    <td align="center" width="33%"><img src="docs/images/review-uncertain.webp" width="300" alt="停止でツールが中断されたあとの「実行結果の確認」ダイアログ。送信したメッセージ、結果が不明と表示されたツール mcp_tasks_5f0c2a9e41b7d368、記録された引数と null の出力、タスクは追加されていなかったという確認メモ、記録を確認して再実行しないことへのチェックが並ぶ。"></td>
  </tr>
  <tr>
    <td align="center"><b>💬 ブラウザでチャット</b><br>Markdownの返信と<br>日付ごとの履歴</td>
    <td align="center"><b>⚡ 動きが見える</b><br>状態表示、ストリーミング、<br>停止</td>
    <td align="center"><b>🛟 分からない結果は確認</b><br>結果が不明な実行は確認を待ち、<br>自動で再実行しない</td>
  </tr>
  <tr>
    <td align="center"><img src="docs/images/phone-drawer.webp" width="170" alt="スマートフォン相当の画面で会話一覧を開いたところ。「新しい会話」ボタン、検索欄、日付ごとに分かれた会話があり、各会話に「⋯」メニューがある。"></td>
    <td align="center"><img src="docs/images/phone-trip.webp" width="170" alt="スマートフォン相当の画面で大阪出張の会話を開いたところ。予定の表とチェックリストが狭い画面に収まり、下に入力欄がある。"></td>
    <td align="center"><img src="docs/images/phone-clock-dark.webp" width="170" alt="ダークモードのスマートフォン相当の画面。「いま何時？」と聞くと、時計ツールで確認したうえで日本時間の現在時刻を答えている。"></td>
  </tr>
  <tr>
    <td align="center"><b>🔎 会話をすぐ探せる</b><br>検索、名前の変更、<br>削除</td>
    <td align="center"><b>📱 iPhoneでも</b><br>Tailscale経由で<br>ホーム画面に追加</td>
    <td align="center"><b>🧰 許可したツールだけ</b><br>内蔵の時計と、<br>指定したMCPツール</td>
  </tr>
</table>

<sub>2026年11月19日に、PlaywrightのChromiumで撮影しました（1280×900と、390×844のスマートフォン相当の画面）。実際のチャット画面・HTTP API・ランナーを、隔離したPostgreSQL 17と、OpenAIの代わりの合成モデルで動かしています。会話と<code>tasks</code>ツールはサンプルです。</sub>

</div>

## ✨ できること

- 🔑 **APIキーなしで、ChatGPTプランで動く** — by-humanは独自にOpenAIのログインを登録し、公開されているResponses APIと直接やり取りします。CodexやOpenClawなど、ほかのアプリの認証情報は読みません。APIキーの従量課金へ勝手に切り替えることもなく、従量課金は明示的に選んだときだけです。
- 🗄️ **履歴は自分のPostgreSQLに** — 会話、実行、イベント、ツールの実行記録は自分のデータベースに保存します。返信を作るときは、会話の内容を`store: false`でOpenAIに送ります。ページを閉じても実行は止まらず、イベントの配信は途切れたところから再開します。同じキーで送り直しても、実行が二重になることはありません。
- 💬 **チャット画面はサーバーに内蔵** — `http://127.0.0.1:4310/`を開けば、Markdownの返信のストリーミング表示、停止、検索、名前の変更と削除が使え、ライト／ダークにも対応します。フロントエンドのビルドも実行時のNode.jsも不要で、ブラウザのログインは、使わないまま約30日たつと切れます（最長400日）。
- 📱 **tailnetの中で、iPhoneからも** — Tailscale Serveでサーバーを共有し、ページをホーム画面に追加できます。専用のアイコンから全画面で開き、入力欄はキーボードの上に表示されます。
- 🧰 **MCPツールは指定したものだけ** — 内蔵の時計のほかにモデルが使えるのは、stdioかStreamable HTTPでつないだMCPサーバーのうち、名前で指定したツールだけです。指定していないツールはモデルに見せず、各MCPサーバーに渡す環境変数やBearerトークンも指定したものだけです（stdioのサーバーは`PATH`も引き継ぎます）。
- 🛟 **結果が分からない処理は、あなたを待つ** — 停止、クラッシュ、タイムアウトでツールの結果が分からなくなった実行は`uncertain`として残し、自動で再実行しません。記録を確認してメモを残してから、会話を続けます。
- 🔒 **秘密はその場から動かない** — ChatGPTのログイン情報は所有者だけが読める`~/.config/by-human`に保存し、ブラウザには渡しません。サーバー用トークンもページには保存しません。

## 🚀 はじめかた

必要なものは、macOSまたはLinux、Rust 1.96以降、by-human専用のPostgreSQL 17以降のデータベース、ChatGPTプランです。この方法でプランを使えるアカウントかどうかはOpenAIが決めます。詳しくは[ChatGPT-plan login](docs/openai-login.md)を参照してください。

### 1. データベースを準備する

```sh
git clone https://github.com/kandotrun/by-human.git
cd by-human
export DATABASE_URL='postgres://byhuman:local-test-only@127.0.0.1:5432/byhuman'
cargo run -p by-human -- check
```

`check`はマイグレーションを適用し、`by-human: PostgreSQL schema is ready`と表示します。上記のパスワードはローカル試験用の例です。秘密値は環境変数で渡し、Gitには保存しません。[`.env.example`](.env.example)は設定の一覧ですが、テンプレートなので自動では読み込まれません。

<details>
<summary>PostgreSQLがまだない場合：Dockerで起動する</summary>

```sh
docker run -d --name by-human-postgres \
  -e POSTGRES_USER=byhuman -e POSTGRES_PASSWORD=local-test-only -e POSTGRES_DB=byhuman \
  -p 127.0.0.1:5432:5432 -v by-human-postgres:/var/lib/postgresql/data \
  postgres:17-alpine
```

CIと同じPostgreSQLのイメージです。このコンピューターからの接続だけを受け付け、名前付きボリュームのおかげでコンテナを削除してもデータが残ります。

</details>

### 2. ChatGPTでログインする

```sh
cargo run -p by-human -- login
cargo run -p by-human -- auth-status
```

表示されたOpenAIのURLを、**同じコンピューターの**ブラウザで開いてください。ログインはローカルの`127.0.0.1`へのコールバックで完了します。`auth-status`は、`authenticated=true`や`plan_enabled=true`のような準備状態だけを表示します。`plan_enabled`が`false`なら、`cargo run -p by-human -- login --enable-plan`でプランの利用を許可してください。

### 3. サーバーを起動する

サーバーには、OpenAIのログインとは別の専用トークンが必要です。空白を含まない32〜512文字のASCII文字列にしてください。一度だけ作り、パスワードマネージャーなどに保管します。変更すると、すべてのブラウザがログアウトします。

```sh
openssl rand -hex 32   # 一度だけ。出力を保管する
export BY_HUMAN_API_TOKEN='<保管したトークン>'
cargo run -p by-human -- serve
```

### 4. チャットを開く

<http://127.0.0.1:4310/>を開き、サーバー用トークンで一度だけログインします。
同じトークンは、[HTTP API](docs/http-api.md)のBearerトークンとしても使えます。

> [!TIP]
> `OPENAI_API_KEY`は、`BY_HUMAN_OPENAI_AUTH=api-key`も設定しない限り使われません。設定した場合は、プランではなくOpenAI Platformのアカウントに課金されます。

## 📱 iPhoneから使う

iPhoneからサーバーに接続するにはHTTPSが必要です。両方の端末に[Tailscale](https://tailscale.com/)を入れ、tailnetでMagicDNSとHTTPS証明書を有効にしたうえで、次のように設定します。

1. by-humanを動かしているコンピューターで`tailscale serve --bg 4310`を実行します（Linuxでは、自分のユーザーがTailscaleのoperatorでなければ`sudo`を付けます）。`https://<machine>.<tailnet>.ts.net`のようなアドレスが表示されます。
2. そのアドレスを末尾のスラッシュなしで、そのまま`BY_HUMAN_PUBLIC_ORIGIN`に設定し、`serve`を再起動します。
3. iPhoneのSafariでアドレスを開いてサーバー用トークンでログインし、共有メニューから「ホーム画面に追加」を選びます。

iPhoneでは、Returnで改行し、送信ボタンで送信します。

> [!WARNING]
> Tailscale Serveを使うと、tailnetで許可されたすべての端末とユーザーがページを開けるようになります。会話はサーバー用トークンで守られます。インターネットに公開してしまう`tailscale funnel`は使わないでください。

## 🧰 MCPツールをつなぐ

[`mcp.example.json`](mcp.example.json)をコピーし、使うサーバーとツールだけを残して、`BY_HUMAN_MCP_CONFIG`でそのファイルを指定します。

```json
{
  "servers": [
    {
      "id": "notes",
      "transport": {
        "type": "stdio",
        "command": "/absolute/path/to/your-mcp-server",
        "args": [],
        "env": ["NOTES_TOKEN"]
      },
      "tools": ["read_note"],
      "timeout_seconds": 30
    }
  ]
}
```

```sh
export BY_HUMAN_MCP_CONFIG=/absolute/path/to/mcp.json
cargo run -p by-human -- mcp list
```

`mcp list`は接続して有効なツールをJSONで表示し、切断します。ツールは実行せず、データベースもChatGPTのログインも不要です。`serve`は起動時に同じファイルを読み込むので、変更したら再起動してください。stdioのサーバーは自分と同じユーザーで動き、サンドボックスではありません。信頼できるプログラムだけを指定してください。stdioのサーバーが受け取る環境変数は、by-humanから引き継ぐ`PATH`（設定されている場合）と、`env`に書いたもの（この例では`NOTES_TOKEN`）だけです。`env`に書いた変数は、by-humanを動かす環境で設定しておかないと起動に失敗します。HTTPのサーバーにはHTTPS（ローカルならループバックのHTTP）で接続し、`bearer_token_env`で指定したトークンだけを使います。詳しくは[MCP tools](docs/mcp.md)を参照してください。

## 🧭 しくみ

```mermaid
flowchart LR
    browser["ブラウザ"] --> server
    phone["iPhone<br>Tailscale経由"] --> server
    server["by-human<br>自分の<br>コンピューター"] <--> db[("PostgreSQL<br>会話と実行")]
    server --> openai["OpenAI<br>Responses API<br>ChatGPTプラン"]
    server --> tools["MCPサーバー<br>指定した<br>ツールだけ"]
```

送ったメッセージは、それぞれ1つの**実行**（run）になります。by-humanは最初に実行をPostgreSQLへ保存し、それからモデルの返信をストリーミングして、モデルが求めたツールを呼び出します。ツールを呼ぶ前にはこれから行う内容を、呼んだ後には受け取った結果を記録します。画面には返信と進み具合を表示し、推論の内容やツールの生の出力はチャットの本文に混ぜません。

| 実行の状態 | 意味 |
| --- | --- |
| `queued` | 保存済みで、ランナーの順番を待っている |
| `running` | モデル、またはモデルが求めたツールが動いている |
| `completed` | 返信が完了した |
| `failed` | エラーで終わった |
| `cancelled` | 停止で終わり、結果が分からないツールは残っていない |
| `uncertain` | 結果を確かめられない。ツールが中断された、実行中にサーバーが再起動した、など |

> [!IMPORTANT]
> 停止は実行の終了を求める操作で、外部のツールが止まったことの証明にはなりません。by-humanもそのようには扱いません。`uncertain`の実行がある会話は、確認するまで新しいメッセージを送れません（ほかの会話は使えます）。確認するとメモが記録され、会話を続けられます。古い処理を再実行したり、成功扱いにしたりはしません。

## 🔧 設定

| 環境変数 | 既定値 | 内容 |
| --- | --- | --- |
| `DATABASE_URL` | `check`と`serve`で必須 | by-human用のPostgreSQLデータベース |
| `BY_HUMAN_API_TOKEN` | `serve`で必須 | チャット画面とHTTP API用のサーバートークン |
| `BY_HUMAN_LISTEN` | `127.0.0.1:4310` | 待ち受けるアドレスとポート |
| `OPENAI_MODEL` | `gpt-6.1-sol` | 返信を書くモデル |
| `BY_HUMAN_OPENAI_AUTH` | `chatgpt` | `chatgpt`はプランを使う。`api-key`はAPIの従量課金を明示的に選ぶ |
| `OPENAI_API_KEY` | 未設定 | `BY_HUMAN_OPENAI_AUTH=api-key`のときだけ読む |
| `BY_HUMAN_AUTH_DIR` | `~/.config/by-human` | ChatGPTのログイン情報を置く、所有者専用のフォルダ |
| `BY_HUMAN_MCP_CONFIG` | 未設定（MCPなし） | MCP設定ファイルのパス |
| `BY_HUMAN_PUBLIC_ORIGIN` | 未設定 | サーバーへ中継する`https://`のオリジン1つ（Tailscale Serveのアドレスなど） |

| コマンド | 内容 |
| --- | --- |
| `login` | このコンピューターのブラウザで、ChatGPTプランにログインする |
| `login --enable-plan` | 同じ登録のまま、プランの利用許可をもう一度求める |
| `login --new-registration` | 新しく同意して、保存済みの登録を置き換える |
| `auth-status` | 準備状態だけを表示する（秘密値は出さない） |
| `mcp list` | 設定したMCPサーバーに接続し、有効なツールを一覧表示する |
| `check` | PostgreSQLのマイグレーションを適用し、データベースの準備ができたか確認する |
| `serve` | HTTP API、チャット画面、ランナーを起動する |

どれも`cargo run -p by-human -- <command>`の形で実行します。

## ✅ 確認済みのことと、まだのこと

**CIで確認していること**（サーバーかブラウザテストを変更するプルリクエストごと。Ubuntu、PostgreSQL 17）:

- `cargo fmt`、警告をエラーとして扱う`cargo clippy`、生成した試験用schemaで実際のPostgreSQLを使う`cargo test`。
- PlaywrightによるChromiumでのブラウザ契約テスト。実際のAPIとランナーを合成モデルで動かし、日本語入力、ログイン、Markdown、検索、停止、確認、ライト／ダークのコントラスト、画面キーボードを再現したスマートフォン相当の画面を確かめます。

**自動テストでは確かめていないこと**:

- 実際のChatGPTアカウントでのログインと応答。OAuthとトークンの検証は、ローカルの試験用サーバーに対して行っています。
- 実機のiPhoneのSafariと、Tailscale Serve自体。
- OpenClawとの機能の同等性や、性能の向上。

**既知の制限**:

- 会話に表示するのは、8 MiBに収まる範囲で最新100件の実行までです。古い実行も保存されていますが、ページングはまだありません。
- コンテキストは256ターンまたは2 MiBまでで、圧縮はまだありません。超えると実行は`CONTEXT_LIMIT_REACHED`で失敗します。
- 1つのデータベースを使えるサーバーは1つだけです。同じデータベースで2つ目の`serve`は起動できません。
- サーバー自体はTLS、CORS、端末のペアリングを持ちません。ループバックのまま使うか、Tailscale Serve経由で接続してください。
- ChatGPTのログインには、まだログアウトや失効のコマンドがありません。
- MCPサーバーのOAuthログインと、古いHTTP+SSEトランスポートには対応していません。
- シェルコマンドの実行、ブラウザ操作、スケジュール実行、プッシュ通知はまだありません。
- チャット画面は日本語のみです。

## 🧱 構成

```text
server/
  src/
    api.rs, web.rs        HTTP APIと内蔵のチャット画面
    runner.rs, engine/    実行の管理、モデルとツールのループ、OpenAIのストリーミング
    auth/                 ChatGPTのログイン、トークンの更新と保存
    browser_session.rs    チャット画面のブラウザセッション
    mcp/                  stdioとStreamable HTTPでのMCP接続
    store.rs              PostgreSQLへの保存
  web/                    チャット画面（HTML、CSS、JavaScript、アイコン）
  migrations/             PostgreSQLのスキーマ
  tests/                  Rustの契約テストと、そこで使う合成fixture
web-tests/                Playwrightのブラウザ契約テスト（開発用）
docs/                     ガイドと、docs/superpowers/ の設計メモ
by-human-app/             以前のExpoの試作（サーバーでは使わない）
```

## 🧪 開発

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
```

`--all-features`で有効になる`postgres-tests`には`DATABASE_URL`が必要で、生成した試験用schemaで実際のPostgreSQLを使います。テストが既存のOpenClaw、Every、Mukumoのサービスに接続することはありません。

ブラウザ契約テストは、同じRustのAPIとランナーを、合成モデルと隔離して生成したschemaで動かします。Node.jsはこのテストにだけ必要です（CIはNode.js 24を使用）。

```sh
cd web-tests
npm ci
npx playwright install chromium
npm test
```

Linuxでは`npx playwright install --with-deps chromium`にすると、Chromiumに必要なシステムライブラリも入ります。隔離した`DATABASE_URL`をexportしたまま実行してください。ブラウザ用のfixtureはループバックの43219番ポートを使い、通常のビルドには含まれません。

## 📚 ドキュメント

| ガイド | 内容 |
| --- | --- |
| [architecture.md](docs/architecture.md) | PostgreSQLが持つ状態、復旧、OpenAIとの境界 |
| [openai-login.md](docs/openai-login.md) | ChatGPTプランでのログイン、認証情報の保存場所、トークンの更新 |
| [http-api.md](docs/http-api.md) | ルート、ブラウザセッション、イベントの再取得、停止と確認 |
| [web-chat.md](docs/web-chat.md) | チャット画面、iPhoneの設定、結果不明の実行の確認 |
| [mcp.md](docs/mcp.md) | MCPの設定、結果、停止と復旧 |
| [deploy.md](docs/deploy.md) | 常時起動のLinuxホストでDocker Composeを使って動かす方法と`just deploy` |
| [設計メモ](docs/superpowers/specs/2026-11-18-daily-driver-web-chat-design.md) | ブラウザチャットの設計。実装計画は[docs/superpowers/plans](docs/superpowers/plans)にあります |

## 📄 ライセンスとクレジット

このリポジトリには、まだライセンスがありません。

by-humanは[Pi Agent Core](https://github.com/earendil-works/pi/blob/main/packages/agent/README.md)を設計の参考にしており、MCPクライアントには[公式のRust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)を使っています。by-humanは独立したプロジェクトで、OpenAIとは関係ありません。
