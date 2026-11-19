#!/usr/bin/env bash
# Runs on the deploy host (see docs/deploy.md): check out a commit, rebuild the
# server image and restart the stack, then wait until the server is healthy.
set -euo pipefail

ref=${1:-origin/main}
repo=${BY_HUMAN_REPO_DIR:-$HOME/by-human}
export BY_HUMAN_DEPLOY_DIR=${BY_HUMAN_DEPLOY_DIR:-$HOME/by-human-deploy}
export BY_HUMAN_DEPLOY_UID=${BY_HUMAN_DEPLOY_UID:-$(id -u)}

cd "$repo"
git fetch --quiet --prune origin
git checkout --quiet --detach "$ref"
compose() { docker compose -f deploy/compose.yaml "$@"; }

# A restart interrupts work in progress, so refuse while any run is active.
if [ -n "$(compose ps -q --status running postgres)" ]; then
  active=$(compose exec -T postgres psql -U byhuman -d byhuman -tAc \
    "SELECT count(*) FROM runs WHERE status IN ('queued', 'running')")
  if [ "$active" != "0" ]; then
    echo "deploy: $active run(s) queued or running; try again when they finish" >&2
    exit 1
  fi
fi

compose build --pull server
compose up -d --remove-orphans
for _ in $(seq 1 90); do
  status=$(docker inspect -f '{{.State.Health.Status}}' "$(compose ps -q server)")
  if [ "$status" = healthy ]; then
    echo "deploy: by-human $(git rev-parse --short HEAD) is healthy"
    docker image prune -f --filter label=com.docker.compose.project=by-human >/dev/null
    exit 0
  fi
  sleep 2
done
echo "deploy: the server did not become healthy; see: docker compose -f deploy/compose.yaml logs server" >&2
exit 1
