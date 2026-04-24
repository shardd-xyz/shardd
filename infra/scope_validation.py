from __future__ import annotations

import json
import os
import re
import subprocess
import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Any

MAX_WRITE_PROBE_NOTE_CHARS = 4097
WRITE_PROBE_ERROR_TEXT = "note must be at most 4096 characters"
CURL_USER_AGENT = "curl/8.12.1"


class ScopeValidationError(RuntimeError):
    pass


@dataclass(frozen=True)
class ScopeValidationCase:
    name: str
    bucket: str
    api_key: str | None = None
    api_key_env: str | None = None
    expect_valid: bool = True
    expect_read: bool | None = None
    expect_write: bool | None = None
    expect_match_type: str | None = None
    expect_resource_value: str | None = None
    expect_denial_reason: str | None = None
    edges: tuple[str, ...] | None = None

    def actions(self) -> list[tuple[str, bool]]:
        actions: list[tuple[str, bool]] = []
        if self.expect_read is not None:
            actions.append(("read", self.expect_read))
        if self.expect_write is not None:
            actions.append(("write", self.expect_write))
        return actions


def load_cases(path: str | Path) -> list[ScopeValidationCase]:
    raw = json.loads(Path(path).read_text())
    entries = raw.get("cases") if isinstance(raw, dict) else raw
    if not isinstance(entries, list):
        raise ScopeValidationError("scope validation cases must be a JSON array or an object with a `cases` array")
    return [_parse_case(entry, index) for index, entry in enumerate(entries)]


def validate_cases(
    cases: list[ScopeValidationCase],
    *,
    dashboard_url: str,
    machine_secret: str,
    edge_urls: dict[str, str],
    timeout: float = 15.0,
) -> list[dict[str, str]]:
    if not machine_secret.strip():
        raise ScopeValidationError("machine auth secret is empty")

    rows: list[dict[str, str]] = []
    for case in cases:
        api_key = resolve_api_key(case)
        selected_edges = _select_edges(case, edge_urls)
        for action, expected_allowed in case.actions():
            decision = introspect_scope(
                dashboard_url=dashboard_url,
                machine_secret=machine_secret,
                api_key=api_key,
                action=action,
                bucket=case.bucket,
                timeout=timeout,
            )
            _validate_decision(case, action, expected_allowed, decision)
            rows.append(
                {
                    "case": case.name,
                    "action": action,
                    "target": "dashboard",
                    "edge": "-",
                    "expected": _format_expected_decision(case, expected_allowed),
                    "observed": _format_observed_decision(decision),
                    "result": "ok",
                }
            )

            if action == "read":
                expected_status = expected_gateway_status(case.expect_valid, expected_allowed, action=action)
                for edge_id, edge_url in selected_edges.items():
                    status, body = probe_public_read(
                        edge_url=edge_url,
                        api_key=api_key,
                        bucket=case.bucket,
                        timeout=timeout,
                    )
                    _validate_gateway_response(
                        case,
                        action,
                        edge_id,
                        expected_status,
                        status,
                        body,
                    )
                    rows.append(
                        {
                            "case": case.name,
                            "action": action,
                            "target": "gateway",
                            "edge": edge_id,
                            "expected": str(expected_status),
                            "observed": str(status),
                            "result": "ok",
                        }
                    )
            else:
                expected_status = expected_gateway_status(case.expect_valid, expected_allowed, action=action)
                for edge_id, edge_url in selected_edges.items():
                    status, body = probe_public_write(
                        edge_url=edge_url,
                        api_key=api_key,
                        bucket=case.bucket,
                        account=_write_probe_account(case.name),
                        timeout=timeout,
                    )
                    _validate_gateway_response(
                        case,
                        action,
                        edge_id,
                        expected_status,
                        status,
                        body,
                    )
                    rows.append(
                        {
                            "case": case.name,
                            "action": action,
                            "target": "gateway",
                            "edge": edge_id,
                            "expected": str(expected_status),
                            "observed": str(status),
                            "result": "ok",
                        }
                    )

    return rows


def resolve_api_key(case: ScopeValidationCase) -> str:
    if case.api_key:
        return case.api_key
    if not case.api_key_env:
        raise ScopeValidationError(f"case `{case.name}` is missing `api_key` or `api_key_env`")
    value = os.environ.get(case.api_key_env, "").strip()
    if not value:
        raise ScopeValidationError(
            f"case `{case.name}` references api_key_env `{case.api_key_env}`, but it is not set"
        )
    return value


def expected_gateway_status(expect_valid: bool, expect_allowed: bool, *, action: str) -> int:
    if not expect_valid:
        return 401
    if not expect_allowed:
        return 403
    if action == "write":
        return 400
    return 200


