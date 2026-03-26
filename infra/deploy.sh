#!/usr/bin/env bash
set -euo pipefail

DEPLOY_HOST="${DEPLOY_HOST:?Set DEPLOY_HOST}"
DEPLOY_USER="${DEPLOY_USER:-ubuntu}"
REMOTE_DIR="${REMOTE_DIR:-/opt/shardd}"
SSH_OPTS="${SSH_OPTS:-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

echo "==> Building Docker image..."
docker build -f "$ROOT_DIR/apps/node/Dockerfile" -t shardd-node:latest "$ROOT_DIR"

echo "==> Saving image to tarball..."
docker save shardd-node:latest | gzip > /tmp/shardd-node.tar.gz

echo "==> Syncing to $DEPLOY_USER@$DEPLOY_HOST:$REMOTE_DIR..."
ssh $SSH_OPTS "$DEPLOY_USER@$DEPLOY_HOST" "mkdir -p $REMOTE_DIR"
rsync -avz --progress /tmp/shardd-node.tar.gz "$DEPLOY_USER@$DEPLOY_HOST:$REMOTE_DIR/"
rsync -avz --progress "$SCRIPT_DIR/compose.yml" "$DEPLOY_USER@$DEPLOY_HOST:$REMOTE_DIR/"
rsync -avz --progress "$SCRIPT_DIR/caddy/" "$DEPLOY_USER@$DEPLOY_HOST:$REMOTE_DIR/caddy/"

if [ -f "$SCRIPT_DIR/.env" ]; then
    rsync -avz --progress "$SCRIPT_DIR/.env" "$DEPLOY_USER@$DEPLOY_HOST:$REMOTE_DIR/"
fi

echo "==> Loading image on remote..."
ssh $SSH_OPTS "$DEPLOY_USER@$DEPLOY_HOST" "cd $REMOTE_DIR && gunzip -c shardd-node.tar.gz | docker load"

echo "==> Starting services..."
ssh $SSH_OPTS "$DEPLOY_USER@$DEPLOY_HOST" "cd $REMOTE_DIR && docker compose up -d"

echo "==> Deploy complete."
