#!/usr/bin/env python3
"""Exercise a built Linux Docker image, using only Python's standard library.

Example (on a Linux runner with Docker):
    python3 tests/container_smoke.py --image local/wb2api:amd64 \
        --architecture amd64 --report container-smoke.json

No upstream credentials, host data mounts, image pulls, or resource pruning.
Reports intentionally exclude HTTP bodies, Docker output, and credentials.
"""
from __future__ import annotations

import argparse
import http.client
import json
import math
from pathlib import Path
import subprocess
import sys
import time
import uuid


LABEL = "wb2api.container-smoke"
ENTRYPOINT = ["/usr/local/bin/wb2api"]
CA_FILE = "/etc/ssl/certs/ca-certificates.crt"


class SmokeFailure(RuntimeError):
    """A safe, credential-free diagnostic suitable for the JSON report."""


def require(condition, message):
    if not condition:
        raise SmokeFailure(message)


def positive_timeout(value):
    try:
        seconds = float(value)
    except ValueError:
        raise argparse.ArgumentTypeError("timeout must be a number") from None
    if not math.isfinite(seconds) or not 0 < seconds <= 300:
        raise argparse.ArgumentTypeError("timeout must be greater than 0 and at most 300")
    return seconds


def image_reference(value):
    if not value or value.startswith("-") or any(c.isspace() for c in value):
        raise argparse.ArgumentTypeError("expected a local Docker image reference")
    return value


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, type=image_reference)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--architecture", choices=("amd64", "arm64"))
    parser.add_argument("--startup-timeout", type=positive_timeout, default=60.0)
    parser.add_argument("--shutdown-timeout", type=positive_timeout, default=15.0)
    return parser


def docker(arguments, *, timeout=30, check=True):
    # Never let CalledProcessError/TimeoutExpired expose command-line credentials.
    try:
        result = subprocess.run(
            ["docker", *arguments], capture_output=True, text=True,
            encoding="utf-8", errors="replace", timeout=timeout, check=False,
        )
    except subprocess.TimeoutExpired:
        raise SmokeFailure("Docker command timed out (output omitted)") from None
    except OSError:
        raise SmokeFailure("Docker executable unavailable or could not be started") from None
    if check and result.returncode != 0:
        raise SmokeFailure(f"Docker command failed with exit {result.returncode} (output omitted)")
    return result


def inspect_one(arguments):
    output = docker(arguments).stdout
    try:
        value = json.loads(output)
    except ValueError:
        raise SmokeFailure("Docker inspect did not return valid JSON") from None
    require(isinstance(value, list) and len(value) == 1 and isinstance(value[0], dict),
            "Docker inspect did not return exactly one object")
    return value[0]


def published_port(container):
    bindings = container.get("NetworkSettings", {}).get("Ports", {}).get("8788/tcp")
    require(isinstance(bindings, list) and len(bindings) == 1,
            "Expected exactly one published port for 8788/tcp")
    binding = bindings[0]
    require(isinstance(binding, dict) and binding.get("HostIp") == "127.0.0.1",
            "Container port must be published on IPv4 loopback only")
    try:
        port = int(binding["HostPort"])
    except (KeyError, TypeError, ValueError):
        raise SmokeFailure("Docker returned an invalid published port") from None
    require(0 < port <= 65535, "Docker returned an out-of-range published port")
    return port


def run_arguments(image, name, owner, api_key, password):
    return [
        "run", "--detach", "--pull", "never", "--name", name,
        "--label", f"{LABEL}={owner}",
        "--read-only", "--user", "10001:10001", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges:true",
        "--tmpfs", "/data/accounts:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700",
        "--tmpfs", "/data/usage:rw,nosuid,nodev,noexec,uid=10001,gid=10001,mode=0700",
        "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,mode=1777",
        "--publish", "127.0.0.1::8788",
        "--env", f"API_KEY={api_key}",
        "--env", f"WB_PANEL_PASSWORD={password}",
        image, "--host", "0.0.0.0", "--port", "8788", "--offline",
        "--accounts-dir", "/data/accounts", "--usage-dir", "/data/usage",
    ]


def request(port, method, path, *, headers=None, payload=None, timeout=3):
    # http.client bypasses proxy environment variables and does not follow redirects.
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    outgoing_headers = dict(headers or {})
    body = None
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
        outgoing_headers["Content-Type"] = "application/json"
    try:
        connection.request(method, path, body=body, headers=outgoing_headers)
        reply = connection.getresponse()
        data = reply.read(4 * 1024 * 1024 + 1)
        require(len(data) <= 4 * 1024 * 1024, "HTTP response exceeded smoke-test size limit")
        return reply.status, reply.getheader("Content-Type", ""), data
    finally:
        connection.close()