def introspect_scope(
    *,
    dashboard_url: str,
    machine_secret: str,
    api_key: str,
    action: str,
    bucket: str,
    timeout: float,
) -> dict[str, Any]:
    status, body = _request_json(
        "POST",
        f"{dashboard_url.rstrip('/')}/api/machine/introspect",
        headers={"x-machine-auth-secret": machine_secret},
        payload={
            "api_key": api_key,
            "action": action,
            "bucket": bucket,
        },
        timeout=timeout,
    )
    if status != 200:
        raise ScopeValidationError(f"dashboard introspect returned HTTP {status}: {_body_error_text(body) or body}")
    if not isinstance(body, dict) or not isinstance(body.get("decision"), dict):
        raise ScopeValidationError("dashboard introspect returned an invalid JSON body")
    return body["decision"]


def probe_public_read(
    *,
    edge_url: str,
    api_key: str,
    bucket: str,
    timeout: float,
) -> tuple[int, Any]:
    query = urllib.parse.urlencode({"bucket": bucket})
    return _request_json(
        "GET",
        f"{edge_url.rstrip('/')}/balances?{query}",
        headers={"Authorization": f"Bearer {api_key}"},
        timeout=timeout,
    )


def probe_public_write(
    *,
    edge_url: str,
    api_key: str,
    bucket: str,
    account: str,
    timeout: float,
) -> tuple[int, Any]:
    return _request_json(
        "POST",
        f"{edge_url.rstrip('/')}/events",
        headers={"Authorization": f"Bearer {api_key}"},
        payload={
            "bucket": bucket,
            "account": account,
            "amount": 1,
            "note": "x" * MAX_WRITE_PROBE_NOTE_CHARS,
        },
        timeout=timeout,
    )


def _request_json(
    method: str,
    url: str,
    *,
    headers: dict[str, str] | None = None,
    payload: dict[str, Any] | None = None,
    timeout: float,
) -> tuple[int, Any]:
    request_headers = {"Accept": "application/json"}
    if headers:
        request_headers.update(headers)
    command = [
        "curl",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        str(timeout),
        "--request",
        method,
        "--user-agent",
        CURL_USER_AGENT,
        "--write-out",
        "\n%{http_code}",
        url,
    ]
    for key, value in request_headers.items():
        command.extend(["--header", f"{key}: {value}"])
    if payload is not None:
        command.extend(["--header", "Content-Type: application/json"])
        command.extend(["--data-binary", json.dumps(payload)])

    try:
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as error:
        raise ScopeValidationError("curl is required for scope validation but is not installed") from error

    if result.returncode != 0:
        detail = (result.stderr or result.stdout or f"curl exit {result.returncode}").strip()
        raise ScopeValidationError(f"request to {url} failed: {detail}")

    text, status_text = result.stdout.rsplit("\n", 1)
    status = int(status_text)
    if not text:
        return status, None
    try:
        return status, json.loads(text)
    except json.JSONDecodeError:
        return status, text


def _validate_decision(
    case: ScopeValidationCase,
    action: str,
    expected_allowed: bool,
    decision: dict[str, Any],
) -> None:
    valid = bool(decision.get("valid"))
    allowed = bool(decision.get("allowed"))
    if valid != case.expect_valid:
        raise ScopeValidationError(
            f"case `{case.name}` action `{action}` expected valid={case.expect_valid}, got {valid}"
        )
    if allowed != expected_allowed:
        raise ScopeValidationError(
            f"case `{case.name}` action `{action}` expected allowed={expected_allowed}, got {allowed}"
        )

    matched_scope = decision.get("matched_scope")
    if expected_allowed:
        if not isinstance(matched_scope, dict):
            raise ScopeValidationError(
                f"case `{case.name}` action `{action}` should have matched a scope, but none was returned"
            )
        if not decision.get("user_id"):
            raise ScopeValidationError(
                f"case `{case.name}` action `{action}` was allowed, but no user_id was returned"
            )
        if case.expect_match_type is not None and matched_scope.get("match_type") != case.expect_match_type:
            raise ScopeValidationError(
                f"case `{case.name}` action `{action}` expected match_type={case.expect_match_type}, "
                f"got {matched_scope.get('match_type')}"
            )
        if (
            case.expect_resource_value is not None
            and matched_scope.get("resource_value") != case.expect_resource_value
        ):
            raise ScopeValidationError(
                f"case `{case.name}` action `{action}` expected resource_value={case.expect_resource_value}, "
                f"got {matched_scope.get('resource_value')}"
            )
        return

    if matched_scope is not None:
        raise ScopeValidationError(
            f"case `{case.name}` action `{action}` should not have matched a scope, but got {matched_scope}"
        )
    if case.expect_denial_reason is not None and decision.get("denial_reason") != case.expect_denial_reason:
        raise ScopeValidationError(
            f"case `{case.name}` action `{action}` expected denial_reason={case.expect_denial_reason}, "
            f"got {decision.get('denial_reason')}"
        )


def _validate_gateway_response(
    case: ScopeValidationCase,
    action: str,
    edge_id: str,
    expected_status: int,
    actual_status: int,
    body: Any,
) -> None:
    if actual_status != expected_status:
        raise ScopeValidationError(
            f"case `{case.name}` action `{action}` edge `{edge_id}` expected HTTP {expected_status}, "
            f"got {actual_status}: {_body_error_text(body) or body}"
        )
    if action == "write" and expected_status == 400:
        error_text = _body_error_text(body)
        if WRITE_PROBE_ERROR_TEXT not in error_text:
            raise ScopeValidationError(
                f"case `{case.name}` action `{action}` edge `{edge_id}` expected write probe error "
                f"`{WRITE_PROBE_ERROR_TEXT}`, got `{error_text or body}`"
            )


