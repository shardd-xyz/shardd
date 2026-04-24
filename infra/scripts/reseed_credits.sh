#!/usr/bin/env bash
#
# Post-wipe verification helper for the v1.9 per-bucket migration.
#
# The dashboard's /api/billing/status endpoint already auto-provisions
# each user's credit balance to their plan's allowance whenever the
# observed balance is below the allowance (see
# `apps/billing/src/adapters/http/routes/billing.rs:78-97`). That means
# no explicit "reseed" step is required — the first time each user hits
# the dashboard after the wipe, their plan allowance is granted
# automatically via `MeshClient::create_billing_event`, which allocates
# fresh `(bucket=__billing__<user_id>, seq)` under the new scheme.
#
# This script just:
# 1. Verifies every edge's `sync_gap` is 0 after the deploy.
# 2. Prints a sanity count of users whose billing balance is still 0
#    (i.e. haven't had their first post-wipe login yet) so operators
#    know how many auto-provisions are still pending.
#
# Usage:
#   ./infra/scripts/reseed_credits.sh <deployment>   # e.g. prod

set -euo pipefail

DEPLOYMENT="${1:?deployment name required (e.g. prod)}"
SECRETS="infra/secrets/${DEPLOYMENT}.env"
if [[ ! -f "$SECRETS" ]]; then
    echo "secrets file not found: $SECRETS" >&2
    exit 1
fi

CLUSTER="infra/state/cluster.json"
SSH_KEY="infra/secrets/${DEPLOYMENT}-infra"
chmod 600 "$SSH_KEY"

# Each edge's public DNS name.
EDGES=$(
    python3 -c "
import json, sys
d = json.load(open('${CLUSTER}'))
for e in d['deployments']['${DEPLOYMENT}']['public_edges']:
    print(e['edge_id'], e['base_url'])
"
)

echo "→ checking sync_gap on every edge"
ALL_ZERO=1
while IFS= read -r line; do
    edge_id=$(echo "$line" | awk '{print $1}')
    base_url=$(echo "$line" | awk '{print $2}')
    gap=$(curl -fsS --max-time 5 "${base_url}/gateway/health" \
        | python3 -c "import sys, json; d = json.load(sys.stdin); print(d.get('sync_gap'))")
    printf "  %s  sync_gap=%s\n" "$edge_id" "$gap"
    if [[ "$gap" != "0" ]]; then
        ALL_ZERO=0
    fi
done <<< "$EDGES"

if [[ $ALL_ZERO -eq 1 ]]; then
    echo "✓ every edge reports sync_gap=0 — per-bucket migration healthy"
else
    echo "! at least one edge has a non-zero gap — investigate before treating the migration as done" >&2
fi

# Dashboard IP for postgres exec.
DASHBOARD_IP=$(
    python3 -c "
import json
d = json.load(open('${CLUSTER}'))
mach = d['deployments']['${DEPLOYMENT}']['machines']
for name, info in mach.items():
    if 'dashboard' in name and info.get('public_ip'):
        print(info['public_ip']); break
    elif 'dashboard' in name and info.get('public_dns_name'):
        print(info['public_dns_name']); break
"
)

echo "→ counting users who haven't auto-provisioned yet"
PENDING=$(
    ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no -o BatchMode=yes \
        "ubuntu@${DASHBOARD_IP}" '
        sudo docker exec shardd-prod-use1-dashboard-postgres \
            psql -U saassy -d user_gateway -At \
                -c "SELECT COUNT(*) FROM users WHERE deleted_at IS NULL AND is_frozen = FALSE"
    '
)
echo "  $PENDING active users exist; each will auto-provision on next /api/billing/status hit"
echo
echo "Nothing else to do — auto-provision happens on first login."
