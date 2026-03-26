#!/usr/bin/env bash
set -euo pipefail

echo "Setting up UFW firewall rules for shardd..."

# Allow SSH
ufw allow ssh

# Allow HTTP/HTTPS (Caddy)
ufw allow 80/tcp
ufw allow 443/tcp

# Block direct external access to node ports (internal only via Docker network)
# If nodes need to be directly accessible, uncomment:
# ufw allow 3001:3003/tcp

ufw --force enable
ufw status verbose

echo "Firewall configured."