def expect_status(reply, expected):
    require(reply[0] == expected, f"Expected HTTP {expected}, received HTTP {reply[0]}")
    return reply


def json_object(reply):
    require("application/json" in reply[1].lower(), "Expected a JSON response content type")
    try:
        value = json.loads(reply[2])
    except (ValueError, UnicodeError):
        raise SmokeFailure("HTTP response was not valid JSON") from None
    require(isinstance(value, dict), "Expected a JSON object response")
    return value


class ContainerSmoke:
    def __init__(self, args):
        self.args = args
        self.owner = uuid.uuid4().hex
        self.name = "wb2api-smoke-" + self.owner
        self.api_key = "synthetic-smoke-api-" + uuid.uuid4().hex
        self.password = "synthetic-smoke-panel-" + uuid.uuid4().hex
        self.run_attempted = False
        self.port = None
        self.token = None
        self.report = {
            "schema_version": 1, "image": args.image,
            "expected_architecture": args.architecture, "container": self.name,
            "ok": False, "checks": [],
        }
        self.stage = "initialization"

    def passed(self, name):
        self.report["checks"].append({"name": name, "ok": True})

    def call(self, path, *, method="GET", auth=None, payload=None):
        headers = {}
        if auth == "api":
            headers["Authorization"] = "Bearer " + self.api_key
        elif auth == "panel":
            headers["X-Panel-Token"] = self.token
        return request(self.port, method, path, headers=headers, payload=payload)

    def get_json(self, path, *, method="GET", auth=None, payload=None):
        return json_object(expect_status(
            self.call(path, method=method, auth=auth, payload=payload), 200))

    def wait_ready(self):
        deadline = time.monotonic() + self.args.startup_timeout
        while time.monotonic() < deadline:
            container = inspect_one(["container", "inspect", self.name])
            require(container.get("State", {}).get("Running") is True,
                    "Container exited before becoming healthy (logs omitted)")
            try:
                reply = request(self.port, "GET", "/health",
                                timeout=max(0.01, min(2, deadline - time.monotonic())))
            except (OSError, http.client.HTTPException):
                reply = None
            if reply is not None and reply[0] == 200:
                health = json_object(reply)
                require(health.get("ok") is True, "Health did not report ok=true")
                require(health.get("api_key_required") is True,
                        "Health did not require an API key")
                return
            time.sleep(min(0.2, max(0, deadline - time.monotonic())))
        raise SmokeFailure("Container health readiness deadline exceeded")

    def exercise(self):
        self.stage = "image_metadata"
        image = inspect_one(["image", "inspect", self.args.image])
        config = image.get("Config", {})
        require(image.get("Os") == "linux", "Image OS must be linux")
        require(config.get("User") == "10001:10001", "Image USER must be 10001:10001")
        require(config.get("Entrypoint") == ENTRYPOINT,
                "Image entrypoint must be /usr/local/bin/wb2api")
        require(image.get("Architecture") in ("amd64", "arm64"), "Unexpected image architecture")
        if self.args.architecture:
            require(image.get("Architecture") == self.args.architecture,
                    "Image architecture does not match --architecture")
        self.report["image_metadata"] = {
            "os": image["Os"], "architecture": image["Architecture"],
            "user": config["User"], "entrypoint": config["Entrypoint"],
        }
        self.passed(self.stage)

        self.stage = "hardened_container"
        self.run_attempted = True
        docker(run_arguments(self.args.image, self.name, self.owner, self.api_key, self.password))
        container = inspect_one(["container", "inspect", self.name])
        require(container.get("Config", {}).get("User") == "10001:10001",
                "Container must run as UID/GID 10001")
        host = container.get("HostConfig", {})
        require(host.get("ReadonlyRootfs") is True, "Container root filesystem must be read-only")
        require("ALL" in host.get("CapDrop", []), "Container must drop all capabilities")
        require(any(item in ("no-new-privileges", "no-new-privileges:true")
                    for item in host.get("SecurityOpt", [])),
                "Container must enable no-new-privileges")
        self.port = published_port(container)
        self.passed(self.stage)

        self.stage = "health_auth_required"
        self.wait_ready()
        self.passed(self.stage)

        self.stage = "tls_ca_bundle"
        docker(["exec", self.name, "test", "-s", CA_FILE])
        self.passed(self.stage)

        self.stage = "models_authentication"
        expect_status(self.call("/v1/models"), 401)
        models = self.get_json("/v1/models", auth="api")
        require(models.get("object") == "list" and isinstance(models.get("data"), list),
                "Models response must be an OpenAI-style list")
        require(len(models["data"]) > 0, "Bundled model catalogue must not be empty")
        require(all(isinstance(model, dict) and isinstance(model.get("id"), str)
                    and model["id"] and model.get("object") == "model"
                    for model in models["data"]), "Models response contained an invalid model")
        self.passed(self.stage)

        self.stage = "panel_password"
        panel = self.get_json("/panel/status")
        require(panel.get("panel_password_required") is True
                and panel.get("panel_password_is_default") is False
                and panel.get("authenticated") is False
                and panel.get("api_key_set") is True, "Unexpected public panel authentication flags")
        expect_status(self.call("/panel/login", method="POST", payload={"password": "admin"}), 401)
        login = self.get_json("/panel/login", method="POST", payload={"password": self.password})
        require(login.get("ok") is True and login.get("using_default_password") is False,
                "Panel login did not accept the supplied non-default password")
        self.token = login.get("token")
        require(isinstance(self.token, str) and bool(self.token)
                and self.token not in (self.api_key, self.password),
                "Panel login must issue an independent session token")
        self.passed(self.stage)

        self.stage = "management_authentication"
        for path in ("/accounts", "/settings"):
            expect_status(self.call(path), 401)
            expect_status(self.call(path, auth="api"), 401)
            self.get_json(path, auth="panel")
        settings = self.get_json("/settings", auth="panel")
        require(settings.get("auth_required") is True and settings.get("auth_disabled") is False
                and settings.get("panel_password_is_default") is False
                and settings.get("runtime") == "rust", "Unexpected settings authentication/runtime flags")
        require(self.get_json("/panel/status", auth="panel").get("authenticated") is True,
                "Issued panel token was not authenticated")
        self.passed(self.stage)

        self.stage = "dashboard_html"
        dashboard = expect_status(self.call("/dashboard"), 200)
        require("text/html" in dashboard[1].lower() and b"<html" in dashboard[2].lower(),
                "Bundled dashboard did not return HTML")
        self.passed(self.stage)

        self.stage = "graceful_shutdown"
        expect_status(self.call("/shutdown", method="POST", auth="api", payload={}), 401)
        shutdown = self.get_json("/shutdown", method="POST", auth="panel", payload={})
        require(shutdown.get("ok") is True and shutdown.get("shutting_down") is True,
                "Shutdown endpoint did not acknowledge shutdown")
        exited = docker(["wait", self.name], timeout=self.args.shutdown_timeout)
        require(exited.stdout.strip() == "0", "Container did not exit with code 0")
        state = inspect_one(["container", "inspect", self.name]).get("State", {})
        require(state.get("Running") is False and state.get("ExitCode") == 0
                and state.get("OOMKilled") is False, "Container did not shut down cleanly")
        self.passed(self.stage)

    def cleanup(self):
        if not self.run_attempted:
            return
        # If run timed out, its container may still exist. Inspect before removing;
        # never remove by prefix, prune, or touch any unrelated container/image.
        result = docker(["container", "ls", "--all", "--quiet",
                         "--filter", f"label={LABEL}={self.owner}"])
        if not result.stdout.strip():
            return
        container = inspect_one(["container", "inspect", self.name])
        require(container.get("Name") == "/" + self.name
                and container.get("Config", {}).get("Labels", {}).get(LABEL) == self.owner,
                "Cleanup refused: container ownership does not match this run")
        docker(["container", "rm", "--force", self.name])

    def run(self):
        started = time.monotonic()
        try:
            self.exercise()
            self.report["ok"] = True
        except (Exception, KeyboardInterrupt) as exc:
            self.report["error"] = str(exc) if isinstance(exc, SmokeFailure) else type(exc).__name__
            self.report["checks"].append({"name": self.stage, "ok": False})
        finally:
            try:
                self.cleanup()
                self.passed("cleanup")
            except (Exception, KeyboardInterrupt) as exc:
                self.report["ok"] = False
                self.report["cleanup_error"] = str(exc) if isinstance(exc, SmokeFailure) else type(exc).__name__
                self.report["checks"].append({"name": "cleanup", "ok": False})
            self.report["elapsed_seconds"] = round(time.monotonic() - started, 3)
            self.args.report.parent.mkdir(parents=True, exist_ok=True)
            self.args.report.write_text(json.dumps(self.report, indent=2) + "\n", encoding="utf-8")
        return self.report["ok"]


def main(argv=None):
    args = build_parser().parse_args(argv)
    smoke = ContainerSmoke(args)
    try:
        ok = smoke.run()
    except OSError:
        print("Container smoke failed: could not write JSON report", file=sys.stderr)
        return 1
    print(f"Container smoke {'passed' if ok else 'FAILED'}; report: {args.report}")
    if not ok:
        print(smoke.report.get("error", smoke.report.get("cleanup_error", "Smoke test failed")),
              file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
