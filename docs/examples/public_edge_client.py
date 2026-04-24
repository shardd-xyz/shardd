#!/usr/bin/env python3

import argparse
import json
import time
import urllib.error
import urllib.parse
import urllib.request


def http_json(url, api_key=None, method="GET", body=None, timeout=3.0):
    headers = {"Accept": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    if body is not None:
        headers["Content-Type"] = "application/json"
        body = json.dumps(body).encode("utf-8")
    request = urllib.request.Request(url, method=method, headers=headers, data=body)
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        latency_ms = (time.perf_counter() - started) * 1000.0
        return json.load(response), latency_ms


def normalize_base_url(url):
    return url.rstrip("/")


def discover_edges(bootstraps):
    seen = {}
    for base_url in bootstraps:
        base_url = normalize_base_url(base_url)
        seen[base_url] = {"base_url": base_url}
        try:
            payload, _ = http_json(f"{base_url}/gateway/edges")
        except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, ValueError):
            continue
        for edge in payload.get("edges", []):
            edge_base_url = normalize_base_url(edge.get("base_url", ""))
            if edge_base_url:
                seen[edge_base_url] = edge
    return list(seen.values())


def probe_edge(edge):
    base_url = normalize_base_url(edge["base_url"])
    try:
        health, latency_ms = http_json(f"{base_url}/gateway/health")
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, ValueError):
        return None
    return {
        "edge_id": health.get("edge_id") or edge.get("edge_id") or base_url,
        "region": health.get("region") or edge.get("region") or "unknown",
        "base_url": base_url,
        "latency_ms": latency_ms,
        "ready": bool(health.get("ready")),
        "overloaded": bool(health.get("overloaded", False)),
        "healthy_nodes": int(health.get("healthy_nodes", 0)),
    }


def edge_sort_key(candidate):
    return (
        not candidate["ready"],
        candidate["overloaded"],
        -candidate["healthy_nodes"],
        candidate["latency_ms"],
    )


def choose_edge(bootstraps):
    discovered = discover_edges(bootstraps)
    candidates = [probe_edge(edge) for edge in discovered]
    candidates = [candidate for candidate in candidates if candidate is not None]
    if not candidates:
        raise RuntimeError("no reachable public edges")
    candidates.sort(key=edge_sort_key)
    return candidates[0], candidates


def main():
    parser = argparse.ArgumentParser(description="Pick the best shardd public edge and send a request.")
    parser.add_argument("--bootstrap", action="append", required=True, help="Bootstrap edge base URL.")
    parser.add_argument("--api-key", required=True, help="Bearer API key.")
    parser.add_argument("--bucket", required=True)
    parser.add_argument("--account", default="main")
    parser.add_argument("--amount", type=int, default=10)
    parser.add_argument("--write", action="store_true", help="POST /events instead of GET /balances.")
    args = parser.parse_args()

    selected, candidates = choose_edge(args.bootstrap)
    print(json.dumps({"selected_edge": selected, "candidates": candidates}, indent=2))

    if args.write:
        payload = {
            "bucket": args.bucket,
            "account": args.account,
            "amount": args.amount,
            "note": "python example",
        }
        response, _ = http_json(
            f"{selected['base_url']}/events",
            api_key=args.api_key,
            method="POST",
            body=payload,
            timeout=5.0,
        )
    else:
        query = urllib.parse.urlencode({"bucket": args.bucket})
        response, _ = http_json(
            f"{selected['base_url']}/balances?{query}",
            api_key=args.api_key,
            timeout=5.0,
        )
    print(json.dumps(response, indent=2))


if __name__ == "__main__":
    main()
