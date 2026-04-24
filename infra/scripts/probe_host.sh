#!/usr/bin/env bash
set -euo pipefail

DEPLOY_USER="${1:?deploy user required}"
INFRA_SSH_KEY_B64="${2:?infra ssh public key required}"
EXPECTED_HOSTNAME="${3:-}"

tmp_key="$(mktemp)"
trap 'rm -f "$tmp_key"' EXIT
printf '%s' "$INFRA_SSH_KEY_B64" | base64 -d > "$tmp_key"
pubkey="$(cat "$tmp_key")"

docker_installed=0
docker_active=0
deploy_user_docker_access=0
ufw_installed=0
ufw_active=0
docker_ufw_patch=0
infra_ssh_key_installed=0
tailscale_active=0
hostname_matches=0
tailscale_ipv4=""
docker_registry_trusted=0

if command -v docker >/dev/null 2>&1; then
    docker_installed=1
fi

if systemctl is-active --quiet docker 2>/dev/null; then
    docker_active=1
fi

if sudo -u "$DEPLOY_USER" docker version >/dev/null 2>&1; then
    deploy_user_docker_access=1
fi

if command -v ufw >/dev/null 2>&1; then
    ufw_installed=1
fi

if sudo ufw status 2>/dev/null | grep -q '^Status: active'; then
    ufw_active=1
fi

if sudo grep -q '# BEGIN UFW AND DOCKER' /etc/ufw/after.rules 2>/dev/null; then
    docker_ufw_patch=1
fi

if sudo test -f "/home/$DEPLOY_USER/.ssh/authorized_keys" && sudo grep -Fqx "$pubkey" "/home/$DEPLOY_USER/.ssh/authorized_keys"; then
    infra_ssh_key_installed=1
fi

if command -v tailscale >/dev/null 2>&1 && sudo tailscale status --json 2>/dev/null | grep -q '"BackendState": *"Running"'; then
    tailscale_active=1
    tailscale_ipv4="$(sudo tailscale ip -4 2>/dev/null | head -n1 || true)"
fi

if [ -n "$EXPECTED_HOSTNAME" ] && [ "$(hostname)" = "$EXPECTED_HOSTNAME" ]; then
    hostname_matches=1
fi

if sudo test -f /etc/docker/daemon.json && sudo grep -q '"insecure-registries"' /etc/docker/daemon.json 2>/dev/null; then
    docker_registry_trusted=1
fi

cat <<EOF
{"docker_installed":${docker_installed},"docker_active":${docker_active},"deploy_user_docker_access":${deploy_user_docker_access},"ufw_installed":${ufw_installed},"ufw_active":${ufw_active},"docker_ufw_patch":${docker_ufw_patch},"infra_ssh_key_installed":${infra_ssh_key_installed},"tailscale_active":${tailscale_active},"hostname_matches":${hostname_matches},"tailscale_ipv4":"${tailscale_ipv4}","docker_registry_trusted":${docker_registry_trusted}}
EOF
