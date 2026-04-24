#!/usr/bin/env python3

import argparse
import json
import subprocess
import sys
import urllib.parse
from pathlib import Path


def parse_args():
    parser = argparse.ArgumentParser(
        description="Rewrite legacy owner_* bucket prefixes to user_* prefixes in shardd node databases."
    )
    parser.add_argument(
        "--dashboard-dsn",
        help="Dashboard Postgres DSN to read public_id -> user_id mappings before the dashboard migration drops developer_accounts.",
    )
    parser.add_argument(
        "--mapping-file",
        help="Path to a JSON file containing [{'public_id': 'dev_123', 'user_id': '<uuid>'}, ...].",
    )
    parser.add_argument(
        "--write-mapping",
        help="Optional path to write the resolved mapping JSON for later reuse.",
    )
    parser.add_argument(
        "--node-dsn",
        action="append",
        default=[],
        help="Node Postgres DSN to migrate. Pass once per node database.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show how many rows would change without mutating node databases.",
    )
    args = parser.parse_args()
    if not args.dashboard_dsn and not args.mapping_file:
        parser.error("pass --dashboard-dsn or --mapping-file")
    if not args.node_dsn:
        parser.error("pass at least one --node-dsn")
    return args


def sanitize_namespace_value(value: str) -> str:
    return "".join(
        ch if (ch.isascii() and (ch.isalnum() or ch in "_-")) else "_"
        for ch in value
    )


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def run_psql(dsn: str, sql: str) -> str:
    result = subprocess.run(
        ["psql", dsn, "-v", "ON_ERROR_STOP=1", "-At", "-F", "\t", "-c", sql],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def redact_dsn(dsn: str) -> str:
    parsed = urllib.parse.urlsplit(dsn)
    if not parsed.scheme or not parsed.hostname:
        return "<redacted>"
    user = parsed.username or "user"
    host = parsed.hostname
    port = f":{parsed.port}" if parsed.port else ""
    path = parsed.path or ""
    return f"{parsed.scheme}://{user}:***@{host}{port}{path}"


def load_mapping_from_dashboard(dashboard_dsn: str):
    rows = run_psql(
        dashboard_dsn,
        """
        SELECT public_id, owner_user_id::text
        FROM developer_accounts
        ORDER BY public_id
        """,
    ).splitlines()
    mapping = []
    for row in rows:
        if not row.strip():
            continue
        public_id, user_id = row.split("\t", 1)
        mapping.append({"public_id": public_id, "user_id": user_id})
    return mapping


def load_mapping_from_file(path: str):
    raw = json.loads(Path(path).read_text())
    if isinstance(raw, dict):
        items = [{"public_id": key, "user_id": value} for key, value in raw.items()]
    else:
        items = raw
    mapping = []
    for entry in items:
        public_id = str(entry["public_id"]).strip()
        user_id = str(entry["user_id"]).strip()
        if public_id and user_id:
            mapping.append({"public_id": public_id, "user_id": user_id})
    return mapping


def normalize_mapping(mapping):
    deduped = {}
    for entry in mapping:
        public_id = entry["public_id"]
        user_id = entry["user_id"]
        existing = deduped.get(public_id)
        if existing and existing != user_id:
            raise SystemExit(
                f"conflicting mapping for {public_id}: {existing} vs {user_id}"
            )
        deduped[public_id] = user_id
    resolved = []
    for public_id, user_id in sorted(deduped.items()):
        old_prefix = f"owner_{sanitize_namespace_value(public_id)}__bucket_"
        new_prefix = f"user_{sanitize_namespace_value(user_id)}__bucket_"
        resolved.append(
            {
                "public_id": public_id,
                "user_id": user_id,
                "old_prefix": old_prefix,
                "new_prefix": new_prefix,
                "old_len": len(old_prefix),
            }
        )
    return resolved


def mapping_values_sql(mapping):
    values = []
    for entry in mapping:
        values.append(
            "("
            + ", ".join(
                [
                    sql_literal(entry["public_id"]),
                    sql_literal(entry["user_id"]),
                    sql_literal(entry["old_prefix"]),
                    sql_literal(entry["new_prefix"]),
                    str(entry["old_len"]),
                ]
            )
            + ")"
        )
    return ",\n            ".join(values)


def count_sql(mapping):
    values = mapping_values_sql(mapping)
    return f"""
        WITH mapping(public_id, user_id, old_prefix, new_prefix, old_len) AS (
            VALUES
            {values}
        )
        SELECT
            m.public_id,
            m.user_id,
            COUNT(e.*)::bigint
        FROM mapping m
        LEFT JOIN events e ON e.bucket LIKE m.old_prefix || '%'
        GROUP BY m.public_id, m.user_id
        ORDER BY m.public_id
    """


def update_sql(mapping):
    values = mapping_values_sql(mapping)
    return f"""
        WITH mapping(public_id, user_id, old_prefix, new_prefix, old_len) AS (
            VALUES
            {values}
        ),
        updated AS (
            UPDATE events e
            SET bucket = m.new_prefix || substr(e.bucket, m.old_len + 1)
            FROM mapping m
            WHERE e.bucket LIKE m.old_prefix || '%'
            RETURNING m.public_id, m.user_id
        )
        SELECT public_id, user_id, COUNT(*)::bigint
        FROM updated
        GROUP BY public_id, user_id
        ORDER BY public_id
    """


def parse_count_rows(raw: str):
    counts = []
    for line in raw.splitlines():
        if not line.strip():
            continue
        public_id, user_id, count = line.split("\t", 2)
        counts.append(
            {"public_id": public_id, "user_id": user_id, "count": int(count or "0")}
        )
    return counts


def refresh_balance_summary(dsn: str):
    subprocess.run(
        [
            "psql",
            dsn,
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            "REFRESH MATERIALIZED VIEW CONCURRENTLY balance_summary",
        ],
        check=True,
        capture_output=True,
        text=True,
    )


def main():
    args = parse_args()

    mapping = []
    if args.dashboard_dsn:
        mapping.extend(load_mapping_from_dashboard(args.dashboard_dsn))
    if args.mapping_file:
        mapping.extend(load_mapping_from_file(args.mapping_file))
    mapping = normalize_mapping(mapping)

    if not mapping:
        raise SystemExit("no developer->user mappings found")

    if args.write_mapping:
        output_path = Path(args.write_mapping)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(mapping, indent=2) + "\n")

    summary = {
        "dry_run": args.dry_run,
        "mapping_count": len(mapping),
        "nodes": [],
    }

    for node_dsn in args.node_dsn:
        preview_counts = parse_count_rows(run_psql(node_dsn, count_sql(mapping)))
        total_matches = sum(entry["count"] for entry in preview_counts)
        node_summary = {
            "node_target": redact_dsn(node_dsn),
            "matched_rows": total_matches,
            "by_mapping": preview_counts,
        }

        if not args.dry_run and total_matches > 0:
            updated_counts = parse_count_rows(run_psql(node_dsn, update_sql(mapping)))
            node_summary["updated_rows"] = sum(entry["count"] for entry in updated_counts)
            node_summary["updated_by_mapping"] = updated_counts
            refresh_balance_summary(node_dsn)
            node_summary["refreshed_balance_summary"] = True
        else:
            node_summary["updated_rows"] = 0
            node_summary["updated_by_mapping"] = []
            node_summary["refreshed_balance_summary"] = False

        summary["nodes"].append(node_summary)

    json.dump(summary, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
