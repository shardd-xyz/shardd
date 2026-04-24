#!/usr/bin/env bash
set -euo pipefail

# Configure UFW and Docker firewall integration on a remote host.
# Usage:
#   HOST=ubuntu@16.162.34.54 ./infra/setup_firewall.sh

HOST="${HOST:-}"
SSH_OPTS="${SSH_OPTS:-}"

if [ -z "$HOST" ]; then
  echo "Usage: HOST=user@hostname [SSH_OPTS=\"...\"] $0" >&2
  exit 1
fi

ssh_cmd() {
  ssh -o "StrictHostKeyChecking no" $SSH_OPTS "$HOST" "$@"
}

echo "==> Installing ufw on $HOST..."
ssh_cmd "sudo apt-get update -qq && sudo apt-get install -y -qq ufw"

echo "==> Configuring UFW base policies..."
ssh_cmd "sudo ufw default deny incoming && sudo ufw default allow outgoing"
ssh_cmd "sudo ufw allow 22/tcp"
ssh_cmd "sudo ufw allow 80/tcp"
ssh_cmd "sudo ufw allow 443/tcp"
# Docker ports
ssh_cmd "sudo ufw allow 2376/tcp && sudo ufw allow 2377/tcp && sudo ufw allow 7946/tcp && sudo ufw allow 7946/udp && sudo ufw allow 4789/udp"
ssh_cmd "sudo ufw --force enable"

echo "==> Ensuring DOCKER-USER iptables rules..."
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

ssh_cmd "sudo grep -q '# BEGIN UFW AND DOCKER' /etc/ufw/after.rules || sudo tee -a /etc/ufw/after.rules >/dev/null" <<<"$RULES"

echo "==> Reloading ufw..."
ssh_cmd "sudo ufw reload"

echo "==> Firewall setup complete on $HOST."