def _select_edges(case: ScopeValidationCase, edge_urls: dict[str, str]) -> dict[str, str]:
    if case.edges is None:
        return dict(edge_urls)
    selected: dict[str, str] = {}
    for edge_id in case.edges:
        if edge_id not in edge_urls:
            raise ScopeValidationError(
                f"case `{case.name}` requested unknown edge `{edge_id}`; available: {', '.join(sorted(edge_urls)) or '(none)'}"
            )
        selected[edge_id] = edge_urls[edge_id]
    return selected


def _parse_case(entry: Any, index: int) -> ScopeValidationCase:
    if not isinstance(entry, dict):
        raise ScopeValidationError(f"scope validation case #{index + 1} must be an object")

    name = str(entry.get("name") or f"case-{index + 1}").strip()
    bucket = str(entry.get("bucket") or "").strip()
    if not bucket:
        raise ScopeValidationError(f"scope validation case `{name}` is missing `bucket`")

    api_key = _optional_string(entry, "api_key")
    api_key_env = _optional_string(entry, "api_key_env")
    if not api_key and not api_key_env:
        raise ScopeValidationError(f"scope validation case `{name}` is missing `api_key` or `api_key_env`")

    expect_valid = _optional_bool(entry, "expect_valid", default=True)
    expect_read = _optional_bool(entry, "expect_read", default=None)
    expect_write = _optional_bool(entry, "expect_write", default=None)
    if expect_read is None and expect_write is None:
        raise ScopeValidationError(
            f"scope validation case `{name}` must set `expect_read`, `expect_write`, or both"
        )
    if not expect_valid and (expect_read or expect_write):
        raise ScopeValidationError(
            f"scope validation case `{name}` cannot allow read/write while `expect_valid` is false"
        )

    expect_match_type = _optional_string(entry, "expect_match_type")
    if expect_match_type is not None and expect_match_type not in {"all", "exact", "prefix"}:
        raise ScopeValidationError(
            f"scope validation case `{name}` has invalid expect_match_type `{expect_match_type}`"
        )
    expect_resource_value = _optional_string(entry, "expect_resource_value")
    expect_denial_reason = _optional_string(entry, "expect_denial_reason")

    edges_value = entry.get("edges")
    if edges_value is None:
        edges = None
    elif isinstance(edges_value, list):
        edges = tuple(str(value) for value in edges_value)
    else:
        raise ScopeValidationError(f"scope validation case `{name}` has non-array `edges`")

    return ScopeValidationCase(
        name=name,
        bucket=bucket,
        api_key=api_key,
        api_key_env=api_key_env,
        expect_valid=expect_valid,
        expect_read=expect_read,
        expect_write=expect_write,
        expect_match_type=expect_match_type,
        expect_resource_value=expect_resource_value,
        expect_denial_reason=expect_denial_reason,
        edges=edges,
    )


def _optional_string(entry: dict[str, Any], key: str) -> str | None:
    value = entry.get(key)
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def _optional_bool(entry: dict[str, Any], key: str, *, default: bool | None) -> bool | None:
    if key not in entry:
        return default
    value = entry[key]
    if value is None:
        return None
    if not isinstance(value, bool):
        raise ScopeValidationError(f"scope validation field `{key}` must be a boolean")
    return value


def _body_error_text(body: Any) -> str:
    if isinstance(body, dict):
        error = body.get("error")
        if error is not None:
            return str(error)
    if isinstance(body, str):
        return body
    return ""


def _format_expected_decision(case: ScopeValidationCase, expected_allowed: bool) -> str:
    parts = [
        f"valid={case.expect_valid}",
        f"allowed={expected_allowed}",
    ]
    if expected_allowed and case.expect_match_type is not None:
        parts.append(f"match={case.expect_match_type}")
    if expected_allowed and case.expect_resource_value is not None:
        parts.append(f"resource={case.expect_resource_value}")
    if not expected_allowed and case.expect_denial_reason is not None:
        parts.append(f"denial={case.expect_denial_reason}")
    return " ".join(parts)


def _format_observed_decision(decision: dict[str, Any]) -> str:
    matched_scope = decision.get("matched_scope")
    if isinstance(matched_scope, dict):
        match_text = f"{matched_scope.get('match_type')}:{matched_scope.get('resource_value')}"
    else:
        match_text = "-"
    return " ".join(
        [
            f"valid={bool(decision.get('valid'))}",
            f"allowed={bool(decision.get('allowed'))}",
            f"denial={decision.get('denial_reason') or '-'}",
            f"match={match_text}",
        ]
    )


def _write_probe_account(name: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
    return f"scope-validation-{slug or 'probe'}"
