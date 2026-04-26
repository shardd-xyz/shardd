#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
import copy
import hashlib
import ipaddress
import json
import os
import secrets
import shlex
import shutil
import scope_validation
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT_DIR = Path(__file__).resolve().parent.parent
INFRA_DIR = ROOT_DIR / "infra"
STATE_DIR = INFRA_DIR / "state"
BUNDLES_DIR = INFRA_DIR / "bundles"
SCRIPTS_DIR = INFRA_DIR / "scripts"
TERRAFORM_DIR = INFRA_DIR / "terraform"
TERRAFORM_STATE_ROOT = STATE_DIR / "terraform"
DEFAULT_CLUSTER_PATH = STATE_DIR / "cluster.json"
DEFAULT_MACHINES_PATH = STATE_DIR / "machines.json"


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def fail(message: str) -> "NoReturn":
    print(f"error: {message}", file=sys.stderr, flush=True)
    raise SystemExit(1)


def info(message: str) -> None:
    print(f"==> {message}", flush=True)


def expand_path(value: str | Path) -> Path:
    return Path(value).expanduser().resolve()


def load_json_file(path: Path, *, default: dict[str, Any] | None = None) -> dict[str, Any]:
    if not path.exists():
        if default is not None:
            return copy.deepcopy(default)
        fail(f"state file not found: {path} (run `./run state init` first)")
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError as error:
        fail(f"invalid JSON in {path}: {error}")


