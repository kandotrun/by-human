# Deploy a commit (default: the latest main) to the always-on host named by the
# SSH destination in BY_HUMAN_DEPLOY_HOST. See docs/deploy.md.
deploy ref="origin/main":
    ssh "${BY_HUMAN_DEPLOY_HOST:?set BY_HUMAN_DEPLOY_HOST to the SSH destination}" 'bash -s' -- "{{ref}}" < deploy/remote-deploy.sh
