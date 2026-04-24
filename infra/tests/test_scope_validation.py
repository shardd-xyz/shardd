from __future__ import annotations

import json
import os
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse


INFRA_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(INFRA_DIR))

import scope_validation


MACHINE_SECRET = "machine-secret-for-tests"


def auth_decision(api_key: str, action: str, bucket: str) -> dict[str, object]:
    if api_key == "key-exact":
        if bucket == "orders":
            return {
                "valid": True,
                "allowed": True,
                "user_id": "user-1",
                "cache_ttl_ms": 60000,
                "denial_reason": None,
                "matched_scope": {
                    "match_type": "exact",
                    "resource_value": "orders",
                },
            }
        return {
            "valid": True,
            "allowed": False,
            "user_id": "user-1",
            "cache_ttl_ms": 0,
            "denial_reason": "scope_denied",
            "matched_scope": None,
        }
    if api_key == "key-prefix-read":
        if bucket.startswith("orders/") and action == "read":
            return {
                "valid": True,
                "allowed": True,
                "user_id": "user-2",
                "cache_ttl_ms": 60000,
                "denial_reason": None,
                "matched_scope": {
                    "match_type": "prefix",
                    "resource_value": "orders/",
                },
            }
        return {
            "valid": True,
            "allowed": False,
            "user_id": "user-2",
            "cache_ttl_ms": 0,
            "denial_reason": "scope_denied",
            "matched_scope": None,
        }
    if api_key == "revoked":
        return {
            "valid": False,
            "allowed": False,
            "user_id": "user-3",
            "cache_ttl_ms": 0,
            "denial_reason": "api_key_revoked",
            "matched_scope": None,
        }
    return {
        "valid": False,
        "allowed": False,
        "user_id": None,
        "cache_ttl_ms": 0,
        "denial_reason": "invalid_api_key",
        "matched_scope": None,
    }


class ScopeValidationHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        parsed = urlparse(self.path)
        body = self._read_json()
        if parsed.path == "/api/machine/introspect":
            if self.headers.get("x-machine-auth-secret") != MACHINE_SECRET:
                self._write_json(403, {"error": "invalid machine auth secret"})
                return
            decision = auth_decision(
                str(body["api_key"]),
                str(body["action"]),
                str(body["bucket"]),
            )
            self._write_json(200, {"decision": decision})
            return

        if parsed.path.endswith("/events"):
            api_key = self._bearer_token()
            decision = auth_decision(api_key, "write", str(body["bucket"]))
            if not decision["valid"]:
                self._write_json(401, {"error": decision["denial_reason"]})
                return
            if not decision["allowed"]:
                self._write_json(403, {"error": decision["denial_reason"]})
                return
            note = str(body.get("note") or "")
            if len(note) > 4096:
                self._write_json(400, {"error": "note must be at most 4096 characters"})
                return
            self._write_json(201, {"ok": True})
            return

        self._write_json(404, {"error": "not_found"})

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        if parsed.path.endswith("/balances"):
            bucket = parse_qs(parsed.query).get("bucket", [""])[0]
            api_key = self._bearer_token()
            decision = auth_decision(api_key, "read", bucket)
            if not decision["valid"]:
                self._write_json(401, {"error": decision["denial_reason"]})
                return
            if not decision["allowed"]:
                self._write_json(403, {"error": decision["denial_reason"]})
                return
            self._write_json(200, {"balances": []})
            return

        self._write_json(404, {"error": "not_found"})

    def log_message(self, format: str, *args: object) -> None:
        return

    def _read_json(self) -> dict[str, object]:
        length = int(self.headers.get("content-length", "0") or "0")
        raw = self.rfile.read(length).decode("utf-8")
        return json.loads(raw or "{}")

    def _bearer_token(self) -> str:
        header = self.headers.get("Authorization", "")
        return header.removeprefix("Bearer ").strip()

    def _write_json(self, status: int, payload: dict[str, object]) -> None:
        encoded = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


class ScopeValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), ScopeValidationHandler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base_url = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.thread.join(timeout=5)
        cls.server.server_close()

    def test_validate_cases_against_live_like_endpoints(self) -> None:
        os.environ["TEST_SCOPE_EXACT_KEY"] = "key-exact"
        self.addCleanup(lambda: os.environ.pop("TEST_SCOPE_EXACT_KEY", None))

        cases_path = self._write_cases(
            {
                "cases": [
                    {
                        "name": "exact-allowed",
                        "api_key_env": "TEST_SCOPE_EXACT_KEY",
                        "bucket": "orders",
                        "expect_read": True,
                        "expect_write": True,
                        "expect_match_type": "exact",
                        "expect_resource_value": "orders",
                    },
                    {
                        "name": "prefix-read-only",
                        "api_key": "key-prefix-read",
                        "bucket": "orders/eu",
                        "expect_read": True,
                        "expect_write": False,
                        "expect_match_type": "prefix",
                        "expect_resource_value": "orders/",
                        "expect_denial_reason": "scope_denied",
                    },
                    {
                        "name": "outside-prefix-denied",
                        "api_key": "key-prefix-read",
                        "bucket": "payments/eu",
                        "expect_read": False,
                        "expect_write": False,
                        "expect_denial_reason": "scope_denied",
                    },
                    {
                        "name": "revoked",
                        "api_key": "revoked",
                        "bucket": "orders",
                        "expect_valid": False,
                        "expect_read": False,
                        "expect_write": False,
                        "expect_denial_reason": "api_key_revoked",
                    },
                ]
            }
        )

        rows = scope_validation.validate_cases(
            scope_validation.load_cases(cases_path),
            dashboard_url=self.base_url,
            machine_secret=MACHINE_SECRET,
            edge_urls={
                "use1": f"{self.base_url}/use1",
                "euc1": f"{self.base_url}/euc1",
            },
            timeout=2,
        )

        self.assertEqual(len(rows), 24)
        self.assertIn(
            {
                "case": "exact-allowed",
                "action": "read",
                "target": "gateway",
                "edge": "use1",
                "expected": "200",
                "observed": "200",
                "result": "ok",
            },
            rows,
        )
        self.assertTrue(
            any(
                row["case"] == "exact-allowed"
                and row["action"] == "write"
                and row["target"] == "dashboard"
                and "match=exact:orders" in row["observed"]
                for row in rows
            )
        )

    def test_validate_cases_rejects_mismatched_expectation(self) -> None:
        cases_path = self._write_cases(
            [
                {
                    "name": "wrong-write-expectation",
                    "api_key": "key-prefix-read",
                    "bucket": "orders/eu",
                    "expect_read": True,
                    "expect_write": True,
                    "expect_match_type": "prefix",
                    "expect_resource_value": "orders/",
                }
            ]
        )

        with self.assertRaises(scope_validation.ScopeValidationError):
            scope_validation.validate_cases(
                scope_validation.load_cases(cases_path),
                dashboard_url=self.base_url,
                machine_secret=MACHINE_SECRET,
                edge_urls={"use1": f"{self.base_url}/use1"},
                timeout=2,
            )

    def test_load_cases_requires_an_expectation(self) -> None:
        cases_path = self._write_cases(
            [
                {
                    "name": "missing-expectations",
                    "api_key": "key-exact",
                    "bucket": "orders",
                }
            ]
        )

        with self.assertRaises(scope_validation.ScopeValidationError):
            scope_validation.load_cases(cases_path)

    def _write_cases(self, payload: object) -> Path:
        handle = tempfile.NamedTemporaryFile("w", delete=False, suffix=".json")
        self.addCleanup(lambda: Path(handle.name).unlink(missing_ok=True))
        with handle:
            json.dump(payload, handle)
        return Path(handle.name)


if __name__ == "__main__":
    unittest.main()
