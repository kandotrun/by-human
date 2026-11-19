# Deploying to an always-on host

by-human can run on a Linux machine that stays on, so the chat keeps working
while your own computer is off. Docker Compose runs three containers from
[deploy/compose.yaml](../deploy/compose.yaml):

- `postgres`: PostgreSQL 17 with its data in the `postgres` volume.
- `server`: by-human, built from this repository by
  [deploy/Dockerfile](../deploy/Dockerfile). The ChatGPT login lives in the
  `auth` volume.
- `tunnel`: a Cloudflare Tunnel connector that shares the server's network
  namespace.

The server still listens on `127.0.0.1:4310`, inside its own namespace, so
browser sessions work as they do on a laptop. Only the tunnel reaches it, and
no port is published on the host. All three containers restart with Docker
after a reboot.

## One-time host setup

The host needs Docker Engine with Compose v2, git, and an SSH login for a user
in the `docker` group.

1. Clone the repository:

   ```sh
   git clone https://github.com/kandotrun/by-human.git ~/by-human
   ```

2. Create `~/by-human-deploy` with mode `0700`, holding three files with mode
   `0600`:

   - `postgres.env`: `POSTGRES_PASSWORD=<a random password>`
   - `server.env`:

     ```sh
     DATABASE_URL=postgres://byhuman:<the same password>@postgres:5432/byhuman
     BY_HUMAN_API_TOKEN=<32 to 512 non-whitespace ASCII characters>
     BY_HUMAN_OPENAI_AUTH=chatgpt
     ```

     Add `BY_HUMAN_PUBLIC_ORIGIN` and, if you use them, the Cloudflare Access
     variables described in [browser chat](web-chat.md#cloudflare-access).
     Do not set `BY_HUMAN_LISTEN` or `BY_HUMAN_AUTH_DIR`; the Compose file and
     the image set them.
   - `tunnel.token`: the token of a remotely managed Cloudflare Tunnel whose
     published application points at `http://127.0.0.1:4310`.

3. Give the server a ChatGPT login. Commands on the host run from `~/by-human`
   after:

   ```sh
   export BY_HUMAN_DEPLOY_DIR=~/by-human-deploy BY_HUMAN_DEPLOY_UID=$(id -u)
   ```

   Either sign in on the host with
   `docker compose -f deploy/compose.yaml run --rm server login --enable-plan`,
   forwarding the callback port it prints over SSH to the computer whose
   browser completes the sign-in, or copy `openai.json` from another
   machine's `~/.config/by-human` into the `auth` volume:

   ```sh
   docker compose -f deploy/compose.yaml run --rm --no-deps --user root \
     -v "$PWD/openai.json:/import/openai.json:ro" --entrypoint install server \
     -m 600 -o by-human -g by-human /import/openai.json /home/by-human/.config/by-human/openai.json
   ```

   Then delete the copied file. Stop by-human on the other machine first: two
   servers refreshing the same login invalidate each other.

4. To keep existing conversations, stop the old server, dump its database with
   `pg_dump -Fc`, start only PostgreSQL on the host and restore the dump before
   the first server start:

   ```sh
   docker compose -f deploy/compose.yaml up -d postgres
   docker compose -f deploy/compose.yaml exec -T postgres \
     pg_restore -U byhuman -d byhuman --no-owner --role=byhuman < byhuman.dump
   ```

5. Start everything with the first deploy below.

## Deploy

From your computer, with an SSH destination for the host:

```sh
BY_HUMAN_DEPLOY_HOST=user@host just deploy                  # latest main
BY_HUMAN_DEPLOY_HOST=user@host just deploy origin/<branch>  # another ref
```

[deploy/remote-deploy.sh](../deploy/remote-deploy.sh) runs on the host. It
checks out the commit, refuses while a run is queued or running, rebuilds the
server image, restarts the stack and waits until `/health` answers. On the host,
`BY_HUMAN_REPO_DIR` (default `~/by-human`) and `BY_HUMAN_DEPLOY_DIR` (default
`~/by-human-deploy`) override the locations.

## Operating

- Logs: `docker compose -f deploy/compose.yaml logs -f server`
- Backup:
  `docker compose -f deploy/compose.yaml exec -T postgres pg_dump -U byhuman -Fc byhuman > byhuman.dump`
- Stop: `docker compose -f deploy/compose.yaml down` (volumes are kept)

## 日本語

電源を入れたままのLinuxマシンでby-humanを動かすと、自分のPCを閉じていてもチャットを使えます。
Docker Composeで、PostgreSQL 17・by-human・Cloudflare Tunnelのコネクタの3つのコンテナを動かします。
by-humanはコンテナ内の`127.0.0.1:4310`で待ち受け、そこに届くのはネットワークを共有するTunnelだけです。
ホストのポートは公開せず、再起動後もDockerと一緒に自動で起動します。

最初に、ホストにリポジトリをcloneし、`~/by-human-deploy`（0700）に`postgres.env`・`server.env`・`tunnel.token`（それぞれ0600）を置きます。
ChatGPTのログインは、ホストで`login --enable-plan`を実行するか、別のマシンの`openai.json`を`auth`ボリュームへコピーします。
コピーする場合は、先に元のマシンのby-humanを止めてください。
既存の会話は、元のデータベースを`pg_dump -Fc`で書き出し、初回起動の前に`pg_restore`で入れます。

以後のデプロイは、自分のPCで`BY_HUMAN_DEPLOY_HOST=user@host just deploy`を実行します。
実行中の処理があるときはデプロイを中止し、終わったあとは`/health`が応答するまで待ちます。
