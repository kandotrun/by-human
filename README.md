<div align="center">

<img src="docs/images/readme-hero.svg" width="100%" alt="by-human at a glance. On the left, the orange app icon with a white lowercase b, and the words “Your own AI agent, on your own computer.” In the middle, a browser window at 127.0.0.1:4310 shows the chat in Japanese: the sidebar lists conversations under today, yesterday and the past 7 days, and a reply about preparing a business trip to Osaka is streaming in, with a status pill reading 応答中 (replying), a schedule table, a checklist and a Stop button. On the right, an iPhone shows the same chat answering “what time is it?” with the clock tool.">

# by-human

**Your own AI agent, on your own computer.**<br>
Sign in with your ChatGPT plan, keep every conversation in your own PostgreSQL,<br>
and chat from a browser or your iPhone.

by-human is a single Rust server that you run yourself.<br>
It serves its own chat page, uses only the tools you list, and when it can't tell
whether a tool finished, it stops and asks you instead of trying again.

<sub>Being built to replace the OpenClaw features its owner uses every day. It stands alone: Pi's design is a reference, and running Pi, OpenClaw or Mukumo is not required.</sub>

[![Rust server CI](https://github.com/kandotrun/by-human/actions/workflows/rust.yml/badge.svg)](https://github.com/kandotrun/by-human/actions/workflows/rust.yml)
[![Rust 1.96+](https://img.shields.io/badge/Rust-1.96%2B-C2410C?logo=rust&logoColor=white&labelColor=1F2328)](#-development)
[![PostgreSQL 17+](https://img.shields.io/badge/PostgreSQL-17%2B-C2410C?logo=postgresql&logoColor=white&labelColor=1F2328)](#-quick-start)
[![OpenAI: ChatGPT plan](https://img.shields.io/badge/OpenAI-ChatGPT%20plan-C2410C?labelColor=1F2328)](docs/openai-login.md)
[![MCP: stdio · HTTP](https://img.shields.io/badge/MCP-stdio%20%C2%B7%20HTTP-C2410C?labelColor=1F2328)](docs/mcp.md)
[![macOS · Linux](https://img.shields.io/badge/platform-macOS%20%C2%B7%20Linux-C2410C?labelColor=1F2328)](#-quick-start)

**[Quick start](#-quick-start)**&emsp;**[Features](#-features)**&emsp;**[iPhone](#-use-it-from-your-iphone)**&emsp;[How it works](#-how-it-works)&emsp;[What's verified](#-whats-verified-and-what-isnt)&emsp;[日本語で読む](README.ja.md)

</div>

> [!NOTE]
> by-human is at an early stage and grows in small, tested changes.
> The chat page is in Japanese. The guides in `docs/` are in English, and most end with a short Japanese section.

## 🎬 Screens

<div align="center">

<table>
  <tr>
    <td align="center" width="33%"><img src="docs/images/chat-light.webp" width="300" alt="The browser chat in light mode, in Japanese. The sidebar groups conversations under 今日 (today), 昨日 (yesterday), 過去 7 日間 (past 7 days) and 10月 (October). The open conversation asks for help preparing a business trip to Osaka, and the reply shows a heading, a three-day schedule table and a checklist, with a copy button below it."></td>
    <td align="center" width="33%"><img src="docs/images/chat-dark-streaming.webp" width="300" alt="The chat in dark mode while a reply streams in. A status pill under the title reads 応答中 (replying). The reply explaining Rust ownership shows a numbered list and the first lines of a Rust code block, and the message box shows a 停止 (Stop) button."></td>
    <td align="center" width="33%"><img src="docs/images/review-uncertain.webp" width="300" alt="The 実行結果の確認 (review the result) dialog after Stop interrupted a tool. It shows the sent message, the tool mcp_tasks_5f0c2a9e41b7d368 marked as having an unknown result, its recorded arguments with an output of null, an owner note saying the task was not added, and a checked box confirming the record was reviewed and the work will not be run again."></td>
  </tr>
  <tr>
    <td align="center"><b>💬 Chat in your browser</b><br>Markdown replies and<br>history grouped by day</td>
    <td align="center"><b>⚡ Watch it work</b><br>Live status, streamed text<br>and Stop</td>
    <td align="center"><b>🛟 Review the unknown</b><br>Interrupted tool work<br>is never run again</td>
  </tr>
  <tr>
    <td align="center"><img src="docs/images/phone-drawer.webp" width="170" alt="The chat on a phone-sized screen with the conversation drawer open: a 新しい会話 (new conversation) button, a search field, and conversations grouped by day, each with a ⋯ menu."></td>
    <td align="center"><img src="docs/images/phone-trip.webp" width="170" alt="The Osaka trip conversation on a phone-sized screen. The schedule table and the checklist fit the narrow screen, and the message box sits at the bottom."></td>
    <td align="center"><img src="docs/images/phone-clock-dark.webp" width="170" alt="A phone-sized screen in dark mode. Asked いま何時？ (what time is it?), the agent answers that it checked with the clock tool, and gives the time in Japan."></td>
  </tr>
  <tr>
    <td align="center"><b>🔎 Find any conversation</b><br>Search, rename<br>and delete</td>
    <td align="center"><b>📱 On your iPhone</b><br>Add it to the Home Screen<br>through Tailscale</td>
    <td align="center"><b>🧰 Tools you allow</b><br>The built-in clock, plus<br>the MCP tools you list</td>
  </tr>
</table>

<sub>Captured on November 19, 2026 with Playwright in Chromium, at 1280×900 and on a 390×844 phone-sized screen. The real chat page, HTTP API and runner used an isolated PostgreSQL 17 database and a synthetic model in place of OpenAI. The conversations and the <code>tasks</code> tool are sample data.</sub>

</div>

## ✨ Features

- 🔑 **Your ChatGPT plan, no API key** — by-human registers its own OpenAI sign-in and talks to the public Responses API. It never reads the credentials of Codex, OpenClaw or any other app, and never switches to API-key billing on its own. That is an explicit opt-in.
- 🗄️ **Everything stays in your PostgreSQL** — Conversations, runs, events and tool receipts live in a database you own. Closing the page doesn't stop a run, the event stream picks up where it left off, and resending with the same key never starts a second run.
- 💬 **A chat page that ships with the server** — Open `http://127.0.0.1:4310/` for streamed Markdown replies, Stop, search, rename and delete, in light or dark. There is no frontend build, no Node.js at runtime, and a browser stays signed in for 30 days after you last use it.
- 📱 **On your iPhone, inside your tailnet** — Share the server with Tailscale Serve and add the page to your Home Screen. It opens full screen with its own icon, and the message box stays above the keyboard.
- 🧰 **Only the tools you list** — Connect MCP servers over stdio or Streamable HTTP, and name each tool you allow. Unlisted tools are never offered to the model, and a tool gets no credentials except the ones you name for it.
- 🛟 **Unknown results wait for you** — When Stop, a crash or a timeout leaves a tool's result unknown, the run is marked `uncertain` and is never replayed. You look at what was recorded, leave a note, and then continue the conversation.
- 🔒 **Secrets stay put** — The ChatGPT sign-in lives in an owner-only `~/.config/by-human` folder, the browser never sees it, and the page never stores the server token.

## 🚀 Quick start

You need macOS or Linux, Rust 1.96 or later, a PostgreSQL 17+ database used only by by-human, and a ChatGPT plan.
OpenAI decides which accounts can use their plan this way; see [ChatGPT-plan login](docs/openai-login.md).

### 1. Prepare the database

```sh
git clone https://github.com/kandotrun/by-human.git
cd by-human
export DATABASE_URL='postgres://byhuman:local-test-only@127.0.0.1:5432/byhuman'
cargo run -p by-human -- check
```

`check` applies the migrations and prints `by-human: PostgreSQL schema is ready`.
The password above is only a local example. Pass secrets through the environment and keep them out of Git.
[`.env.example`](.env.example) lists the settings, but it is a template and is never loaded automatically.

<details>
<summary>No PostgreSQL yet? Start one in Docker</summary>

```sh
docker run -d --name by-human-postgres \
  -e POSTGRES_USER=byhuman -e POSTGRES_PASSWORD=local-test-only -e POSTGRES_DB=byhuman \
  -p 127.0.0.1:5432:5432 -v by-human-postgres:/var/lib/postgresql/data \
  postgres:17-alpine
```

This is the same PostgreSQL image that CI uses. It listens only on this computer, and the named volume keeps your data when the container is removed.

</details>

### 2. Sign in with ChatGPT

```sh
cargo run -p by-human -- login
cargo run -p by-human -- auth-status
```

Open the printed OpenAI URL in a browser **on the same computer**. The sign-in finishes on a local `127.0.0.1` callback.
`auth-status` prints only readiness flags, such as `authenticated=true` and `plan_enabled=true`.
If `plan_enabled` is `false`, run `login --enable-plan` to grant plan access.

### 3. Start the server

The server has its own token, separate from your OpenAI sign-in: 32 to 512 ASCII characters without spaces.
Create it once and keep it, for example in your password manager. Changing it signs every browser out.

```sh
openssl rand -hex 32   # once; save the output
export BY_HUMAN_API_TOKEN='<the token you saved>'
cargo run -p by-human -- serve
```

### 4. Open the chat

Open <http://127.0.0.1:4310/> and sign in once with the server token.
The same token also works as a Bearer token for the [HTTP API](docs/http-api.md).

> [!TIP]
> `OPENAI_API_KEY` is never used unless you also set `BY_HUMAN_OPENAI_AUTH=api-key`, which bills your OpenAI Platform account instead of your plan.

## 📱 Use it from your iPhone

The iPhone needs HTTPS to reach the server. With [Tailscale](https://tailscale.com/) on both devices, and MagicDNS and HTTPS certificates turned on for your tailnet:

1. On the computer running by-human, run `tailscale serve --bg 4310`. It prints an address such as `https://<machine>.<tailnet>.ts.net`.
2. Set `BY_HUMAN_PUBLIC_ORIGIN` to exactly that address, with no trailing slash, and restart `serve`.
3. On the iPhone, open the address in Safari, sign in with the server token, and choose Share → Add to Home Screen.

On the phone, Return adds a new line and the send button sends.

> [!WARNING]
> Tailscale Serve makes the page reachable by every device and user in your tailnet. The server token still protects your conversations.
> Never use `tailscale funnel`, which publishes the page to the internet. `tailscale serve reset` removes the Serve setting.

## 🧰 Connect MCP tools

Copy [`mcp.example.json`](mcp.example.json), keep only the servers and tools you want, and point `BY_HUMAN_MCP_CONFIG` at it:

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

`mcp list` connects, prints the enabled tools as JSON, and disconnects. It calls no tool and needs neither the database nor a ChatGPT sign-in.
`serve` reads the same file at startup, so restart it after a change.
A stdio server runs as your own user and is not sandboxed, so choose programs you trust.
HTTP servers use HTTPS (or loopback HTTP) and only the bearer token named in `bearer_token_env`.
Details: [MCP tools](docs/mcp.md).

## 🧭 How it works

```mermaid
flowchart LR
    browser["Browser"] --> server
    phone["iPhone<br>via Tailscale"] --> server
    server["by-human<br>on your<br>computer"] <--> db[("PostgreSQL<br>conversations<br>and runs")]
    server --> openai["OpenAI<br>Responses API<br>ChatGPT plan"]
    server --> tools["MCP servers<br>only the tools<br>you list"]
```

Each message you send becomes a **run**. by-human saves the run in PostgreSQL first, then streams the model's reply and calls the tools it asks for.
Before each tool call it records what it is about to do, and afterwards the result it got.
The page shows the reply and the progress; reasoning and raw tool output never appear as chat text.

| Run state | Meaning |
| --- | --- |
| `queued` | Saved and waiting for the runner |
| `running` | The model, or a tool it asked for, is working |
| `completed` | The reply is finished |
| `failed` | It ended with an error |
| `cancelled` | Stop ended it, with no tool result left in doubt |
| `uncertain` | The result can't be confirmed, for example a tool was interrupted or the server restarted mid-run |

> [!IMPORTANT]
> Stop asks a run to end. It doesn't prove that a remote tool stopped, so by-human never treats it that way.
> An `uncertain` run blocks new messages in its conversation (other conversations keep working) until you review it.
> Reviewing records your note and lets you continue. It never re-runs the old work or marks it as a success.

## 🔧 Configuration

| Variable | Default | What it does |
| --- | --- | --- |
| `DATABASE_URL` | required by `check` and `serve` | The PostgreSQL database for by-human |
| `BY_HUMAN_API_TOKEN` | required by `serve` | The server token for the chat page and the HTTP API |
| `BY_HUMAN_LISTEN` | `127.0.0.1:4310` | The address and port to listen on |
| `OPENAI_MODEL` | `gpt-6.1-sol` | The model that writes the replies |
| `BY_HUMAN_OPENAI_AUTH` | `chatgpt` | `chatgpt` uses your plan; `api-key` opts in to API billing |
| `OPENAI_API_KEY` | unset | Read only when `BY_HUMAN_OPENAI_AUTH=api-key` |
| `BY_HUMAN_AUTH_DIR` | `~/.config/by-human` | The owner-only folder that holds the ChatGPT sign-in |
| `BY_HUMAN_MCP_CONFIG` | unset, no MCP | The path to your MCP configuration |
| `BY_HUMAN_PUBLIC_ORIGIN` | unset | One exact `https://` origin that proxies to the server, such as your Tailscale Serve address |

| Command | What it does |
| --- | --- |
| `login` | Sign in with your ChatGPT plan, in a browser on this computer |
| `login --enable-plan` | Ask again for plan access, keeping the same registration |
| `login --new-registration` | Replace the saved registration after a new consent |
| `auth-status` | Print safe readiness flags, never secrets |
| `mcp list` | Connect the configured MCP servers and list the enabled tools |
| `check` | Apply the PostgreSQL migrations and check that the database is ready |
| `serve` | Start the HTTP API, the chat page and the runner |

Run each one as `cargo run -p by-human -- <command>`.

## ✅ What's verified, and what isn't

**Checked in CI** on every pull request that changes the server or the browser tests (Ubuntu, PostgreSQL 17):

- `cargo fmt`, `cargo clippy` with warnings as errors, and `cargo test` with real PostgreSQL contracts in generated test schemas.
- Browser contracts in Chromium with Playwright, against the real API and runner with a synthetic model. They cover Japanese input, sign-in, Markdown, search, Stop, review, contrast in light and dark, and phone-sized screens with a simulated on-screen keyboard.

**Not established by automated tests:**

- A real ChatGPT sign-in and a real reply. The OAuth and token checks run against local test servers.
- Safari on a real iPhone, and Tailscale Serve itself.
- Feature parity with OpenClaw, or any performance gain.

**Known limits:**

- A conversation shows its latest 100 runs. Older runs stay stored, but there is no paging yet.
- Context is capped at 256 turns or 2 MiB, with no compaction yet; past that, a run fails with `CONTEXT_LIMIT_REACHED`.
- One server per database. A second `serve` on the same database is refused.
- The server has no TLS, CORS or device pairing of its own. Keep it on loopback, or reach it through Tailscale Serve.
- The ChatGPT sign-in has no logout or revocation command yet.
- MCP servers can't use OAuth sign-in, and the older HTTP+SSE transport isn't supported.
- Shell commands, browser automation, schedules and push notifications aren't built yet.
- The chat page is in Japanese only.

## 🧱 Repository layout

```text
server/
  src/
    api.rs, web.rs        HTTP API and the embedded chat page
    runner.rs, engine/    Runs, the model and tool loop, OpenAI streaming
    auth/                 ChatGPT sign-in, token refresh and storage
    browser_session.rs    Browser sessions for the chat page
    mcp/                  MCP over stdio and Streamable HTTP
    store.rs              PostgreSQL storage
  web/                    The chat page: HTML, CSS, JavaScript and icons
  migrations/             PostgreSQL schema
  tests/                  Rust contracts and their synthetic fixtures
web-tests/                Playwright browser contracts (development only)
docs/                     Guides, and design notes in docs/superpowers/
core/                     Earlier Python prototype (not used by the server)
by-human-app/             Earlier Expo experiment (not used by the server)
```

## 🧪 Development

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
```

`--all-features` turns on `postgres-tests`, which needs `DATABASE_URL` and runs real database contracts in generated test schemas.
Tests never connect to an existing OpenClaw, Every or Mukumo service.

The browser contracts run the same Rust API and runner with a synthetic model and an isolated generated schema.
Node.js is needed only for them:

```sh
cd web-tests
npm ci
npx playwright install chromium
npm test
```

Keep the isolated `DATABASE_URL` exported. The browser fixture binds loopback port 43219 and is not part of a normal build.

## 📚 Docs

| Guide | Covers |
| --- | --- |
| [architecture.md](docs/architecture.md) | What PostgreSQL owns, recovery, and the OpenAI boundary |
| [openai-login.md](docs/openai-login.md) | ChatGPT-plan sign-in, where credentials live, and token refresh |
| [http-api.md](docs/http-api.md) | Routes, browser sessions, event replay, Stop and review |
| [web-chat.md](docs/web-chat.md) | The chat page, iPhone setup, and reviewing uncertain runs |
| [mcp.md](docs/mcp.md) | MCP configuration, results, Stop and recovery |
| [Design notes](docs/superpowers/specs/2026-11-18-daily-driver-web-chat-design.md) | The browser chat design; implementation plans are in [docs/superpowers/plans](docs/superpowers/plans) |

## 📄 License and credits

This repository doesn't include a license yet.

by-human uses [Pi Agent Core](https://github.com/earendil-works/pi/blob/main/packages/agent/README.md) as a design reference, and its MCP client uses the [official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk).
by-human is an independent project and is not affiliated with OpenAI.
