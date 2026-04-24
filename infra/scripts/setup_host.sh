#!/usr/bin/env bash
set -euo pipefail

REMOTE_ROOT="${1:?remote root required}"
DEPLOY_USER="${2:?deploy user required}"
ALLOWED_PORTS_CSV="${3:-}"
AUTHORIZED_KEYS_B64="${4:?authorized ssh public keys required}"
HOSTNAME_TARGET="${5:?hostname required}"
TAILSCALE_AUTH_KEY="${6:?tailscale auth key required}"

export DEBIAN_FRONTEND=noninteractive

sudo apt-get update -y
compose_pkg=""
if apt-cache show docker-compose-plugin >/dev/null 2>&1; then
    compose_pkg="docker-compose-plugin"
elif apt-cache show docker-compose-v2 >/dev/null 2>&1; then
    compose_pkg="docker-compose-v2"
elif apt-cache show docker-compose >/dev/null 2>&1; then
    compose_pkg="docker-compose"
fi

install_packages=(ca-certificates curl gnupg ufw docker.io)
if [ -n "$compose_pkg" ]; then
    install_packages+=("$compose_pkg")
fi
sudo apt-get install -y "${install_packages[@]}"

sudo systemctl enable --now docker
sudo usermod -aG docker "$DEPLOY_USER"
sudo mkdir -p "$REMOTE_ROOT"
sudo chown -R "$DEPLOY_USER:$DEPLOY_USER" "$REMOTE_ROOT"

sudo install -d -m 700 -o "$DEPLOY_USER" -g "$DEPLOY_USER" "/home/$DEPLOY_USER/.ssh"
tmp_keys="$(mktemp)"
trap 'rm -f "$tmp_keys"' EXIT
printf '%s' "$AUTHORIZED_KEYS_B64" | base64 -d > "$tmp_keys"
sudo touch "/home/$DEPLOY_USER/.ssh/authorized_keys"
sudo chown "$DEPLOY_USER:$DEPLOY_USER" "/home/$DEPLOY_USER/.ssh/authorized_keys"
sudo chmod 600 "/home/$DEPLOY_USER/.ssh/authorized_keys"
while IFS= read -r pubkey; do
    [ -n "$pubkey" ] || continue
    if ! sudo grep -Fqx "$pubkey" "/home/$DEPLOY_USER/.ssh/authorized_keys"; then
        printf '%s\n' "$pubkey" | sudo tee -a "/home/$DEPLOY_USER/.ssh/authorized_keys" >/dev/null
    fi
done < "$tmp_keys"

sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
if [ -n "$ALLOWED_PORTS_CSV" ]; then
    old_ifs="$IFS"
    IFS=','
    for port in $ALLOWED_PORTS_CSV; do
        [ -n "$port" ] || continue
        sudo ufw allow "${port}/tcp"
        sudo ufw route allow proto tcp to any port "${port}"
    done
    IFS="$old_ifs"
fi

read -r -d '' RULES <<'EOF' || true
# BEGIN UFW AND DOCKER
*filter
:ufw-user-forward - [0:0]
:ufw-docker-logging-deny - [0:0]
:DOCKER-USER - [0:0]
-A DOCKER-USER -j ufw-user-forward

-A DOCKER-USER -j RETURN -s 10.0.0.0/8
-A DOCKER-USER -j RETURN -s 172.16.0.0/12
-A DOCKER-USER -j RETURN -s 192.168.0.0/16
-A DOCKER-USER -j RETURN -s 100.64.0.0/10

-A DOCKER-USER -p udp -m udp --sport 53 --dport 1024:65535 -j RETURN

-A DOCKER-USER -j ufw-docker-logging-deny -p tcp -m tcp --tcp-flags FIN,SYN,RST,ACK SYN -d 192.168.0.0/16
-A DOCKER-USER -j ufw-docker-logging-deny -p tcp -m tcp --tcp-flags FIN,SYN,RST,ACK SYN -d 10.0.0.0/8
-A DOCKER-USER -j ufw-docker-logging-deny -p tcp -m tcp --tcp-flags FIN,SYN,RST,ACK SYN -d 172.16.0.0/12
-A DOCKER-USER -j ufw-docker-logging-deny -p udp -m udp --dport 0:32767 -d 192.168.0.0/16
-A DOCKER-USER -j ufw-docker-logging-deny -p udp -m udp --dport 0:32767 -d 10.0.0.0/8
-A DOCKER-USER -j ufw-docker-logging-deny -p udp -m udp --dport 0:32767 -d 172.16.0.0/12

-A DOCKER-USER -j RETURN

-A ufw-docker-logging-deny -m limit --limit 3/min --limit-burst 10 -j LOG --log-prefix "[UFW DOCKER BLOCK] "
-A ufw-docker-logging-deny -j DROP

COMMIT
# END UFW AND DOCKER
EOF

# Strip any previous block, then re-append so rule updates (e.g. adding the
# 100.64/10 tailnet RETURN for registry pulls) take effect on re-runs.
if sudo grep -q '# BEGIN UFW AND DOCKER' /etc/ufw/after.rules; then
    sudo sed -i '/# BEGIN UFW AND DOCKER/,/# END UFW AND DOCKER/d' /etc/ufw/after.rules
fi
printf '%s\n' "$RULES" | sudo tee -a /etc/ufw/after.rules >/dev/null

sudo ufw --force enable
sudo ufw reload

# Hostname — match the cluster.json machine name so `hostname` on the box,
# tailnet identity, and infractl all agree.
if [ "$(hostname)" != "$HOSTNAME_TARGET" ]; then
    sudo hostnamectl set-hostname "$HOSTNAME_TARGET"
fi
# Keep /etc/hosts aligned so sudo doesn't spam "unable to resolve host".
if ! grep -qE "^127\.0\.1\.1[[:space:]]+${HOSTNAME_TARGET}" /etc/hosts; then
    echo "127.0.1.1 ${HOSTNAME_TARGET}" | sudo tee -a /etc/hosts >/dev/null
fi

# Tailscale — idempotent: install-if-missing, then `up` (safe to re-run).
if ! command -v tailscale >/dev/null 2>&1; then
    curl -fsSL https://tailscale.com/install.sh | sudo sh
fi
sudo systemctl enable --now tailscaled
sudo tailscale up \
    --authkey="$TAILSCALE_AUTH_KEY" \
    --hostname="$HOSTNAME_TARGET" \
    --ssh \
    --accept-routes=false

# Trust the shardd private Docker registry on the tailnet. Reachable only
# from within the tailnet (registry binds its tailscale IP), so "insecure"
# in the HTTP-no-TLS sense is still tailnet-encrypted end-to-end. Restart
# docker only when the file actually changed.
sudo mkdir -p /etc/docker
cat <<'JSON' | sudo tee /etc/docker/daemon.json.new >/dev/null
{
  "insecure-registries": [
    "100.104.178.26:5000",
    "shardd-prod-use1-dashboard:5000"
  ]
}
JSON
if ! sudo cmp -s /etc/docker/daemon.json.new /etc/docker/daemon.json 2>/dev/null; then
    sudo mv /etc/docker/daemon.json.new /etc/docker/daemon.json
    sudo systemctl restart docker
else
    sudo rm -f /etc/docker/daemon.json.new
fi