def save_json_file(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    tmp.replace(path)


def run_command(
    args: list[str],
    *,
    cwd: Path | None = None,
    capture: bool = False,
    check: bool = True,
    input_text: str | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            args,
            cwd=str(cwd) if cwd else None,
            input=input_text,
            text=True,
            capture_output=capture,
            check=False,
            env=env,
        )
    except FileNotFoundError:
        fail(f"required local tool is not installed or not on PATH: {args[0]}")
    if check and result.returncode != 0:
        rendered = " ".join(shlex.quote(part) for part in args)
        if capture:
            stderr = (result.stderr or "").strip()
            stdout = (result.stdout or "").strip()
            detail = stderr or stdout or f"exit code {result.returncode}"
            fail(f"command failed: {rendered}\n{detail}")
        fail(f"command failed: {rendered} (exit code {result.returncode})")
    return result


def run_json_command(
    args: list[str], *, cwd: Path | None = None, env: dict[str, str] | None = None
) -> Any:
    result = run_command(args, capture=True, cwd=cwd, env=env)
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        rendered = " ".join(shlex.quote(part) for part in args)
        fail(f"command returned invalid JSON: {rendered}\n{error}\n{result.stdout}")


def random_secret(length: int = 32) -> str:
    return secrets.token_urlsafe(length)


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def table(rows: list[dict[str, Any]], columns: list[tuple[str, str]]) -> str:
    widths: dict[str, int] = {}
    for key, title in columns:
        widths[key] = len(title)
    for row in rows:
        for key, _title in columns:
            widths[key] = max(widths[key], len(str(row.get(key, ""))))
    header = "  ".join(title.ljust(widths[key]) for key, title in columns)
    separator = "  ".join("-" * widths[key] for key, _title in columns)
    body = [
        "  ".join(str(row.get(key, "")).ljust(widths[key]) for key, _title in columns)
        for row in rows
    ]
    return "\n".join([header, separator, *body])


@dataclass(frozen=True)
class ImageBuildSpec:
    key: str
    tag: str
    context: Path
    dockerfile: Path
    build_args: dict[str, str] = field(default_factory=dict)


IMAGE_SPECS: dict[str, ImageBuildSpec] = {
    "shardd-node": ImageBuildSpec(
        key="shardd-node",
        tag="local/shardd-node:infra",
        context=ROOT_DIR,
        dockerfile=ROOT_DIR / "apps/node/Dockerfile",
    ),
    "shardd-gateway": ImageBuildSpec(
        key="shardd-gateway",
        tag="local/shardd-gateway:infra",
        context=ROOT_DIR,
        dockerfile=ROOT_DIR / "apps/gateway/Dockerfile",
    ),
    "shardd-dashboard": ImageBuildSpec(
        key="shardd-dashboard",
        tag="local/shardd-dashboard:infra",
        context=ROOT_DIR,
        dockerfile=ROOT_DIR / "apps/dashboard/Dockerfile",
    ),
    "shardd-billing": ImageBuildSpec(
        key="shardd-billing",
        tag="local/shardd-billing:infra",
        context=ROOT_DIR,
        dockerfile=ROOT_DIR / "apps/billing/Dockerfile",
    ),
    # Static landing site (vitepress SSG → caddy:2-alpine). Build context
    # is apps/landing/ specifically so the image is independent of the
    # rest of the workspace and doesn't need .git in context.
    # GIT_SHA is injected via build-arg (the vitepress config picks it
    # up via $GITHUB_SHA for the footer "build <sha>" link).
    "shardd-landing": ImageBuildSpec(
        key="shardd-landing",
        tag="local/shardd-landing:infra",
        context=ROOT_DIR / "apps" / "landing",
        dockerfile=ROOT_DIR / "apps" / "landing" / "Dockerfile",
    ),
}


def resolve_image_version(override: str | None) -> str:
    """Deploy-time tag suffix. Explicit override wins; else git SHA (with a
    `-dirty` mark when the tree has uncommitted changes); else fall back to
    a timestamped dev tag so the push always has a unique name."""
    if override:
        return override
    try:
        sha = subprocess.run(
            ["git", "rev-parse", "--short=10", "HEAD"],
            cwd=str(ROOT_DIR),
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        status = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=str(ROOT_DIR),
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        return f"{sha}-dirty" if status.strip() else sha
    except (subprocess.CalledProcessError, FileNotFoundError):
        return f"dev-{int(time.time())}"


def registry_image_ref(spec: ImageBuildSpec, registry: str, version: str) -> str:
    """Fully qualified image reference pushed to the private tailnet registry,
    e.g. `100.104.178.26:5000/shardd-node:abc1234567`."""
    name = spec.tag.rsplit("/", 1)[-1].split(":", 1)[0]
    return f"{registry}/{name}:{version}"


def compose_image_env_var(image_key: str) -> str:
    """Env var name the compose templates look up to get an image ref, e.g.
    `SHARDD_NODE_IMAGE` for the `shardd-node` bundle key."""
    return image_key.upper().replace("-", "_") + "_IMAGE"


@dataclass(frozen=True)
class BundleSpec:
    name: str
    template_dir: Path
    image_keys: tuple[str, ...]

    def public_ports(self, service_vars: dict[str, Any]) -> list[int]:
        if self.name == "full-node":
            return [int(service_vars.get("libp2p_port", 9000))]
        if self.name in {"edge-node", "dashboard"}:
            return [80, 443]
        fail(f"bundle port rules not implemented for {self.name}")


BUNDLE_SPECS: dict[str, BundleSpec] = {
    "full-node": BundleSpec("full-node", BUNDLES_DIR / "full-node", ("shardd-node",)),
    "edge-node": BundleSpec("edge-node", BUNDLES_DIR / "edge-node", ("shardd-gateway",)),
    "dashboard": BundleSpec(
        "dashboard",
        BUNDLES_DIR / "dashboard",
        ("shardd-dashboard", "shardd-billing", "shardd-landing"),
    ),
}


def load_cluster(path: Path) -> dict[str, Any]:
    cluster = load_json_file(path)
    if cluster.get("version") != 1:
        fail(f"unsupported cluster config version in {path}: {cluster.get('version')}")
    if "deployments" not in cluster:
        fail(f"cluster config missing top-level `deployments`: {path}")
    return cluster


def load_machines(path: Path) -> dict[str, Any]:
    machines = load_json_file(path, default={"version": 1, "machines": {}})
    if machines.get("version") != 1:
        fail(f"unsupported machines state version in {path}: {machines.get('version')}")
    machines.setdefault("machines", {})
    return machines


def terraform_state_dir(deployment_name: str) -> Path:
    return TERRAFORM_STATE_ROOT / deployment_name


def terraform_backend_path(deployment_name: str) -> Path:
    return terraform_state_dir(deployment_name) / "terraform.tfstate"


def terraform_tf_data_dir(deployment_name: str) -> Path:
    return terraform_state_dir(deployment_name) / ".terraform"


def terraform_vars_path(deployment_name: str) -> Path:
    return terraform_state_dir(deployment_name) / "terraform.tfvars.json"


def lookup_named_env(env_name: str | None, description: str, *, strict: bool = True) -> str:
    if not env_name:
        if strict:
            fail(f"missing environment mapping for {description}")
        return ""
    value = os.environ.get(env_name)
    if value is None or value == "":
        if strict:
            fail(f"required environment variable is not set for {description}: {env_name}")
        return ""
    return value


def cloudflare_zone_name(deployment: dict[str, Any]) -> str:
    return normalize_dns_name(
        str(deployment.get("cloudflare_zone_name") or deployment.get("dns_root_zone") or "")
    )


def cloudflare_api_token(deployment: dict[str, Any], *, strict: bool = True) -> str:
    return lookup_named_env(
        deployment.get("cloudflare_api_token_env"),
        "Cloudflare API token",
        strict=strict,
    )


def cloudflare_zone_id(deployment: dict[str, Any], *, strict: bool = True) -> str:
    return lookup_named_env(
        deployment.get("cloudflare_zone_id_env"),
        "Cloudflare zone id",
        strict=strict,
    )


def cloudflare_account_id(deployment: dict[str, Any], *, strict: bool = True) -> str:
    return lookup_named_env(
        deployment.get("cloudflare_account_id_env"),
        "Cloudflare account id",
        strict=strict,
    )


def get_deployment(cluster: dict[str, Any], deployment_name: str) -> dict[str, Any]:
    deployments = cluster.get("deployments", {})
    if deployment_name not in deployments:
        fail(
            f"deployment `{deployment_name}` not found in cluster config; "
            f"available: {', '.join(sorted(deployments)) or '(none)'}"
        )
    deployment = deployments[deployment_name]
    if "machines" not in deployment:
        fail(f"deployment `{deployment_name}` is missing `machines`")
    return deployment


def ensure_machine_record(
    machines_state: dict[str, Any],
    deployment_name: str,
    machine_name: str,
    machine_def: dict[str, Any],
) -> dict[str, Any]:
    record = machines_state.setdefault("machines", {}).setdefault(machine_name, {})
    record.setdefault("deployment", deployment_name)
    record.setdefault("provider", machine_def["provider"])
    record.setdefault("region", machine_def["region"])
    record.setdefault("ssh", copy.deepcopy(machine_def.get("ssh", {})))
    record.setdefault("remote_root", machine_def.get("remote_root", "/opt/shardd"))
    record.setdefault("setup", {})
    record.setdefault("deployments", {})
    record.setdefault("generated_secrets", {})
    return record


def select_machine_names(
    deployment: dict[str, Any],
    *,
    names: list[str] | None = None,
    provider: str | None = None,
) -> list[str]:
    wanted = names or list(deployment["machines"].keys())
    unknown = [name for name in wanted if name not in deployment["machines"]]
    if unknown:
        fail(f"unknown machine(s) in deployment: {', '.join(unknown)}")
    selected = []
    for name in wanted:
        machine_def = deployment["machines"][name]
        if provider and machine_def["provider"] != provider:
            continue
        selected.append(name)
    return selected


def ssh_target(machine_state: dict[str, Any], machine_def: dict[str, Any]) -> str:
    host = (
        machine_state.get("host")
        or machine_state.get("public_ip")
        or machine_state.get("public_dns")
        or machine_state.get("private_ip")
    )
    if not host:
        fail("machine host is unknown; create/sync the server first")
    user = machine_def.get("ssh", {}).get("user", "ubuntu")
    return f"{user}@{host}"


def ssh_base_args(machine_state: dict[str, Any], machine_def: dict[str, Any]) -> list[str]:
    ssh_cfg = machine_def.get("ssh", {})
    args = ["ssh"]
    port = ssh_cfg.get("port")
    if port:
        args.extend(["-p", str(port)])
    identity = ssh_cfg.get("identity_file")
    if identity:
        args.extend(["-i", str(expand_path(identity))])
    args.extend(
        [
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ServerAliveInterval=30",
            ssh_target(machine_state, machine_def),
        ]
    )
    return args


def rsync_base_args(machine_state: dict[str, Any], machine_def: dict[str, Any]) -> list[str]:
    ssh_cfg = machine_def.get("ssh", {})
    ssh_bits = ["ssh"]
    port = ssh_cfg.get("port")
    if port:
        ssh_bits.extend(["-p", str(port)])
    identity = ssh_cfg.get("identity_file")
    if identity:
        ssh_bits.extend(["-i", str(expand_path(identity))])
    ssh_bits.extend(["-o", "StrictHostKeyChecking=accept-new"])
    return ["rsync", "-az", "--delete", "-e", " ".join(shlex.quote(bit) for bit in ssh_bits)]


def ssh_run(
    machine_state: dict[str, Any],
    machine_def: dict[str, Any],
    remote_command: str,
    *,
    capture: bool = False,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    args = ssh_base_args(machine_state, machine_def)
    args.append(remote_command)
    return run_command(args, capture=capture, input_text=input_text)


def ssh_run_script(
    machine_state: dict[str, Any],
    machine_def: dict[str, Any],
    script_path: Path,
    script_args: list[str],
    *,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    remote = "bash -s -- " + " ".join(shlex.quote(arg) for arg in script_args)
    return ssh_run(
        machine_state,
        machine_def,
        remote,
        capture=capture,
        input_text=script_path.read_text(),
    )


def rsync_to(
    machine_state: dict[str, Any],
    machine_def: dict[str, Any],
    source: str | Path,
    remote_dest: str,
    *,
    delete: bool = False,
) -> None:
    args = rsync_base_args(machine_state, machine_def)
    if not delete:
        args.remove("--delete")
    if isinstance(source, Path):
        source_arg = f"{source}/" if source.is_dir() else str(source)
    else:
        source_arg = source
    args.extend([source_arg, f"{ssh_target(machine_state, machine_def)}:{remote_dest}"])
    run_command(args)


def infer_multiaddr(host: str, port: int) -> str:
    try:
        ip_obj = ipaddress.ip_address(host)
    except ValueError:
        return f"/dns4/{host}/tcp/{port}"
    if ip_obj.version == 6:
        return f"/ip6/{host}/tcp/{port}"
    return f"/ip4/{host}/tcp/{port}"


def node_advertise_addrs(
    machine_state: dict[str, Any],
    public_host: str,
    port: int,
) -> list[str]:
    """Multiaddrs a node should advertise. libp2p dials these in parallel, so
    listing every reachable path lets same-region peers pick the AWS private
    IP (lowest RTT), other tailnet members pick the Tailscale IP, and
    everyone else fall back to the public address."""
    addrs = [infer_multiaddr(public_host, port)]
    private_ip = machine_state.get("private_ip")
    if private_ip:
        priv = infer_multiaddr(private_ip, port)
        if priv not in addrs:
            addrs.append(priv)
    ts_ip = machine_state.get("setup", {}).get("tailscale_ipv4")
    if ts_ip:
        ts = infer_multiaddr(ts_ip, port)
        if ts not in addrs:
            addrs.append(ts)
    return addrs


def machine_public_host(machine_state: dict[str, Any]) -> str | None:
    return machine_state.get("public_ip") or machine_state.get("public_dns") or machine_state.get("host")


def normalize_dns_name(value: str) -> str:
    return value.strip().rstrip(".").lower()


def hostname_from_urlish(value: str | None) -> str | None:
    if not value:
        return None
    text = value.strip()
    if not text or text in {"http://:80", "https://:443", ":80", ":443"}:
        return None
    if "://" in text:
        parsed = urlparse(text)
        if not parsed.hostname:
            return None
        return normalize_dns_name(parsed.hostname)
    host = text.split("/", 1)[0]
    if host.startswith(":"):
        return None
    if ":" in host:
        host = host.rsplit(":", 1)[0]
    return normalize_dns_name(host) if host else None


def is_full_node_machine(machine_def: dict[str, Any]) -> bool:
    return any(service.get("bundle") == "full-node" for service in machine_def.get("services", []))


def bootstrap_peers(
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    current_machine_name: str,
    current_bundle: str,
    *,
    allow_placeholders: bool = False,
) -> list[str]:
    peers: list[str] = []
    for machine_name, machine_def in deployment["machines"].items():
        if not is_full_node_machine(machine_def):
            continue
        if current_bundle == "full-node" and machine_name == current_machine_name:
            continue
        machine_state = machines_state.get("machines", {}).get(machine_name, {})
        public_host = machine_public_host(machine_state)
        if not public_host:
            if not allow_placeholders:
                continue
            public_host = f"{machine_name}.planned.invalid"
        for service in machine_def.get("services", []):
            if service.get("bundle") != "full-node":
                continue
            port = int(service.get("vars", {}).get("libp2p_port", 9000))
            # Emit every reachable path for this peer. libp2p dials them in
            # parallel on bootstrap; same-region peers establish over the
            # private IP first, others fall back to Tailscale or public.
            peers.extend(node_advertise_addrs(machine_state, public_host, port))
    return peers


def lookup_secret_env(deployment: dict[str, Any], logical_name: str, *, strict: bool = True) -> str:
    env_name = deployment.get("secret_env", {}).get(logical_name)
    if not env_name:
        if strict:
            fail(f"deployment missing secret_env mapping for `{logical_name}`")
        return f"<missing-secret-env:{logical_name}>"
    value = os.environ.get(env_name)
    if value is None or value == "":
        if strict:
            fail(f"required secret environment variable is not set: {env_name}")
        return f"<env:{env_name}>"
    return value


def lookup_cluster_key(deployment: dict[str, Any], *, strict: bool = True) -> str:
    env_name = deployment.get("cluster_key_env")
    if not env_name:
        if strict:
            fail("deployment missing `cluster_key_env`")
        return "<missing-cluster-key-env>"
    value = os.environ.get(env_name)
    if value is None or value == "":
        if strict:
            fail(f"required cluster key environment variable is not set: {env_name}")
        return f"<env:{env_name}>"
    return value


def infra_ssh_public_key_path(deployment: dict[str, Any]) -> Path:
    path = deployment.get("infra_ssh_public_key_path")
    if not path:
        fail("deployment missing `infra_ssh_public_key_path`")
    return expand_path(path)


def infra_ssh_private_key_path(deployment: dict[str, Any]) -> Path:
    public_path = infra_ssh_public_key_path(deployment)
    if public_path.suffix != ".pub":
        fail(f"infra ssh public key path must end with .pub: {public_path}")
    return public_path.with_suffix("")


def ssh_public_key_lines_from_file(path: Path) -> list[str]:
    if not path.exists():
        return []
    lines: list[str] = []
    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line:
            continue
        if not (
            line.startswith("ssh-")
            or line.startswith("ecdsa-")
            or line.startswith("sk-")
        ):
            continue
        lines.append(line)
    return lines


def collect_authorized_public_keys(deployment: dict[str, Any]) -> tuple[str, list[Path]]:
    public_path = infra_ssh_public_key_path(deployment)
    if not public_path.exists():
        fail(
            f"infra ssh public key not found: {public_path}. "
            "Run `./run state ensure-ssh-key --deployment <name>` first."
        )

    seen: set[str] = set()
    keys: list[str] = []
    sources: list[Path] = []

    def add_keys(path: Path) -> None:
        nonlocal keys, seen, sources
        for line in ssh_public_key_lines_from_file(path):
            if line in seen:
                continue
            seen.add(line)
            keys.append(line)
            if path not in sources:
                sources.append(path)

    add_keys(public_path)

    ssh_dir = Path.home() / ".ssh"
    if ssh_dir.exists():
        for pub_path in sorted(ssh_dir.glob("*.pub")):
            add_keys(pub_path)

    if not keys:
        fail("no SSH public keys found to install on remote host")
    return ("\n".join(keys) + "\n"), sources


def read_infra_ssh_key_b64(deployment: dict[str, Any]) -> str:
    key_path = infra_ssh_public_key_path(deployment)
    if not key_path.exists():
        fail(f"infra ssh public key not found: {key_path}")
    return base64.b64encode(key_path.read_bytes()).decode("ascii")


def machine_service_name(machine_name: str, machine_def: dict[str, Any], index: int, service: dict[str, Any]) -> str:
    if service.get("name"):
        return service["name"]
    if len(machine_def.get("services", [])) == 1:
        return machine_name
    return f"{machine_name}-{service['bundle']}-{index + 1}"


def ensure_generated_secret(machine_state: dict[str, Any], key: str, *, length: int = 24) -> str:
    store = machine_state.setdefault("generated_secrets", {})
    if key not in store or not store[key]:
        store[key] = random_secret(length)
    return store[key]


def bundle_spec(name: str) -> BundleSpec:
    spec = BUNDLE_SPECS.get(name)
    if not spec:
        fail(f"unsupported bundle: {name}")
    return spec


def render_env_file(env_values: dict[str, Any]) -> str:
    lines = []
    for key in sorted(env_values):
        value = str(env_values[key])
        lines.append(f"{key}={value}")
    return "\n".join(lines) + "\n"


def merge_site_addresses(*values: str | None) -> str:
    seen: set[str] = set()
    merged: list[str] = []
    for value in values:
        if not value:
            continue
        for raw_part in str(value).split(","):
            part = raw_part.strip()
            if not part or part in seen:
                continue
            seen.add(part)
            merged.append(part)
    return ", ".join(merged)


def build_service_env(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    machine_name: str,
    machine_def: dict[str, Any],
    machine_state: dict[str, Any],
    service_index: int,
    service: dict[str, Any],
    *,
    strict: bool,
    image_refs: dict[str, str] | None = None,
) -> dict[str, Any]:
    bundle_name = service["bundle"]
    service_vars = service.get("vars", {})
    service_name = machine_service_name(machine_name, machine_def, service_index, service)

    env_values: dict[str, Any] = {
        "SERVICE_NAME": service_name,
    }

    # Image refs — the compose templates read $SHARDD_NODE_IMAGE etc., whether
    # that points at the tailnet registry (new transport) or the legacy
    # `local/shardd-X:infra` tag (tar transport fallback).
    for image_key in bundle_spec(bundle_name).image_keys:
        ref = (image_refs or {}).get(image_key) or IMAGE_SPECS[image_key].tag
        env_values[compose_image_env_var(image_key)] = ref

    if bundle_name == "dashboard":
        # Registry service binds to the host's tailscale IP only. Null out
        # if the probe hasn't captured it yet; compose will fail loudly so
        # the operator knows to re-run servers:setup first.
        env_values["REGISTRY_BIND_IP"] = (
            machine_state.get("setup", {}).get("tailscale_ipv4", "") or ""
        )

    if bundle_name == "full-node":
        public_host = machine_public_host(machine_state)
        if not public_host:
            if strict:
                fail(
                    f"machine {machine_name} has no public host; "
                    "run `./run infra:apply` or `./run infra:output` first"
                )
            public_host = f"{machine_name}.planned.invalid"
        libp2p_port = int(service_vars.get("libp2p_port", 9000))
        env_values.update(
            {
                "POSTGRES_PASSWORD": ensure_generated_secret(machine_state, f"{service_name}.postgres_password"),
                "SHARDD_CLUSTER_KEY": lookup_cluster_key(deployment, strict=strict),
                "RUST_LOG": service_vars.get("rust_log", "info"),
                "LIBP2P_PORT": libp2p_port,
                "ADVERTISE_ADDRS_CSV": ",".join(
                    node_advertise_addrs(machine_state, public_host, libp2p_port)
                ),
                "BATCH_FLUSH_INTERVAL_MS": int(service_vars.get("batch_flush_interval_ms", 100)),
                "BATCH_FLUSH_SIZE": int(service_vars.get("batch_flush_size", 1000)),
                "MATVIEW_REFRESH_MS": int(service_vars.get("matview_refresh_ms", 5000)),
                "ORPHAN_CHECK_INTERVAL_MS": int(service_vars.get("orphan_check_interval_ms", 500)),
                "ORPHAN_AGE_MS": int(service_vars.get("orphan_age_ms", 500)),
                "HOLD_MULTIPLIER": int(service_vars.get("hold_multiplier", 5)),
                "HOLD_DURATION_MS": int(service_vars.get("hold_duration_ms", 600000)),
                "EVENT_WORKER_COUNT": int(service_vars.get("event_worker_count", 4)),
                "BOOTSTRAP_PEERS_CSV": ",".join(
                    bootstrap_peers(
                        deployment,
                        machines_state,
                        machine_name,
                        bundle_name,
                        allow_placeholders=not strict,
                    )
                ),
            }
        )
        return env_values

    if bundle_name == "edge-node":
        dashboard_url = service_vars.get("dashboard_url")
        if not dashboard_url:
            fail(f"edge-node service `{service_name}` is missing vars.dashboard_url")
        site_address = merge_site_addresses(service_vars.get("site_address", "http://:80"))
        env_values.update(
            {
                "SHARDD_CLUSTER_KEY": lookup_cluster_key(deployment, strict=strict),
                "SHARDD_DASHBOARD_URL": dashboard_url,
                "SHARDD_DASHBOARD_MACHINE_AUTH_SECRET": lookup_secret_env(
                    deployment, "dashboard_machine_auth_secret", strict=strict
                ),
                "RUST_LOG": service_vars.get("rust_log", "info"),
                "BOOTSTRAP_PEERS_CSV": ",".join(
                    bootstrap_peers(
                        deployment,
                        machines_state,
                        machine_name,
                        bundle_name,
                        allow_placeholders=not strict,
                    )
                ),
                "REQUEST_TIMEOUT_MS": int(service_vars.get("request_timeout_ms", 5000)),
                "DISCOVERY_TIMEOUT_MS": int(service_vars.get("discovery_timeout_ms", 5000)),
                "TOP_K": int(service_vars.get("top_k", 3)),
                "MAX_SYNC_GAP": int(service_vars.get("max_sync_gap", 64)),
                "SITE_ADDRESS": site_address,
            }
        )
        public_edges = deployment.get("public_edges", [])
        if public_edges and not isinstance(public_edges, list):
            fail("deployment.public_edges must be a list when set")
        public_edge_id = service_vars.get("public_edge_id")
        if public_edge_id:
            match = next(
                (
                    edge
                    for edge in public_edges
                    if isinstance(edge, dict) and str(edge.get("edge_id", "")) == str(public_edge_id)
                ),
                None,
            )
            if not match:
                fail(
                    f"edge-node service `{service_name}` configured public_edge_id={public_edge_id}, "
                    "but deployment.public_edges has no matching entry"
                )
            base_url = str(match.get("base_url", "")).strip()
            if not base_url:
                fail(
                    f"deployment.public_edges entry for public_edge_id={public_edge_id} is missing base_url"
                )
            env_values["SHARDD_PUBLIC_EDGE_ID"] = str(public_edge_id)
            env_values["SHARDD_PUBLIC_EDGE_REGION"] = str(
                match.get("region") or machine_def.get("region", "")
            )
            env_values["SHARDD_PUBLIC_BASE_URL"] = base_url
        if public_edges:
            env_values["SHARDD_PUBLIC_EDGES_JSON"] = json.dumps(public_edges)
        return env_values

    if bundle_name == "dashboard":
        app_origin = service_vars.get("app_origin")
        email_from = service_vars.get("email_from")
        if not app_origin:
            fail(f"dashboard service `{service_name}` is missing vars.app_origin")
        if not email_from:
            fail(f"dashboard service `{service_name}` is missing vars.email_from")
        env_values.update(
            {
                "POSTGRES_PASSWORD": ensure_generated_secret(machine_state, f"{service_name}.postgres_password"),
                "JWT_SECRET": lookup_secret_env(deployment, "dashboard_jwt_secret", strict=strict),
                "RESEND_API_KEY": lookup_secret_env(
                    deployment, "dashboard_resend_api_key", strict=strict
                ),
                "EMAIL_FROM": email_from,
                "APP_ORIGIN": app_origin,
                "RUST_LOG": service_vars.get("rust_log", "info"),
                "ADMIN_EMAILS": service_vars.get("admin_emails", ""),
                "IMPERSONATION_TTL_MINUTES": int(service_vars.get("impersonation_ttl_minutes", 60)),
                "MACHINE_AUTH_SHARED_SECRET": lookup_secret_env(
                    deployment, "dashboard_machine_auth_secret", strict=strict
                ),
                "SHARDD_PUBLIC_EDGES_JSON": json.dumps(deployment.get("public_edges", [])),
                "GOOGLE_CLIENT_ID": lookup_secret_env(deployment, "google_client_id", strict=False),
                "GOOGLE_CLIENT_SECRET": lookup_secret_env(deployment, "google_client_secret", strict=False),
                "STRIPE_SECRET_KEY": lookup_secret_env(deployment, "stripe_secret_key", strict=False),
                "STRIPE_WEBHOOK_SECRET": lookup_secret_env(deployment, "stripe_webhook_secret", strict=False),
                "BILLING_INTERNAL_SECRET": lookup_secret_env(
                    deployment, "billing_internal_secret", strict=strict
                ),
                "SITE_ADDRESS": service_vars.get("site_address", "http://:80"),
            }
        )
        return env_values

    fail(f"unsupported bundle env rendering: {bundle_name}")


def render_service_bundle(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    machine_name: str,
    machine_def: dict[str, Any],
    machine_state: dict[str, Any],
    service_index: int,
    service: dict[str, Any],
    image_ids: dict[str, str],
    destination: Path,
    *,
    strict: bool,
    image_refs: dict[str, str] | None = None,
) -> tuple[str, str, list[str]]:
    bundle_name = service["bundle"]
    spec = bundle_spec(bundle_name)
    env_values = build_service_env(
        deployment_name,
        deployment,
        machines_state,
        machine_name,
        machine_def,
        machine_state,
        service_index,
        service,
        strict=strict,
        image_refs=image_refs,
    )
    service_name = env_values["SERVICE_NAME"]
    service_dir = destination / service_name
    service_dir.mkdir(parents=True, exist_ok=True)

    compose_src = spec.template_dir / "compose.yml"
    shutil.copy2(compose_src, service_dir / "compose.yml")
    caddy_src = spec.template_dir / "Caddyfile"
    if caddy_src.exists():
        shutil.copy2(caddy_src, service_dir / "Caddyfile")

    for extra in spec.template_dir.iterdir():
        if extra.name in ("compose.yml", "Caddyfile"):
            continue
        if extra.is_file():
            shutil.copy2(extra, service_dir / extra.name)
        elif extra.is_dir():
            dst = service_dir / extra.name
            if dst.exists():
                shutil.rmtree(dst)
            shutil.copytree(extra, dst)

    ui_src = ROOT_DIR / "apps" / "dashboard" / "assets"
    if bundle_name == "dashboard" and ui_src.exists():
        ui_dst = service_dir / "ui"
        if ui_dst.exists():
            shutil.rmtree(ui_dst)
        shutil.copytree(ui_src, ui_dst)

    env_text = render_env_file(env_values)
    (service_dir / ".env").write_text(env_text)
    manifest = {
        "deployment": deployment_name,
        "machine": machine_name,
        "service_name": service_name,
        "bundle": bundle_name,
        "image_ids": {image_key: image_ids.get(image_key, "") for image_key in spec.image_keys},
        "rendered_at": utc_now(),
    }
    manifest_text = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    (service_dir / "bundle-manifest.json").write_text(manifest_text)
    revision_hash = hashlib.sha256()
    revision_hash.update((service_dir / "compose.yml").read_bytes())
    if (service_dir / "Caddyfile").exists():
        revision_hash.update((service_dir / "Caddyfile").read_bytes())
    revision_hash.update(env_text.encode("utf-8"))
    revision_hash.update(manifest_text.encode("utf-8"))
    return service_name, revision_hash.hexdigest(), list(spec.image_keys)


def inspect_local_image_ids(image_keys: set[str]) -> dict[str, str]:
    image_ids: dict[str, str] = {}
    for image_key in image_keys:
        spec = IMAGE_SPECS[image_key]
        result = run_command(
            ["docker", "image", "inspect", spec.tag, "--format", "{{.Id}}"],
            capture=True,
            check=False,
        )
        if result.returncode == 0:
            image_ids[image_key] = result.stdout.strip()
    return image_ids


def build_images(image_keys: set[str], version: str | None = None) -> dict[str, str]:
    """Build each requested image. `version` (when provided) is forwarded
    as a `GIT_SHA` build-arg to every image — Dockerfiles that don't
    declare `ARG GIT_SHA` ignore it. Used by shardd-landing to inject
    the commit hash that ends up in the footer's "build <sha>" link
    (the `.git` directory isn't in the build context, so we can't run
    `git rev-parse` inside the build)."""
    built_ids: dict[str, str] = {}
    for image_key in sorted(image_keys):
        spec = IMAGE_SPECS[image_key]
        if not spec.context.exists():
            fail(f"build context not found for {image_key}: {spec.context}")
        if not spec.dockerfile.exists():
            fail(f"Dockerfile not found for {image_key}: {spec.dockerfile}")
        info(f"building {spec.tag}")
        cmd = [
            "docker",
            "build",
            "-t",
            spec.tag,
            "-f",
            str(spec.dockerfile),
        ]
        build_args = dict(spec.build_args)
        if version and "GIT_SHA" not in build_args:
            build_args["GIT_SHA"] = version
        for key, value in build_args.items():
            cmd.extend(["--build-arg", f"{key}={value}"])
        cmd.append(str(spec.context))
        run_command(cmd)
        built_ids.update(inspect_local_image_ids({image_key}))
    return built_ids


def push_images(image_keys: set[str], registry: str, version: str) -> dict[str, str]:
    """`docker tag` + `docker push` each built image to the private registry,
    plus a `:latest` alias at the same digest so humans browsing the catalog
    see a stable name. Returns image_key → full registry ref."""
    pushed: dict[str, str] = {}
    for image_key in sorted(image_keys):
        spec = IMAGE_SPECS[image_key]
        ref = registry_image_ref(spec, registry, version)
        latest_ref = registry_image_ref(spec, registry, "latest")
        info(f"pushing {ref}")
        run_command(["docker", "tag", spec.tag, ref])
        run_command(["docker", "tag", spec.tag, latest_ref])
        run_command(["docker", "push", ref])
        run_command(["docker", "push", latest_ref])
        pushed[image_key] = ref
    return pushed


def pull_remote_images(
    machine_state: dict[str, Any],
    machine_def: dict[str, Any],
    image_refs: dict[str, str],
) -> None:
    """Tell the target host to pull each required image from the private
    registry. libp2p-era distribution: one push from the builder, N layer-diff
    pulls on the fleet via the tailnet."""
    for image_key, ref in image_refs.items():
        ssh_run(
            machine_state,
            machine_def,
            f"docker pull {shlex.quote(ref)}",
        )


def save_image_archives(image_keys: set[str], destination: Path) -> dict[str, Path]:
    archives: dict[str, Path] = {}
    destination.mkdir(parents=True, exist_ok=True)
    for image_key in sorted(image_keys):
        spec = IMAGE_SPECS[image_key]
        archive_path = destination / f"{image_key}.tar"
        info(f"saving {spec.tag} -> {archive_path}")
        with archive_path.open("wb") as archive_file:
            proc = subprocess.Popen(
                ["docker", "save", spec.tag],
                stdout=archive_file,
                stderr=subprocess.PIPE,
                text=False,
            )
            _stdout, stderr = proc.communicate()
            if proc.returncode != 0:
                fail(f"docker save failed for {spec.tag}: {(stderr or b'').decode().strip()}")
        archives[image_key] = archive_path
    return archives


def service_ports(machine_def: dict[str, Any]) -> list[int]:
    ports: set[int] = {22}
    for service in machine_def.get("services", []):
        spec = bundle_spec(service["bundle"])
        ports.update(spec.public_ports(service.get("vars", {})))
    return sorted(ports)


def machine_dns_names(machine_def: dict[str, Any]) -> list[str]:
    """Bare hostname list for a machine. Used by firewall/SSH/etc.;
    callers that need the per-record proxied flag should use
    `machine_dns_records()` instead."""
    return [rec["name"] for rec in machine_dns_records(machine_def)]


def machine_dns_records(machine_def: dict[str, Any]) -> list[dict[str, Any]]:
    """All public DNS hostnames for a machine, paired with each
    record's `proxied` flag. Default `proxied` is "true if any service
    on this machine is the dashboard bundle, else false" (matches the
    original behaviour). `extra_dns_names` may add hostnames; each
    entry is either a bare string (inherits the default `proxied`) or
    a `{"name": ..., "proxied": bool}` dict for per-record overrides
    — required for cases like the apex `shardd.xyz` where ACME
    HTTP-01 needs a brief DNS-only window before flipping to
    Cloudflare-proxied.

    See cluster.json:
      "extra_dns_names": [
          { "name": "shardd.xyz", "proxied": true },
          "www.shardd.xyz"
      ]
    """
    default_proxied = any(
        service.get("bundle") == "dashboard" for service in machine_def.get("services", [])
    )

    records: list[dict[str, Any]] = []
    seen: set[str] = set()

    def add(raw_value: str | Any, proxied: bool) -> None:
        host = hostname_from_urlish(raw_value if isinstance(raw_value, str) else None)
        if not host or host in seen:
            return
        seen.add(host)
        records.append({"name": host, "proxied": bool(proxied)})

    add(machine_def.get("public_dns_name"), default_proxied)
    for service in machine_def.get("services", []):
        service_vars = service.get("vars", {})
        add(service_vars.get("site_address"), default_proxied)
        if service.get("bundle") == "dashboard":
            add(service_vars.get("app_origin"), default_proxied)

    for entry in machine_def.get("extra_dns_names", []) or []:
        if isinstance(entry, str):
            add(entry, default_proxied)
        elif isinstance(entry, dict):
            name = entry.get("name")
            proxied = entry.get("proxied", default_proxied)
            add(name, bool(proxied))
        else:
            fail(
                f"extra_dns_names entries must be strings or {{'name': ..., 'proxied': bool}} dicts, got {type(entry).__name__}"
            )
    return records


def edge_api_machine_names(deployment: dict[str, Any]) -> set[str] | None:
    configured = deployment.get("edge_api_machines")
    if configured is None:
        return None
    if not isinstance(configured, list):
        fail("deployment.edge_api_machines must be a list when set")
    return {str(name) for name in configured}


def selected_edge_api_machine_names(deployment: dict[str, Any]) -> list[str]:
    configured = deployment.get("edge_api_machines")
    if configured is None:
        selected: list[str] = []
        for machine_name, machine_def in deployment["machines"].items():
            is_edge = any(service.get("bundle") == "edge-node" for service in machine_def.get("services", []))
            if is_edge:
                selected.append(machine_name)
        return selected

    if not isinstance(configured, list):
        fail("deployment.edge_api_machines must be a list when set")

    selected = [str(name) for name in configured]
    invalid: list[str] = []
    for machine_name in selected:
        machine_def = deployment["machines"].get(machine_name)
        is_edge = machine_def is not None and any(
            service.get("bundle") == "edge-node" for service in machine_def.get("services", [])
        )
        if not is_edge:
            invalid.append(machine_name)
    if invalid:
        fail(f"deployment.edge_api_machines contains unknown or non-edge machines: {', '.join(sorted(invalid))}")
    return selected


def cloudflare_record_key(machine_name: str, hostname: str) -> str:
    digest = sha256_text(f"{machine_name}:{normalize_dns_name(hostname)}")[:10]
    return f"{machine_name}-{digest}"


def build_cloudflare_records(deployment: dict[str, Any]) -> dict[str, dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}
    for machine_name, machine_def in deployment["machines"].items():
        for entry in machine_dns_records(machine_def):
            records[cloudflare_record_key(machine_name, entry["name"])] = {
                "name": normalize_dns_name(entry["name"]),
                "machine_name": machine_name,
                "proxied": entry["proxied"],
            }
    return records


def edge_origin_hostname(machine_name: str, machine_def: dict[str, Any]) -> str:
    hostname = hostname_from_urlish(machine_def.get("public_dns_name"))
    if hostname:
        return hostname
    for service in machine_def.get("services", []):
        if service.get("bundle") != "edge-node":
            continue
        hostname = hostname_from_urlish(service.get("vars", {}).get("site_address"))
        if hostname:
            return hostname
    fail(f"edge machine {machine_name} is missing a DNS hostname")


def build_cloudflare_lb_origins(deployment: dict[str, Any]) -> tuple[dict[str, dict[str, Any]], list[str]]:
    selected_machine_names = selected_edge_api_machine_names(deployment)
    origins: dict[str, dict[str, Any]] = {}
    for machine_name in selected_machine_names:
        machine_def = deployment["machines"][machine_name]
        origins[machine_name] = {
            "machine_name": machine_name,
            "hostname": edge_origin_hostname(machine_name, machine_def),
        }
    return origins, selected_machine_names


def cloudflare_lb_monitor_config(deployment: dict[str, Any]) -> dict[str, Any]:
    configured = deployment.get("cloudflare_lb_monitor", {})
    return {
        "method": str(configured.get("method", "GET")),
        "path": str(configured.get("path", "/gateway/health")),
        "port": int(configured.get("port", 443)),
        "expected_codes": str(configured.get("expected_codes", "200")),
        "timeout_seconds": int(configured.get("timeout_seconds", 5)),
        "interval_seconds": int(configured.get("interval_seconds", 60)),
        "retries": int(configured.get("retries", 2)),
    }


def build_terraform_machine_config(
    deployment: dict[str, Any], machine_name: str, machine_def: dict[str, Any]
) -> dict[str, Any]:
    if machine_def.get("provider") != "aws":
        fail(f"Terraform infra currently supports only AWS machines; got {machine_def.get('provider')} for {machine_name}")

    provider_defaults = deployment.get("provider_defaults", {}).get("aws", {})
    provider_config = machine_def.get("provider_config", {})
    ami = provider_config.get("ami") or provider_defaults.get("ami")
    instance_type = provider_config.get("instance_type") or provider_defaults.get("instance_type")
    volume_size_gb = provider_config.get("volume_size_gb") or provider_defaults.get("volume_size_gb")
    subnet_id = provider_config.get("subnet_id")
    key_name = provider_config.get("key_name")

    if not ami:
        fail(f"machine {machine_name} is missing provider_config.ami or provider_defaults.aws.ami")
    if not instance_type:
        fail(
            f"machine {machine_name} is missing provider_config.instance_type "
            "or provider_defaults.aws.instance_type"
        )
    if not volume_size_gb:
        fail(
            f"machine {machine_name} is missing provider_config.volume_size_gb "
            "or provider_defaults.aws.volume_size_gb"
        )
    if not subnet_id:
        fail(f"machine {machine_name} is missing provider_config.subnet_id")
    if not key_name:
        fail(f"machine {machine_name} is missing provider_config.key_name")

    return {
        "provider": "aws",
        "region": machine_def["region"],
        "public_dns_name": machine_def.get("public_dns_name", ""),
        "remote_root": machine_def.get("remote_root", "/opt/shardd"),
        "ssh_user": machine_def.get("ssh", {}).get("user", "ubuntu"),
        "ssh_port": int(machine_def.get("ssh", {}).get("port", 22)),
        "ssh_identity_file": machine_def.get("ssh", {}).get("identity_file", ""),
        "public_ports": [port for port in service_ports(machine_def) if port != 22],
        "services": copy.deepcopy(machine_def.get("services", [])),
        "provider_config": {
            "ami": str(ami),
            "instance_type": str(instance_type),
            "volume_size_gb": int(volume_size_gb),
            "subnet_id": str(subnet_id),
            "key_name": str(key_name),
        },
    }


def build_terraform_inputs(deployment_name: str, deployment: dict[str, Any], *, strict: bool) -> dict[str, Any]:
    zone_name = cloudflare_zone_name(deployment)
    if not zone_name:
        fail("deployment is missing dns_root_zone or cloudflare_zone_name")

    lb_enabled = bool(deployment.get("cloudflare_lb_enabled", bool(deployment.get("edge_api_dns_name"))))
    lb_origins, lb_origin_order = build_cloudflare_lb_origins(deployment)
    if lb_enabled and not lb_origin_order:
        fail("cloudflare_lb_enabled is true, but no edge machines are selected for edge_api_machines")

    return {
        "deployment_name": deployment_name,
        "display_name": str(deployment.get("display_name", deployment_name)),
        "expected_aws_account_id": str(deployment.get("expected_aws_account_id", "")),
        "dns_root_zone": zone_name,
        "cloudflare_zone_name": zone_name,
        "cloudflare_zone_id": cloudflare_zone_id(deployment, strict=strict),
        "cloudflare_account_id": cloudflare_account_id(deployment, strict=strict),
        "cloudflare_lb_enabled": lb_enabled,
        "edge_api_dns_name": str(deployment.get("edge_api_dns_name", "")),
        "cloudflare_lb_monitor": cloudflare_lb_monitor_config(deployment),
        "cloudflare_records": build_cloudflare_records(deployment),
        "cloudflare_lb_origins": lb_origins,
        "cloudflare_lb_origin_order": lb_origin_order,
        "infra_ssh_public_key": infra_ssh_public_key_path(deployment).read_text().strip(),
        "machines": {
            machine_name: build_terraform_machine_config(deployment, machine_name, machine_def)
            for machine_name, machine_def in deployment["machines"].items()
        },
    }


def relative_dns_name(name: str, zone_root: str) -> str:
    normalized_name = normalize_dns_name(name)
    normalized_zone = normalize_dns_name(zone_root)
    if normalized_name == normalized_zone:
        return "@"
    suffix = "." + normalized_zone
    if normalized_name.endswith(suffix):
        return normalized_name[: -len(suffix)]
    return normalized_name + "."


def probe_setup(
    deployment: dict[str, Any],
    machine_name: str,
    machine_def: dict[str, Any],
    machine_state: dict[str, Any],
) -> dict[str, Any]:
    key_b64 = read_infra_ssh_key_b64(deployment)
    result = ssh_run_script(
        machine_state,
        machine_def,
        SCRIPTS_DIR / "probe_host.sh",
        [machine_def.get("ssh", {}).get("user", "ubuntu"), key_b64, machine_name],
        capture=True,
    )
    try:
        probe = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"host probe returned invalid JSON for {machine_name}: {error}\n{result.stdout}")
    setup_state = machine_state.setdefault("setup", {})
    # Probe returns two kinds of fields: bool/int flags (feed `fully_setup`)
    # and string info like `tailscale_ipv4` (stored verbatim, not gated on).
    flag_keys = [k for k, v in probe.items() if not isinstance(v, str)]
    for key, value in probe.items():
        setup_state[key] = bool(value) if key in flag_keys else value
    setup_state["fully_setup"] = all(setup_state[k] for k in flag_keys)
    setup_state["last_checked_at"] = utc_now()
    return setup_state


def resolve_aws_ami(region: str, value: str) -> str:
    if value.startswith("resolve:ssm:"):
        parameter_name = value.split("resolve:ssm:", 1)[1]
        result = run_command(
            [
                "aws",
                "ssm",
                "get-parameter",
                "--region",
                region,
                "--name",
                parameter_name,
                "--query",
                "Parameter.Value",
                "--output",
                "text",
            ],
            capture=True,
        )
        return result.stdout.strip()
    return value


def aws_describe_instance_by_name(region: str, machine_name: str) -> dict[str, Any] | None:
    data = run_json_command(
        [
            "aws",
            "ec2",
            "describe-instances",
            "--region",
            region,
            "--output",
            "json",
            "--filters",
            f"Name=tag:Name,Values={machine_name}",
            "Name=tag:ManagedBy,Values=shardd-infra",
            "Name=instance-state-name,Values=pending,running,stopping,stopped",
        ]
    )
    instances: list[dict[str, Any]] = []
    for reservation in data.get("Reservations", []):
        instances.extend(reservation.get("Instances", []))
    if not instances:
        return None
    instances.sort(key=lambda item: item.get("LaunchTime", ""), reverse=True)
    return instances[0]


def aws_find_key_pair(region: str, key_name: str) -> dict[str, Any] | None:
    data = run_json_command(
        [
            "aws",
            "ec2",
            "describe-key-pairs",
            "--region",
            region,
            "--output",
            "json",
            "--filters",
            f"Name=key-name,Values={key_name}",
        ]
    )
    key_pairs = data.get("KeyPairs", [])
    if not key_pairs:
        return None
    return key_pairs[0]


def aws_describe_subnet(region: str, subnet_id: str) -> dict[str, Any]:
    data = run_json_command(
        [
            "aws",
            "ec2",
            "describe-subnets",
            "--region",
            region,
            "--output",
            "json",
            "--subnet-ids",
            subnet_id,
        ]
    )
    subnets = data.get("Subnets", [])
    if not subnets:
        fail(f"subnet not found in {region}: {subnet_id}")
    return subnets[0]


def aws_find_security_group(region: str, group_name: str, vpc_id: str) -> dict[str, Any] | None:
    data = run_json_command(
        [
            "aws",
            "ec2",
            "describe-security-groups",
            "--region",
            region,
            "--output",
            "json",
            "--filters",
            f"Name=group-name,Values={group_name}",
            f"Name=vpc-id,Values={vpc_id}",
        ]
    )
    groups = data.get("SecurityGroups", [])
    if not groups:
        return None
    return groups[0]


def aws_security_group_allows_cidr_port(
    security_group: dict[str, Any], *, port: int, cidr: str, protocol: str = "tcp"
) -> bool:
    for permission in security_group.get("IpPermissions", []):
        if permission.get("IpProtocol") != protocol:
            continue
        from_port = permission.get("FromPort")
        to_port = permission.get("ToPort")
        if from_port is None or to_port is None:
            continue
        if int(from_port) > port or int(to_port) < port:
            continue
        for ip_range in permission.get("IpRanges", []):
            if ip_range.get("CidrIp") == cidr:
                return True
    return False


def aws_ensure_security_group(
    *,
    region: str,
    vpc_id: str,
    group_name: str,
    description: str,
    tags: list[dict[str, str]],
) -> dict[str, Any]:
    existing = aws_find_security_group(region, group_name, vpc_id)
    if existing:
        return existing

    created = run_json_command(
        [
            "aws",
            "ec2",
            "create-security-group",
            "--region",
            region,
            "--output",
            "json",
            "--group-name",
            group_name,
            "--description",
            description,
            "--vpc-id",
            vpc_id,
            "--tag-specifications",
            json.dumps([{"ResourceType": "security-group", "Tags": tags}]),
        ]
    )
    group_id = created["GroupId"]
    security_group = run_json_command(
        [
            "aws",
            "ec2",
            "describe-security-groups",
            "--region",
            region,
            "--output",
            "json",
            "--group-ids",
            group_id,
        ]
    )["SecurityGroups"][0]
    return security_group


def aws_authorize_security_group_cidr_port(
    *, region: str, group_id: str, port: int, cidr: str, description: str
) -> None:
    result = run_command(
        [
            "aws",
            "ec2",
            "authorize-security-group-ingress",
            "--region",
            region,
            "--group-id",
            group_id,
            "--ip-permissions",
            json.dumps(
                [
                    {
                        "IpProtocol": "tcp",
                        "FromPort": port,
                        "ToPort": port,
                        "IpRanges": [{"CidrIp": cidr, "Description": description}],
                    }
                ]
            ),
        ],
        capture=True,
        check=False,
    )
    if result.returncode == 0:
        return
    detail = "\n".join(part.strip() for part in [result.stderr or "", result.stdout or ""] if part.strip())
    if "InvalidPermission.Duplicate" in detail:
        return
    fail(f"failed to authorize security-group ingress for {group_id} port {port} in {region}\n{detail}")


def update_machine_state_from_instance(
    machine_state: dict[str, Any],
    deployment_name: str,
    machine_def: dict[str, Any],
    instance: dict[str, Any],
) -> None:
    machine_state["deployment"] = deployment_name
    machine_state["provider"] = "aws"
    machine_state["provider_id"] = instance.get("InstanceId")
    machine_state["provider_state"] = instance.get("State", {}).get("Name")
    machine_state["region"] = machine_def["region"]
    machine_state["public_ip"] = instance.get("PublicIpAddress")
    machine_state["public_dns"] = instance.get("PublicDnsName")
    machine_state["private_ip"] = instance.get("PrivateIpAddress")
    machine_state["host"] = (
        instance.get("PublicIpAddress")
        or instance.get("PublicDnsName")
        or instance.get("PrivateIpAddress")
    )
    machine_state["ssh"] = copy.deepcopy(machine_def.get("ssh", {}))
    machine_state["remote_root"] = machine_def.get("remote_root", "/opt/shardd")
    machine_state["observed_at"] = utc_now()


def aws_get_caller_identity() -> dict[str, Any]:
    return run_json_command(["aws", "sts", "get-caller-identity", "--output", "json"])


def aws_export_credentials() -> dict[str, str]:
    exported = run_json_command(["aws", "configure", "export-credentials", "--format", "process"])
    access_key = str(exported.get("AccessKeyId", "")).strip()
    secret_key = str(exported.get("SecretAccessKey", "")).strip()
    session_token = str(exported.get("SessionToken", "")).strip()
    expiration = str(exported.get("Expiration", "")).strip()
    if not access_key or not secret_key:
        fail("aws configure export-credentials did not return access credentials")
    credentials = {
        "AWS_ACCESS_KEY_ID": access_key,
        "AWS_SECRET_ACCESS_KEY": secret_key,
    }
    if session_token:
        credentials["AWS_SESSION_TOKEN"] = session_token
    if expiration:
        credentials["AWS_CREDENTIAL_EXPIRATION"] = expiration
    return credentials


def verify_aws_account(deployment: dict[str, Any]) -> dict[str, Any]:
    expected = deployment.get("expected_aws_account_id")
    identity = aws_get_caller_identity()
    account_id = str(identity.get("Account", ""))
    if expected and str(expected) != account_id:
        fail(
            f"AWS account mismatch: expected {expected}, got {account_id} "
            f"({identity.get('Arn', 'unknown-arn')})"
        )
    return identity


def terraform_env(deployment_name: str, deployment: dict[str, Any]) -> dict[str, str]:
    env = os.environ.copy()
    env["TF_DATA_DIR"] = str(terraform_tf_data_dir(deployment_name))
    env["TF_VAR_cloudflare_api_token"] = cloudflare_api_token(deployment, strict=True)
    if not env.get("AWS_ACCESS_KEY_ID") or not env.get("AWS_SECRET_ACCESS_KEY"):
        env.update(aws_export_credentials())
    return env


def terraform_prepare(deployment_name: str, deployment: dict[str, Any], *, strict: bool) -> tuple[Path, dict[str, str]]:
    state_dir = terraform_state_dir(deployment_name)
    state_dir.mkdir(parents=True, exist_ok=True)

    vars_path = terraform_vars_path(deployment_name)
    save_json_file(vars_path, build_terraform_inputs(deployment_name, deployment, strict=strict))

    env = terraform_env(deployment_name, deployment)
    run_command(
        [
            "terraform",
            "init",
            "-input=false",
            "-reconfigure",
            f"-backend-config=path={terraform_backend_path(deployment_name)}",
        ],
        cwd=TERRAFORM_DIR,
        env=env,
    )
    return vars_path, env


def terraform_output_machine_records(deployment_name: str, deployment: dict[str, Any]) -> dict[str, Any]:
    env = terraform_env(deployment_name, deployment)
    output = run_json_command(
        ["terraform", "output", "-json", "machine_records"],
        cwd=TERRAFORM_DIR,
        env=env,
    )
    if not isinstance(output, dict):
        fail("terraform output machine_records did not return an object")
    return output


def sync_machine_records_from_terraform(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    terraform_machine_records: dict[str, Any],
) -> None:
    selected_machine_names = set(deployment["machines"].keys())
    for machine_name in selected_machine_names:
        machine_def = deployment["machines"][machine_name]
        record = ensure_machine_record(machines_state, deployment_name, machine_name, machine_def)
        terraform_record = terraform_machine_records.get(machine_name)
        if terraform_record is None:
            record["provider_state"] = "not-created"
            for key in ["provider_id", "public_ip", "public_dns", "private_ip", "host"]:
                record.pop(key, None)
            record["observed_at"] = utc_now()
            continue

        previous_provider_id = record.get("provider_id")
        record["provider"] = terraform_record.get("provider", machine_def["provider"])
        record["provider_id"] = terraform_record.get("provider_id")
        record["provider_state"] = terraform_record.get("provider_state", "running")
        record["region"] = terraform_record.get("region", machine_def["region"])
        record["public_ip"] = terraform_record.get("public_ip")
        record["public_dns"] = terraform_record.get("public_dns")
        record["private_ip"] = terraform_record.get("private_ip")
        record["host"] = terraform_record.get("host")
        record["ssh"] = copy.deepcopy(machine_def.get("ssh", {}))
        record["remote_root"] = machine_def.get("remote_root", "/opt/shardd")
        record["observed_at"] = utc_now()

        if previous_provider_id and previous_provider_id != record["provider_id"]:
            record["setup"] = {}
            record["deployments"] = {}


def clear_machine_records_for_deployment(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
) -> None:
    for machine_name in deployment["machines"].keys():
        record = machines_state.get("machines", {}).get(machine_name)
        if not record or record.get("deployment") != deployment_name:
            continue
        machines_state["machines"].pop(machine_name, None)


def command_infra_init(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    deployment = get_deployment(cluster, args.deployment)
    vars_path, _env = terraform_prepare(args.deployment, deployment, strict=True)
    info(f"wrote {vars_path}")
    info(f"initialized Terraform backend at {terraform_backend_path(args.deployment)}")


def command_infra_plan(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    deployment = get_deployment(cluster, args.deployment)
    identity = verify_aws_account(deployment)
    info(f"using AWS account {identity['Account']} ({identity['Arn']})")
    vars_path, env = terraform_prepare(args.deployment, deployment, strict=True)
    run_command(
        ["terraform", "plan", "-input=false", f"-var-file={vars_path}"],
        cwd=TERRAFORM_DIR,
        env=env,
    )


def command_infra_apply(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    identity = verify_aws_account(deployment)
    info(f"using AWS account {identity['Account']} ({identity['Arn']})")
    vars_path, env = terraform_prepare(args.deployment, deployment, strict=True)
    run_command(
        ["terraform", "apply", "-input=false", "-auto-approve", f"-var-file={vars_path}"],
        cwd=TERRAFORM_DIR,
        env=env,
    )
    sync_machine_records_from_terraform(
        args.deployment,
        deployment,
        machines_state,
        terraform_output_machine_records(args.deployment, deployment),
    )
    save_json_file(machines_path, machines_state)
    command_servers_list(
        argparse.Namespace(
            cluster_state=args.cluster_state,
            machines_state=args.machines_state,
            deployment=args.deployment,
            name=None,
        )
    )


def command_infra_destroy(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    identity = verify_aws_account(deployment)
    info(f"using AWS account {identity['Account']} ({identity['Arn']})")
    vars_path, env = terraform_prepare(args.deployment, deployment, strict=True)
    run_command(
        ["terraform", "destroy", "-input=false", "-auto-approve", f"-var-file={vars_path}"],
        cwd=TERRAFORM_DIR,
        env=env,
    )
    clear_machine_records_for_deployment(args.deployment, deployment, machines_state)
    save_json_file(machines_path, machines_state)
    command_servers_list(
        argparse.Namespace(
            cluster_state=args.cluster_state,
            machines_state=args.machines_state,
            deployment=args.deployment,
            name=None,
        )
    )


def command_infra_output(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    terraform_prepare(args.deployment, deployment, strict=True)
    machine_records = terraform_output_machine_records(args.deployment, deployment)
    sync_machine_records_from_terraform(args.deployment, deployment, machines_state, machine_records)
    save_json_file(machines_path, machines_state)
    command_servers_list(
        argparse.Namespace(
            cluster_state=args.cluster_state,
            machines_state=args.machines_state,
            deployment=args.deployment,
            name=None,
        )
    )


def command_state_init(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    if cluster_path.exists() and not args.force:
        fail(f"{cluster_path} already exists; pass --force to overwrite")
    if machines_path.exists() and not args.force:
        fail(f"{machines_path} already exists; pass --force to overwrite")
    shutil.copy2(STATE_DIR / "cluster.example.json", cluster_path)
    shutil.copy2(STATE_DIR / "machines.example.json", machines_path)
    info(f"wrote {cluster_path}")
    info(f"wrote {machines_path}")


def command_state_ensure_ssh_key(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    deployment = get_deployment(cluster, args.deployment)
    public_path = infra_ssh_public_key_path(deployment)
    private_path = infra_ssh_private_key_path(deployment)

    if public_path.exists() and not private_path.exists():
        fail(
            f"public key exists but private key is missing: {public_path} / {private_path}. "
            "Restore the private key or remove the public key and rerun."
        )

    if not public_path.exists():
        public_path.parent.mkdir(parents=True, exist_ok=True)
        comment = f"shardd-{args.deployment}-infra"
        run_command(
            [
                "ssh-keygen",
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                comment,
                "-f",
                str(private_path),
            ]
        )
        info(f"created {private_path}")
        info(f"created {public_path}")
    else:
        info(f"infra ssh key already exists: {private_path}")

    authorized_keys, sources = collect_authorized_public_keys(deployment)
    print(f"infra_private_key={private_path}")
    print(f"infra_public_key={public_path}")
    print(f"authorized_key_sources={','.join(str(path) for path in sources)}")
    print(f"authorized_key_count={len([line for line in authorized_keys.splitlines() if line.strip()])}")


def command_servers_list(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    machines_state = load_machines(expand_path(args.machines_state))
    deployment = get_deployment(cluster, args.deployment)

    rows: list[dict[str, Any]] = []
    for machine_name in select_machine_names(deployment, names=args.name):
        machine_def = deployment["machines"][machine_name]
        record = machines_state.get("machines", {}).get(machine_name, {})
        setup = record.get("setup", {})
        services = ",".join(service["bundle"] for service in machine_def.get("services", []))
        rows.append(
            {
                "name": machine_name,
                "provider": machine_def["provider"],
                "region": machine_def["region"],
                "host": record.get("host", "-"),
                "state": record.get("provider_state", "not-created"),
                "setup": "ready" if setup.get("fully_setup") else "pending",
                "services": services,
            }
        )
    print(
        table(
            rows,
            [
                ("name", "NAME"),
                ("provider", "PROVIDER"),
                ("region", "REGION"),
                ("host", "HOST"),
                ("state", "STATE"),
                ("setup", "SETUP"),
                ("services", "SERVICES"),
            ],
        )
    )


def command_servers_sync_aws(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    verify_aws_account(deployment)

    for machine_name in select_machine_names(deployment, names=args.name, provider="aws"):
        machine_def = deployment["machines"][machine_name]
        record = ensure_machine_record(machines_state, args.deployment, machine_name, machine_def)
        instance = aws_describe_instance_by_name(machine_def["region"], machine_name)
        if instance is None:
            record["provider_state"] = "not-found"
            record["observed_at"] = utc_now()
            continue
        update_machine_state_from_instance(record, args.deployment, machine_def, instance)
    save_json_file(machines_path, machines_state)
    command_servers_list(args)


def command_servers_ensure_sg_aws(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    cluster = load_cluster(cluster_path)
    deployment = get_deployment(cluster, args.deployment)
    identity = verify_aws_account(deployment)
    info(f"using AWS account {identity['Account']} ({identity['Arn']})")

    selected_names = select_machine_names(deployment, names=args.name, provider="aws")
    if not selected_names:
        fail("no AWS machines selected")

    group_specs: dict[tuple[str, str, str, tuple[int, ...]], dict[str, Any]] = {}
    display_name = str(deployment.get("display_name", args.deployment))
    for machine_name in selected_names:
        machine_def = deployment["machines"][machine_name]
        provider_config = machine_def.get("provider_config", {})
        subnet_id = provider_config.get("subnet_id")
        if not subnet_id:
            fail(f"machine {machine_name} is missing provider_config.subnet_id")
        subnet = aws_describe_subnet(machine_def["region"], str(subnet_id))
        vpc_id = subnet.get("VpcId")
        if not vpc_id:
            fail(f"subnet {subnet_id} in {machine_def['region']} has no VPC id")
        bundles = tuple(service.get("bundle", "service") for service in machine_def.get("services", []))
        role_key = "-".join(bundles) if bundles else "host"
        ports = tuple(service_ports(machine_def))
        sg_name = f"{display_name}-{machine_def['region']}-{role_key}"
        spec_key = (machine_def["region"], vpc_id, sg_name, ports)
        spec = group_specs.setdefault(
            spec_key,
            {
                "region": machine_def["region"],
                "vpc_id": vpc_id,
                "group_name": sg_name,
                "role_key": role_key,
                "ports": ports,
                "machine_names": [],
            },
        )
        spec["machine_names"].append(machine_name)

    for (_region, _vpc, _sg_name, _ports), spec in sorted(group_specs.items()):
        description = (
            f"Managed by shardd infra for {display_name} "
            f"{spec['role_key']} in {spec['region']}"
        )
        tags = [
            {"Key": "Project", "Value": "shardd"},
            {"Key": "Deployment", "Value": display_name},
            {"Key": "ManagedBy", "Value": "shardd-infra"},
            {"Key": "Role", "Value": spec["role_key"]},
        ]
        security_group = aws_ensure_security_group(
            region=spec["region"],
            vpc_id=spec["vpc_id"],
            group_name=spec["group_name"],
            description=description,
            tags=tags,
        )
        group_id = security_group["GroupId"]
        info(
            f"{spec['region']}: using security group {spec['group_name']} ({group_id}) "
            f"for {', '.join(spec['machine_names'])}"
        )
        for port in spec["ports"]:
            if aws_security_group_allows_cidr_port(security_group, port=port, cidr=args.cidr):
                continue
            info(f"{spec['region']}: allowing {args.cidr} -> tcp/{port} on {group_id}")
            aws_authorize_security_group_cidr_port(
                region=spec["region"],
                group_id=group_id,
                port=port,
                cidr=args.cidr,
                description=f"shardd {display_name} tcp/{port}",
            )
        for machine_name in spec["machine_names"]:
            provider_config = deployment["machines"][machine_name].setdefault("provider_config", {})
            provider_config["security_group_ids"] = [group_id]

    save_json_file(cluster_path, cluster)


def command_servers_create_aws(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    identity = verify_aws_account(deployment)
    info(f"using AWS account {identity['Account']} ({identity['Arn']})")

    for machine_name in select_machine_names(deployment, names=args.name, provider="aws"):
        machine_def = deployment["machines"][machine_name]
        record = ensure_machine_record(machines_state, args.deployment, machine_name, machine_def)

        existing = aws_describe_instance_by_name(machine_def["region"], machine_name)
        if existing and existing.get("State", {}).get("Name") != "terminated":
            info(f"{machine_name}: instance already exists, syncing instead of creating")
            update_machine_state_from_instance(record, args.deployment, machine_def, existing)
            continue

        provider_defaults = deployment.get("provider_defaults", {}).get("aws", {})
        provider_config = machine_def.get("provider_config", {})
        ami_value = provider_config.get("ami", provider_defaults.get("ami"))
        if not ami_value:
            fail(f"machine {machine_name} is missing provider_config.ami or provider default ami")
        ami = resolve_aws_ami(machine_def["region"], str(ami_value))
        subnet_id = provider_config.get("subnet_id")
        key_name = provider_config.get("key_name")
        security_group_ids = provider_config.get("security_group_ids") or []
        if not subnet_id or not key_name or not security_group_ids:
            fail(
                f"machine {machine_name} is missing subnet_id, key_name, or security_group_ids "
                "in provider_config"
            )

        instance_type = provider_config.get(
            "instance_type", provider_defaults.get("instance_type", "t3.small")
        )
        volume_size_gb = int(provider_config.get("volume_size_gb", provider_defaults.get("volume_size_gb", 30)))
        associate_public_ip = bool(provider_config.get("associate_public_ip", True))
        role = ",".join(service["bundle"] for service in machine_def.get("services", []))

        network_interfaces = [
            {
                "DeviceIndex": 0,
                "AssociatePublicIpAddress": associate_public_ip,
                "SubnetId": subnet_id,
                "Groups": security_group_ids,
            }
        ]
        tag_specifications = [
            {
                "ResourceType": "instance",
                "Tags": [
                    {"Key": "Name", "Value": machine_name},
                    {"Key": "Project", "Value": "shardd"},
                    {"Key": "Deployment", "Value": deployment.get("display_name", args.deployment)},
                    {"Key": "ManagedBy", "Value": "shardd-infra"},
                    {"Key": "Role", "Value": role},
                ],
            }
        ]
        block_mappings = [
            {
                "DeviceName": provider_config.get("root_device_name", "/dev/sda1"),
                "Ebs": {
                    "VolumeType": provider_config.get("volume_type", "gp3"),
                    "VolumeSize": volume_size_gb,
                    "DeleteOnTermination": True,
                },
            }
        ]

        info(f"creating {machine_name} in {machine_def['region']}")
        run_instances_cmd = [
            "aws",
            "ec2",
            "run-instances",
            "--region",
            machine_def["region"],
            "--output",
            "json",
            "--image-id",
            ami,
            "--instance-type",
            str(instance_type),
            "--count",
            "1",
            "--key-name",
            str(key_name),
            "--network-interfaces",
            json.dumps(network_interfaces),
            "--block-device-mappings",
            json.dumps(block_mappings),
            "--tag-specifications",
            json.dumps(tag_specifications),
        ]
        if args.dry_run:
            run_instances_cmd.append("--dry-run")
            result = run_command(run_instances_cmd, capture=True, check=False)
            stderr = (result.stderr or "").strip()
            stdout = (result.stdout or "").strip()
            detail = "\n".join(part for part in [stderr, stdout] if part)
            if "DryRunOperation" in detail:
                info(f"{machine_name}: dry-run succeeded in {machine_def['region']}")
                record["last_dry_run_at"] = utc_now()
                continue
            fail(f"{machine_name}: dry-run failed\n{detail or f'exit code {result.returncode}'}")

        created = run_json_command(run_instances_cmd)
        instance_id = created["Instances"][0]["InstanceId"]
        run_command(
            ["aws", "ec2", "wait", "instance-running", "--region", machine_def["region"], "--instance-ids", instance_id]
        )
        run_command(
            ["aws", "ec2", "wait", "instance-status-ok", "--region", machine_def["region"], "--instance-ids", instance_id]
        )

        instance = aws_describe_instance_by_name(machine_def["region"], machine_name)
        if not instance:
            fail(f"created instance for {machine_name}, but could not find it during sync")
        update_machine_state_from_instance(record, args.deployment, machine_def, instance)

    save_json_file(machines_path, machines_state)
    command_servers_list(args)


def command_servers_delete_aws(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    verify_aws_account(deployment)
    selected_names = select_machine_names(deployment, names=args.name, provider="aws")
    if not selected_names:
        fail("no machines selected for deletion")

    for machine_name in selected_names:
        machine_def = deployment["machines"][machine_name]
        record = machines_state.get("machines", {}).get(machine_name)
        instance = aws_describe_instance_by_name(machine_def["region"], machine_name)
        instance_id = record.get("provider_id") if record else None
        if instance:
            instance_id = instance.get("InstanceId")
        if instance_id:
            info(f"terminating {machine_name} ({instance_id})")
            run_command(
                [
                    "aws",
                    "ec2",
                    "terminate-instances",
                    "--region",
                    machine_def["region"],
                    "--instance-ids",
                    instance_id,
                ]
            )
        machines_state.get("machines", {}).pop(machine_name, None)

    save_json_file(machines_path, machines_state)


def command_servers_setup(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    authorized_keys_text, sources = collect_authorized_public_keys(deployment)
    authorized_keys_b64 = base64.b64encode(authorized_keys_text.encode("utf-8")).decode("ascii")
    tailscale_auth_key = lookup_secret_env(deployment, "tailscale_auth_key")
    info(
        "authorizing SSH keys from: "
        + ", ".join(str(path) for path in sources)
    )

    for machine_name in select_machine_names(deployment, names=args.name):
        machine_def = deployment["machines"][machine_name]
        record = ensure_machine_record(machines_state, args.deployment, machine_name, machine_def)
        if not machine_public_host(record):
            fail(
                f"{machine_name} has no reachable host recorded; "
                "run `./run infra:apply` or `./run infra:output` first"
            )
        ports_csv = ",".join(str(port) for port in service_ports(machine_def) if port != 22)
        remote_root = machine_def.get("remote_root", "/opt/shardd")
        deploy_user = machine_def.get("ssh", {}).get("user", "ubuntu")
        info(f"setting up host {machine_name}")
        ssh_run_script(
            record,
            machine_def,
            SCRIPTS_DIR / "setup_host.sh",
            [
                remote_root,
                deploy_user,
                ports_csv,
                authorized_keys_b64,
                machine_name,
                tailscale_auth_key,
            ],
        )
        setup_state = probe_setup(deployment, machine_name, machine_def, record)
        setup_state["last_configured_at"] = utc_now()

    save_json_file(machines_path, machines_state)
    command_servers_list(args)


def command_servers_import_key_aws(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    cluster = load_cluster(cluster_path)
    deployment = get_deployment(cluster, args.deployment)
    identity = verify_aws_account(deployment)
    info(f"using AWS account {identity['Account']} ({identity['Arn']})")

    public_key_path = infra_ssh_public_key_path(deployment)
    private_key_path = infra_ssh_private_key_path(deployment)
    if not public_key_path.exists() or not private_key_path.exists():
        fail(
            f"infra ssh key is missing: {private_key_path} / {public_key_path}. "
            f"Run `./run state ensure-ssh-key --deployment {args.deployment}` first."
        )

    imports: dict[tuple[str, str], dict[str, Any]] = {}
    for machine_name in select_machine_names(deployment, names=args.name, provider="aws"):
        machine_def = deployment["machines"][machine_name]
        key_name = machine_def.get("provider_config", {}).get("key_name")
        if not key_name:
            fail(f"machine {machine_name} is missing provider_config.key_name")
        imports[(machine_def["region"], str(key_name))] = machine_def

    for (region, key_name), _machine_def in sorted(imports.items()):
        existing = aws_find_key_pair(region, key_name)
        if existing and not args.replace:
            info(f"{region}: key pair `{key_name}` already exists, leaving it unchanged")
            continue
        if existing and args.replace:
            info(f"{region}: deleting existing key pair `{key_name}`")
            run_command(
                [
                    "aws",
                    "ec2",
                    "delete-key-pair",
                    "--region",
                    region,
                    "--key-name",
                    key_name,
                ]
            )

        info(f"{region}: importing key pair `{key_name}` from {public_key_path}")
        tag_specifications = json.dumps(
            [
                {
                    "ResourceType": "key-pair",
                    "Tags": [
                        {"Key": "Project", "Value": "shardd"},
                        {"Key": "Deployment", "Value": deployment.get("display_name", args.deployment)},
                        {"Key": "ManagedBy", "Value": "shardd-infra"},
                    ],
                }
            ]
        )
        run_command(
            [
                "aws",
                "ec2",
                "import-key-pair",
                "--region",
                region,
                "--output",
                "json",
                "--key-name",
                key_name,
                "--public-key-material",
                f"fileb://{public_key_path}",
                "--tag-specifications",
                tag_specifications,
            ]
        )


def command_servers_inspect(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    selected_names = select_machine_names(deployment, names=args.name)
    if len(selected_names) != 1:
        fail("servers inspect requires exactly one --name")
    machine_name = selected_names[0]
    machine_def = deployment["machines"][machine_name]
    record = ensure_machine_record(machines_state, args.deployment, machine_name, machine_def)
    if args.refresh and machine_public_host(record):
        probe_setup(deployment, machine_name, machine_def, record)
        save_json_file(machines_path, machines_state)
    print(json.dumps(record, indent=2, sort_keys=True))


def collect_deploy_targets(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    machine_names: list[str],
) -> list[dict[str, Any]]:
    targets: list[dict[str, Any]] = []
    for machine_name in machine_names:
        machine_def = deployment["machines"][machine_name]
        machine_state = ensure_machine_record(machines_state, deployment_name, machine_name, machine_def)
        for index, service in enumerate(machine_def.get("services", [])):
            targets.append(
                {
                    "machine_name": machine_name,
                    "machine_def": machine_def,
                    "machine_state": machine_state,
                    "service_index": index,
                    "service": service,
                }
            )
    return targets


def plan_rows(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    targets: list[dict[str, Any]],
    local_image_ids: dict[str, str],
) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    temp_state = copy.deepcopy(machines_state)
    for target in targets:
        machine_name = target["machine_name"]
        machine_def = target["machine_def"]
        machine_state = temp_state["machines"][machine_name]
        service = target["service"]
        service_index = target["service_index"]
        service_name = machine_service_name(machine_name, machine_def, service_index, service)
        spec = bundle_spec(service["bundle"])
        deployment_record = machine_state.get("deployments", {}).get(service_name, {})
        image_status = "ready"
        if any(image_key not in local_image_ids for image_key in spec.image_keys):
            image_status = "missing-image"
        try:
            rendered_dir = Path(tempfile.mkdtemp(prefix="shardd-plan-"))
            _, revision, _ = render_service_bundle(
                deployment_name,
                deployment,
                temp_state,
                machine_name,
                machine_def,
                machine_state,
                service_index,
                service,
                local_image_ids,
                rendered_dir,
                strict=False,
            )
        finally:
            shutil.rmtree(rendered_dir, ignore_errors=True)
        rows.append(
            {
                "machine": machine_name,
                "service": service_name,
                "bundle": service["bundle"],
                "host": machine_state.get("host", "-"),
                "setup": "ready" if machine_state.get("setup", {}).get("fully_setup") else "pending",
                "images": image_status,
                "status": "up-to-date"
                if deployment_record.get("revision") == revision
                else "needs-apply",
            }
        )
    return rows


def command_deploy_plan(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    machines_state = load_machines(expand_path(args.machines_state))
    deployment = get_deployment(cluster, args.deployment)
    machine_names = select_machine_names(deployment, names=args.name)
    targets = collect_deploy_targets(args.deployment, deployment, machines_state, machine_names)
    needed_image_keys = {image_key for target in targets for image_key in bundle_spec(target["service"]["bundle"]).image_keys}
    local_image_ids = inspect_local_image_ids(needed_image_keys)
    rows = plan_rows(args.deployment, deployment, machines_state, targets, local_image_ids)
    print(
        table(
            rows,
            [
                ("machine", "MACHINE"),
                ("service", "SERVICE"),
                ("bundle", "BUNDLE"),
                ("host", "HOST"),
                ("setup", "SETUP"),
                ("images", "IMAGES"),
                ("status", "STATUS"),
            ],
        )
    )


def load_remote_images(
    machine_state: dict[str, Any],
    machine_def: dict[str, Any],
    remote_root: str,
    image_archives: dict[str, Path],
) -> None:
    remote_images_dir = f"{remote_root.rstrip('/')}/images"
    ssh_run(machine_state, machine_def, f"mkdir -p {shlex.quote(remote_images_dir)}")
    for image_key, archive_path in image_archives.items():
        remote_path = f"{remote_images_dir}/{archive_path.name}"
        rsync_to(machine_state, machine_def, archive_path, remote_path, delete=False)
        ssh_run(
            machine_state,
            machine_def,
            f"docker load -i {shlex.quote(remote_path)}",
        )


def deploy_service_to_host(
    deployment_name: str,
    deployment: dict[str, Any],
    machines_state: dict[str, Any],
    target: dict[str, Any],
    image_ids: dict[str, str],
    image_archives: dict[str, Path],
    scratch_dir: Path,
    *,
    transport: str,
    image_refs: dict[str, str],
) -> tuple[str, str]:
    machine_name = target["machine_name"]
    machine_def = target["machine_def"]
    machine_state = target["machine_state"]
    service = target["service"]
    service_index = target["service_index"]
    if not machine_state.get("setup", {}).get("fully_setup"):
        fail(f"{machine_name} is not marked fully setup; run `servers setup` first")
    remote_root = machine_def.get("remote_root", "/opt/shardd")
    service_name, revision, image_keys = render_service_bundle(
        deployment_name,
        deployment,
        machines_state,
        machine_name,
        machine_def,
        machine_state,
        service_index,
        service,
        image_ids,
        scratch_dir,
        strict=True,
        image_refs=image_refs,
    )
    remote_service_root = f"{remote_root.rstrip('/')}/services/{service_name}"
    ssh_run(
        machine_state,
        machine_def,
        f"mkdir -p {shlex.quote(remote_service_root)}",
    )
    rsync_to(
        machine_state,
        machine_def,
        scratch_dir / service_name,
        f"{remote_service_root}/",
        delete=True,
    )
    if transport == "registry":
        refs_for_service = {image_key: image_refs[image_key] for image_key in image_keys}
        pull_remote_images(machine_state, machine_def, refs_for_service)
    else:
        archives_for_service = {image_key: image_archives[image_key] for image_key in image_keys}
        load_remote_images(machine_state, machine_def, remote_root, archives_for_service)
    ssh_run(
        machine_state,
        machine_def,
        f"cd {shlex.quote(remote_service_root)} && docker compose up -d --remove-orphans",
    )
    deployment_record = machine_state.setdefault("deployments", {}).setdefault(service_name, {})
    deployment_record.update(
        {
            "bundle": service["bundle"],
            "revision": revision,
            "updated_at": utc_now(),
            "remote_dir": remote_service_root,
            "image_ids": {image_key: image_ids.get(image_key, "") for image_key in image_keys},
        }
    )
    return service_name, revision


def command_deploy_apply(args: argparse.Namespace) -> None:
    cluster_path = expand_path(args.cluster_state)
    machines_path = expand_path(args.machines_state)
    cluster = load_cluster(cluster_path)
    machines_state = load_machines(machines_path)
    deployment = get_deployment(cluster, args.deployment)
    machine_names = select_machine_names(deployment, names=args.name)
    targets = collect_deploy_targets(args.deployment, deployment, machines_state, machine_names)
    needed_image_keys = {image_key for target in targets for image_key in bundle_spec(target["service"]["bundle"]).image_keys}

    transport = args.transport
    registry = deployment.get("image_registry")
    if transport == "registry" and not registry:
        fail(
            "transport=registry requires deployment.image_registry to be set in cluster.json"
        )
    version = resolve_image_version(args.image_tag)

    if args.skip_build:
        image_ids = inspect_local_image_ids(needed_image_keys)
    else:
        image_ids = build_images(needed_image_keys, version=version)
    missing = sorted(image_key for image_key in needed_image_keys if image_key not in image_ids)
    if missing:
        fail(f"local Docker image tags are missing: {', '.join(missing)}")

    # Resolve image refs — what compose's `image:` line will point at. For
    # registry transport: `<registry>/<name>:<version>`. For tar: the legacy
    # `local/shardd-X:infra` tag loaded from the archive.
    if transport == "registry":
        image_refs = {
            key: registry_image_ref(IMAGE_SPECS[key], registry, version)
            for key in needed_image_keys
        }
        if args.skip_push:
            info(f"--skip-push set; trusting that {version} is already in the registry")
        else:
            push_images(needed_image_keys, registry, version)
    else:
        image_refs = {key: IMAGE_SPECS[key].tag for key in needed_image_keys}

    with tempfile.TemporaryDirectory(prefix="shardd-apply-") as tempdir:
        temp_root = Path(tempdir)
        archive_dir = temp_root / "images"
        # Only save tarballs when we're about to rsync them to every host.
        if transport == "tar":
            image_archives = save_image_archives(needed_image_keys, archive_dir)
        else:
            image_archives = {}
        for target in targets:
            machine_name = target["machine_name"]
            service_name = machine_service_name(
                machine_name,
                target["machine_def"],
                target["service_index"],
                target["service"],
            )
            info(f"deploying {service_name} on {machine_name}")
            render_root = temp_root / "rendered" / machine_name
            render_root.mkdir(parents=True, exist_ok=True)
            deploy_service_to_host(
                args.deployment,
                deployment,
                machines_state,
                target,
                image_ids,
                image_archives,
                render_root,
                transport=transport,
                image_refs=image_refs,
            )

    save_json_file(machines_path, machines_state)
    command_deploy_status(args)
    if args.validate_scopes_file:
        run_scope_validation_for_deployment(
            deployment=deployment,
            cases_file=args.validate_scopes_file,
            machine_secret_env=args.scope_machine_secret_env,
            edge_ids=args.scope_edge,
            timeout=float(args.scope_timeout),
        )


def command_deploy_status(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    machines_state = load_machines(expand_path(args.machines_state))
    deployment = get_deployment(cluster, args.deployment)
    machine_names = select_machine_names(deployment, names=args.name)

    rows: list[dict[str, Any]] = []
    for machine_name in machine_names:
        machine_def = deployment["machines"][machine_name]
        machine_state = machines_state.get("machines", {}).get(machine_name, {})
        setup = "ready" if machine_state.get("setup", {}).get("fully_setup") else "pending"
        for index, service in enumerate(machine_def.get("services", [])):
            service_name = machine_service_name(machine_name, machine_def, index, service)
            deployed = machine_state.get("deployments", {}).get(service_name, {})
            rows.append(
                {
                    "machine": machine_name,
                    "service": service_name,
                    "bundle": service["bundle"],
                    "host": machine_state.get("host", "-"),
                    "setup": setup,
                    "deployed_at": deployed.get("updated_at", "-"),
                    "revision": deployed.get("revision", "-")[:12] if deployed.get("revision") else "-",
                }
            )
    print(
        table(
            rows,
            [
                ("machine", "MACHINE"),
                ("service", "SERVICE"),
                ("bundle", "BUNDLE"),
                ("host", "HOST"),
                ("setup", "SETUP"),
                ("deployed_at", "DEPLOYED_AT"),
                ("revision", "REVISION"),
            ],
        )
    )


def deployment_dashboard_url(deployment: dict[str, Any]) -> str:
    for machine_def in deployment["machines"].values():
        for service in machine_def.get("services", []):
            if service.get("bundle") != "dashboard":
                continue
            app_origin = str(service.get("vars", {}).get("app_origin", "")).strip()
            if app_origin:
                return app_origin.rstrip("/")
            public_dns_name = str(machine_def.get("public_dns_name", "")).strip()
            if public_dns_name:
                return f"https://{public_dns_name}"
    fail("deployment is missing a dashboard service or app origin")


def deployment_public_edge_urls(deployment: dict[str, Any]) -> dict[str, str]:
    edges: dict[str, str] = {}
    for entry in deployment.get("public_edges", []):
        edge_id = str(entry.get("edge_id", "")).strip()
        base_url = str(entry.get("base_url", "")).strip()
        if edge_id and base_url:
            edges[edge_id] = base_url.rstrip("/")
    return edges


def run_scope_validation_for_deployment(
    *,
    deployment: dict[str, Any],
    cases_file: str,
    machine_secret_env: str | None,
    edge_ids: list[str] | None,
    timeout: float,
) -> None:
    dashboard_url = deployment_dashboard_url(deployment)
    edge_urls = deployment_public_edge_urls(deployment)
    if edge_ids:
        selected: dict[str, str] = {}
        missing = [edge_id for edge_id in edge_ids if edge_id not in edge_urls]
        if missing:
            fail(
                "unknown public edge id(s): "
                + ", ".join(sorted(missing))
                + f"; available: {', '.join(sorted(edge_urls)) or '(none)'}"
            )
        for edge_id in edge_ids:
            selected[edge_id] = edge_urls[edge_id]
        edge_urls = selected

    env_name = machine_secret_env or deployment.get("secret_env", {}).get("dashboard_machine_auth_secret")
    if not env_name:
        fail("deployment is missing the dashboard_machine_auth_secret env mapping")
    machine_secret = os.environ.get(str(env_name), "").strip()
    if not machine_secret:
        fail(f"required scope validation env var is not set: {env_name}")

    info(f"validating scope enforcement via {dashboard_url}")
    try:
        cases = scope_validation.load_cases(expand_path(cases_file))
        rows = scope_validation.validate_cases(
            cases,
            dashboard_url=dashboard_url,
            machine_secret=machine_secret,
            edge_urls=edge_urls,
            timeout=timeout,
        )
    except scope_validation.ScopeValidationError as error:
        fail(str(error))

    print(
        table(
            rows,
            [
                ("case", "CASE"),
                ("action", "ACTION"),
                ("target", "TARGET"),
                ("edge", "EDGE"),
                ("expected", "EXPECTED"),
                ("observed", "OBSERVED"),
                ("result", "RESULT"),
            ],
        )
    )


def command_deploy_validate_scopes(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    deployment = get_deployment(cluster, args.deployment)
    run_scope_validation_for_deployment(
        deployment=deployment,
        cases_file=args.cases_file,
        machine_secret_env=args.machine_secret_env,
        edge_ids=args.edge,
        timeout=float(args.timeout),
    )


def command_dns_zone(args: argparse.Namespace) -> None:
    cluster = load_cluster(expand_path(args.cluster_state))
    machines_state = load_machines(expand_path(args.machines_state))
    deployment = get_deployment(cluster, args.deployment)
    zone_root = normalize_dns_name(args.zone or deployment.get("dns_root_zone", ""))
    if not zone_root:
        fail("dns zone root is missing; pass --zone or set deployment.dns_root_zone")

    records: list[tuple[str, str]] = []
    seen: set[tuple[str, str]] = set()

    def add_record(name: str | None, ip: str | None) -> None:
        if not name or not ip:
            return
        normalized_name = normalize_dns_name(name)
        record = (normalized_name, ip)
        if record in seen:
            return
        seen.add(record)
        records.append(record)

    edge_ips: list[str] = []
    selected_api_machines = edge_api_machine_names(deployment)
    lb_enabled = bool(deployment.get("cloudflare_lb_enabled", bool(deployment.get("edge_api_dns_name"))))
    for machine_name in select_machine_names(deployment, names=args.name):
        machine_def = deployment["machines"][machine_name]
        machine_state = machines_state.get("machines", {}).get(machine_name, {})
        public_ip = machine_state.get("public_ip")
        for dns_name in machine_dns_names(machine_def):
            add_record(dns_name, public_ip)
        if (
            any(service.get("bundle") == "edge-node" for service in machine_def.get("services", []))
            and public_ip
            and (selected_api_machines is None or machine_name in selected_api_machines)
        ):
            edge_ips.append(public_ip)

    api_name = deployment.get("edge_api_dns_name")
    if api_name and not lb_enabled:
        for ip in edge_ips:
            add_record(str(api_name), ip)

    if not records:
        fail("no DNS records could be generated; make sure machines have public_ip values and DNS names configured")

    rendered_lines = [f"$ORIGIN {zone_root}.", f"$TTL {int(args.ttl)}", ""]
    for name, ip in sorted(records):
        rendered_lines.append(f"{relative_dns_name(name, zone_root)}\tIN\tA\t{ip}")
    rendered = "\n".join(rendered_lines) + "\n"

    if args.output:
        output_path = expand_path(args.output)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(rendered)
        info(f"wrote {output_path}")
    else:
        sys.stdout.write(rendered)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="shardd infrastructure control plane")
    parser.add_argument(
        "--cluster-state",
        default=str(DEFAULT_CLUSTER_PATH),
        help="path to desired cluster config JSON",
    )
    parser.add_argument(
        "--machines-state",
        default=str(DEFAULT_MACHINES_PATH),
        help="path to observed machines state JSON",
    )

    subparsers = parser.add_subparsers(dest="command", required=True)

    infra_parser = subparsers.add_parser("infra", help="manage cloud resources through Terraform")
    infra_sub = infra_parser.add_subparsers(dest="infra_command", required=True)
    infra_init = infra_sub.add_parser("init", help="render tfvars and initialize Terraform state")
    infra_init.add_argument("--deployment", required=True)
    infra_init.set_defaults(func=command_infra_init)
    infra_plan = infra_sub.add_parser("plan", help="show the Terraform plan for a deployment")
    infra_plan.add_argument("--deployment", required=True)
    infra_plan.set_defaults(func=command_infra_plan)
    infra_apply = infra_sub.add_parser("apply", help="apply Terraform and sync machines.json")
    infra_apply.add_argument("--deployment", required=True)
    infra_apply.set_defaults(func=command_infra_apply)
    infra_destroy = infra_sub.add_parser("destroy", help="destroy Terraform-managed resources and clear machine records")
    infra_destroy.add_argument("--deployment", required=True)
    infra_destroy.set_defaults(func=command_infra_destroy)
    infra_output = infra_sub.add_parser("output", help="sync Terraform outputs into machines.json")
    infra_output.add_argument("--deployment", required=True)
    infra_output.set_defaults(func=command_infra_output)

    state_parser = subparsers.add_parser("state", help="initialize local state files")
    state_sub = state_parser.add_subparsers(dest="state_command", required=True)
    state_init = state_sub.add_parser("init", help="write cluster.json and machines.json from examples")
    state_init.add_argument("--force", action="store_true", help="overwrite existing state files")
    state_init.set_defaults(func=command_state_init)
    state_ssh = state_sub.add_parser("ensure-ssh-key", help="create the repo-local infra SSH key if missing")
    state_ssh.add_argument("--deployment", required=True)
    state_ssh.set_defaults(func=command_state_ensure_ssh_key)

    servers_parser = subparsers.add_parser("servers", help="manage machines")
    servers_sub = servers_parser.add_subparsers(dest="servers_command", required=True)

    servers_list = servers_sub.add_parser("list", help="list desired machines and observed state")
    servers_list.add_argument("--deployment", required=True)
    servers_list.add_argument("--name", action="append", help="limit to specific machine(s)")
    servers_list.set_defaults(func=command_servers_list)

    servers_create = servers_sub.add_parser("create", help="create cloud machines")
    servers_create_sub = servers_create.add_subparsers(dest="provider", required=True)
    servers_create_aws = servers_create_sub.add_parser("aws", help="create AWS EC2 instances")
    servers_create_aws.add_argument("--deployment", required=True)
    servers_create_aws.add_argument("--name", action="append", help="limit to specific machine(s)")
    servers_create_aws.add_argument("--dry-run", action="store_true", help="validate create requests without creating instances")
    servers_create_aws.set_defaults(func=command_servers_create_aws)

    servers_sync = servers_sub.add_parser("sync", help="sync provider state into machines.json")
    servers_sync_sub = servers_sync.add_subparsers(dest="provider", required=True)
    servers_sync_aws = servers_sync_sub.add_parser("aws", help="sync AWS EC2 instances")
    servers_sync_aws.add_argument("--deployment", required=True)
    servers_sync_aws.add_argument("--name", action="append", help="limit to specific machine(s)")
    servers_sync_aws.set_defaults(func=command_servers_sync_aws)

    servers_ensure_sg = servers_sub.add_parser("ensure-sg", help="create or update cloud security groups and write them into cluster.json")
    servers_ensure_sg_sub = servers_ensure_sg.add_subparsers(dest="provider", required=True)
    servers_ensure_sg_aws = servers_ensure_sg_sub.add_parser("aws", help="ensure AWS security groups for selected machines")
    servers_ensure_sg_aws.add_argument("--deployment", required=True)
    servers_ensure_sg_aws.add_argument("--name", action="append", help="limit to specific machine(s)")
    servers_ensure_sg_aws.add_argument("--cidr", default="0.0.0.0/0", help="CIDR to allow on public TCP ports")
    servers_ensure_sg_aws.set_defaults(func=command_servers_ensure_sg_aws)

    servers_delete = servers_sub.add_parser("delete", help="delete cloud machines")
    servers_delete_sub = servers_delete.add_subparsers(dest="provider", required=True)
    servers_delete_aws = servers_delete_sub.add_parser("aws", help="delete AWS EC2 instances")
    servers_delete_aws.add_argument("--deployment", required=True)
    servers_delete_aws.add_argument("--name", action="append", required=True, help="machine(s) to delete")
    servers_delete_aws.set_defaults(func=command_servers_delete_aws)

    servers_setup = servers_sub.add_parser("setup", help="install Docker/UFW/infra key on existing machines")
    servers_setup.add_argument("--deployment", required=True)
    servers_setup.add_argument("--name", action="append", help="limit to specific machine(s)")
    servers_setup.set_defaults(func=command_servers_setup)

    servers_import_key = servers_sub.add_parser("import-key", help="import the repo infra SSH key into cloud provider key pairs")
    servers_import_key_sub = servers_import_key.add_subparsers(dest="provider", required=True)
    servers_import_key_aws = servers_import_key_sub.add_parser("aws", help="import the infra SSH key into AWS EC2 key pairs")
    servers_import_key_aws.add_argument("--deployment", required=True)
    servers_import_key_aws.add_argument("--name", action="append", help="limit to machine(s) and their referenced key pairs")
    servers_import_key_aws.add_argument("--replace", action="store_true", help="replace an existing AWS key pair with the same name")
    servers_import_key_aws.set_defaults(func=command_servers_import_key_aws)

    servers_inspect = servers_sub.add_parser("inspect", help="show one machine record")
    servers_inspect.add_argument("--deployment", required=True)
    servers_inspect.add_argument("--name", action="append", required=True, help="machine to inspect")
    servers_inspect.add_argument("--refresh", action="store_true", help="probe the host before printing")
    servers_inspect.set_defaults(func=command_servers_inspect)

    deploy_parser = subparsers.add_parser("deploy", help="render and apply service bundles")
    deploy_sub = deploy_parser.add_subparsers(dest="deploy_command", required=True)

    deploy_plan = deploy_sub.add_parser("plan", help="show desired service deployment plan")
    deploy_plan.add_argument("--deployment", required=True)
    deploy_plan.add_argument("--name", action="append", help="limit to specific machine(s)")
    deploy_plan.set_defaults(func=command_deploy_plan)

    deploy_apply = deploy_sub.add_parser("apply", help="build images and deploy services")
    deploy_apply.add_argument("--deployment", required=True)
    deploy_apply.add_argument("--name", action="append", help="limit to specific machine(s)")
    deploy_apply.add_argument("--skip-build", action="store_true", help="reuse existing local Docker tags")
    deploy_apply.add_argument(
        "--transport",
        choices=("registry", "tar"),
        default="registry",
        help="image ship method: pull from the tailnet registry (default) or rsync a docker save tarball (break-glass)",
    )
    deploy_apply.add_argument(
        "--image-tag",
        help="override the version tag used for the registry push/pull (defaults to git short SHA, +'-dirty' if tree is dirty)",
    )
    deploy_apply.add_argument(
        "--skip-push",
        action="store_true",
        help="skip docker push step; trusts that the requested --image-tag is already in the registry (rollback shortcut)",
    )
    deploy_apply.add_argument(
        "--validate-scopes-file",
        help="run live scope validation against the deployed dashboard and public edges using a JSON case file",
    )
    deploy_apply.add_argument(
        "--scope-machine-secret-env",
        help="override the env var name used for the dashboard machine auth secret during scope validation",
    )
    deploy_apply.add_argument(
        "--scope-edge",
        action="append",
        help="limit post-deploy scope validation to specific public edge id(s)",
    )
    deploy_apply.add_argument(
        "--scope-timeout",
        type=float,
        default=15.0,
        help="per-request timeout in seconds for post-deploy scope validation",
    )
    deploy_apply.set_defaults(func=command_deploy_apply)

    deploy_status = deploy_sub.add_parser("status", help="show local deployment state")
    deploy_status.add_argument("--deployment", required=True)
    deploy_status.add_argument("--name", action="append", help="limit to specific machine(s)")
    deploy_status.set_defaults(func=command_deploy_status)

    deploy_validate_scopes = deploy_sub.add_parser(
        "validate-scopes",
        help="validate live scope enforcement against the deployment dashboard and public edges",
    )
    deploy_validate_scopes.add_argument("--deployment", required=True)
    deploy_validate_scopes.add_argument("--cases-file", required=True, help="JSON file describing scope cases to verify")
    deploy_validate_scopes.add_argument(
        "--machine-secret-env",
        help="override the env var name used for the dashboard machine auth secret",
    )
    deploy_validate_scopes.add_argument("--edge", action="append", help="limit validation to specific public edge id(s)")
    deploy_validate_scopes.add_argument(
        "--timeout",
        type=float,
        default=15.0,
        help="per-request timeout in seconds",
    )
    deploy_validate_scopes.set_defaults(func=command_deploy_validate_scopes)

    dns_parser = subparsers.add_parser("dns", help="generate DNS artifacts from observed machine state")
    dns_sub = dns_parser.add_subparsers(dest="dns_command", required=True)
    dns_zone = dns_sub.add_parser("zone", help="render a BIND-style zone file")
    dns_zone.add_argument("--deployment", required=True)
    dns_zone.add_argument("--name", action="append", help="limit to specific machine(s)")
    dns_zone.add_argument("--zone", help="override deployment dns_root_zone")
    dns_zone.add_argument("--ttl", type=int, default=300, help="DNS TTL in seconds")
    dns_zone.add_argument("--output", help="write zone file to a path instead of stdout")
    dns_zone.set_defaults(func=command_dns_zone)

    return parser


def main() -> None:
    parser = build_parser()
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
