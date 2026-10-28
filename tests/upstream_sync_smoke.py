#!/usr/bin/env python3
r"""Focused v1.8 upstream-sync regressions against an already-built wb2api.

    python -B tests/upstream_sync_smoke.py --binary /absolute/path/to/wb2api \
        --report tests/upstream-sync-smoke-results.json

Run only after the main integration/build is ready. This script never builds or
updates the gateway and does not run the existing 39-case gateway_smoke suite.
All accounts, keys, requests and upstream replies are synthetic. Imported smoke
helpers enforce Python loopback-only networking; native child safety additionally
relies on --offline and numeric-loopback --test-upstream (not an OS sandbox).
Only this script and the requested JSON report are persistent test artifacts.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
import gzip
import hashlib
import http.client
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable, Iterator
from urllib.parse import urlsplit

# Import the existing helper without creating/updating its bytecode cache.
sys.dont_write_bytecode = True
import gateway_smoke as smoke

HOST = smoke.HOST
MODEL = "gpt-5.5"  # Nonfree: free-model selection deliberately bypasses cooldowns.
OTHER_MODEL = "gpt-5.4"  # Also nonfree, so account-wide throttling cannot hide here.
ENV_COMPAT = "WB_DESKTOP_CHAT_COMPAT"
TEST_VERSION = "upstream-sync-v1.8-smoke/1"
SHUTDOWN_TIMEOUT = 5.0
INVALID_FILES = 2600  # Exceeds the 2000-line logger queue as well as a 64KiB pipe.
FIRST_FUNCTION, LATER_FUNCTION = '{"city":', '"Paris"}'
FIRST_CUSTOM, LATER_CUSTOM = '{"input":"*** Begin', ' Patch\\n+offline\\n*** End Patch"}'
check, expect_status = smoke.check, smoke.expect_status


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_version(path: Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    stat = path.stat()
    return {"path": str(path.resolve()), "sha256": digest.hexdigest(),
            "size_bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns}


def source_version() -> dict[str, Any]:
    rust = Path(__file__).resolve().parents[1]
    paths = [rust / "Cargo.toml", rust / "Cargo.lock", rust / "BASELINE.json"]
    for root in [rust / "crates", rust / "gateway"]:
        paths.extend(p for p in root.rglob("*") if p.is_file()
                     and p.suffix in {".rs", ".toml", ".json"})
    manifest = {p.relative_to(rust).as_posix(): sha256(p.read_bytes())
                for p in sorted(set(paths)) if p.is_file()}
    version: dict[str, Any] = {
        "rust_root": str(rust), "files_sha256": manifest,
        "manifest_sha256": sha256(json.dumps(manifest, sort_keys=True).encode()),
        "test": file_version(Path(__file__)), "helper": file_version(Path(smoke.__file__)),
    }
    try:
        result = subprocess.run(["git", "-C", str(rust), "rev-parse", "HEAD"],
                                capture_output=True, timeout=5, check=False)
        version["git_head"] = result.stdout.decode("ascii", "replace").strip() or None
    except (OSError, subprocess.TimeoutExpired):
        version["git_head"] = None
    return version


class SyncFixture(smoke.FixtureServer):
    """Reuse fixture lifecycle/audit handling, adding narrowly scoped wire cases."""

    def __init__(self) -> None:
        # Parent binds before its worker sees any requests; change the handler
        # before constructing/launching a child gateway.
        self.reset_at = 0
        super().__init__()
        self.RequestHandlerClass = SyncHandler


class SyncHandler(smoke.FixtureHandler):
    server: SyncFixture

    def handle_post(self) -> None:
        self.connection.settimeout(10)
        length = int(self.headers.get("Content-Length", "0"))
        check(0 <= length <= smoke.MAX_BODY, "fixture request exceeded body limit")
        wire_body = self.rfile.read(length)
        decoded = gzip.decompress(wire_body) if self.headers.get("Content-Encoding") == "gzip" else wire_body
        check(len(decoded) <= smoke.MAX_BODY, "decoded fixture body exceeded limit")
        payload = json.loads(decoded or b"{}")
        path = urlsplit(self.path).path
        headers = {key.lower(): value for key, value in self.headers.items()}
        # Credentials are fake, but do not put even those bearer tokens in reports.
        for name in ["authorization", "x-api-key"]:
            if name in headers:
                headers[name] = "[fixture-secret]"
        with self.server.lock:
            mode = self.server.mode
            self.server.records.append({"method": "POST", "path": path, "mode": mode,
                "model": payload.get("model"), "stream": payload.get("stream"),
                "headers": headers, "payload": payload,
                "wire_body_sha256": sha256(wire_body), "decoded_body_sha256": sha256(decoded)})
        if path != "/v2/chat/completions":
            with self.server.lock:
                self.server.unexpected.append(f"POST {path}")
            self.send_json(404, {"error": "unknown upstream-sync fixture route"})
            return
        if mode == "window_429":
            stamp = datetime.fromtimestamp(self.server.reset_at, timezone(timedelta(hours=8)))
            # A non-6004 response must restore via the sidecar, not modelCaps.
            self.send_json(429, {"code": 429, "msg": "usage exceeds frequency limit, "
                + stamp.strftime("reset at %Y-%m-%d %H:%M:%S UTC+8")},
                {"Retry-After": str(max(1, int(self.server.reset_at - time.time())))})
            return
        model = payload.get("model", MODEL)
        if mode.startswith(("function_", "custom_")):
            custom = mode.startswith("custom_")
            first, later = (FIRST_CUSTOM, LATER_CUSTOM) if custom else (FIRST_FUNCTION, LATER_FUNCTION)
            if mode.endswith("empty"):
                first, later = "", first + later
            function = {"name": "apply_patch" if custom else "lookup", "arguments": first}
            def frame(delta: dict[str, Any], finish: str | None = None) -> bytes:
                return smoke.encode({"id": "chatcmpl-sync", "object": "chat.completion.chunk",
                    "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
            frames = [frame({"role": "assistant", "tool_calls": [{"index": 0,
                "id": "call_sync", "type": "function", "function": function}]}),
                frame({"tool_calls": [{"index": 0, "function": {"arguments": later}}]}),
                frame({}, "tool_calls"), b"data: [DONE]\n\n"]
        else:
            frames = smoke.fixture_frames(model)
        raw = b"".join(frames)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        for frame in frames:
            self.wfile.write(frame)
            self.wfile.flush()


class SyncGateway(smoke.Gateway):
    """The base helper has no environment/stderr injection hook; override start only.

    Account setup, sanitization of messages, login, panel settings, HTTP requests
    and restart behavior are still inherited from the existing smoke helper.
    """

    def __init__(self, *args: Any, compat: str | None = None,
                 stderr_mode: str = "file", **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self.compat, self.stderr_mode = compat, stderr_mode
        self.launches: list[dict[str, Any]] = []

    def start(self) -> None:
        with socket.socket() as reservation:
            reservation.bind((HOST, 0))
            self.port = reservation.getsockname()[1]
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith(("WB_", "CODEBUDDY_", "WORKBUDDY_"))
               and key.upper() not in {"API_KEY", "ACCOUNTS_DIR", "HOST", "PORT", "HTTP_PROXY",
                   "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR"}}
        env.update({"NO_PROXY": "127.0.0.1,::1", "TZ": "UTC", "API_KEY": smoke.API_KEY})
        if self.compat is not None:
            env[ENV_COMPAT] = self.compat
        self.log = (self.root / "gateway.log").open("ab")
        args = [str(self.binary), "--host", HOST, "--port", str(self.port),
                "--accounts-dir", str(self.accounts), "--usage-dir", str(self.usage),
                "--offline", "--test-upstream", f"http://{HOST}:{self.upstream.port}"]
        self.launches.append({"args": args, "compat_env": self.compat, "stderr": self.stderr_mode})
        write_fd = None
        try:
            stderr: Any = subprocess.STDOUT
            if self.stderr_mode == "unread":
                stderr = subprocess.PIPE
            elif self.stderr_mode == "closed":
                read_fd, write_fd = os.pipe()
                os.close(read_fd)  # No reader exists even during child startup.
                stderr = write_fd
            self.process = subprocess.Popen(args, cwd=self.root, env=env, stdin=subprocess.DEVNULL,
                                            stdout=self.log, stderr=stderr, bufsize=0)
            if write_fd is not None:
                os.close(write_fd)
                write_fd = None
            deadline = time.monotonic() + self.startup_timeout
            last = "listener not ready"
            while time.monotonic() < deadline:
                check(self.process.poll() is None,
                      f"gateway exited with code {self.process.returncode}: {self.log_tail()}")
                try:
                    reply = smoke.request(self.port, "GET", "/health", timeout=0.5)
                    if reply.status == 200:
                        return
                    last = f"health HTTP {reply.status}"
                except (OSError, http.client.HTTPException) as exc:
                    last = str(exc)
                time.sleep(0.05)
            raise AssertionError(f"gateway startup timed out: {last}; {self.log_tail()}")
        except BaseException:
            self.stop()
            raise
        finally:
            if write_fd is not None:
                os.close(write_fd)

    def stop(self) -> None:
        # Never consume unread stderr, including on failure or shutdown. The base
        # helper gives forced cleanup a finite bound; it is NOT the graceful test.
        super().stop()
        if self.process is not None and self.process.stderr is not None:
            self.process.stderr.close()

    def graceful_shutdown(self) -> dict[str, Any]:
        check(self.process is not None and self.process.poll() is None, "gateway died before shutdown")
        if not self.token:
            self.login()
        started = time.monotonic()
        response = expect_status(smoke.request(self.port, "POST", "/shutdown", {},
            {"X-Panel-Token": self.token}, timeout=2), 200).json()
        check(response.get("shutting_down") is True, "panel did not acknowledge shutdown")
        remaining = SHUTDOWN_TIMEOUT - (time.monotonic() - started)
        code = self.process.wait(timeout=max(0.01, remaining))
        elapsed = time.monotonic() - started
        check(code == 0, f"graceful shutdown exited with {code}")
        check(elapsed < SHUTDOWN_TIMEOUT, f"graceful shutdown exceeded {SHUTDOWN_TIMEOUT}s")
        return {"exit_code": code, "seconds": round(elapsed, 3), "bound_seconds": SHUTDOWN_TIMEOUT,
                "method": "panel-auth POST /shutdown; stderr remained unread/closed"}


def chat(gateway: SyncGateway, model: str = MODEL) -> smoke.Reply:
    return gateway.call("POST", "/v1/chat/completions", {"model": model,
        "messages": [{"role": "user", "content": "Upstream sync offline fixture."}], "stream": False})


def one_chat(upstream: SyncFixture, mark: int) -> dict[str, Any]:
    rows = upstream.since(mark)
    calls = smoke.chat_calls(rows)
    check(len(rows) == len(calls) == 1, f"expected exactly one upstream chat, got {[r['path'] for r in rows]}")
    check(calls[0]["stream"] is True, "gateway did not request upstream SSE")
    return calls[0]


def responses_tool(g: SyncGateway, upstream: SyncFixture, custom: bool, empty: bool) -> dict[str, Any]:
    mark = upstream.choose(("custom_" if custom else "function_") + ("empty" if empty else "first"))
    tool = ({"type": "custom", "name": "apply_patch", "format": {"type": "text"}} if custom else
            {"type": "function", "name": "lookup", "parameters": {"type": "object",
                "properties": {"city": {"type": "string"}}, "required": ["city"]}})
    reply = g.call("POST", "/v1/responses", {"model": MODEL, "input": "Offline tool test.",
                                            "stream": True, "tools": [tool]})
    events, _ = smoke.sse(reply)
    check(not reply.incomplete, "Responses SSE was truncated")
    check(not any(e.get("type") in {"error", "response.failed"} for e in events), "Responses SSE failed")
    kind = "custom_tool_call_input" if custom else "function_call_arguments"
    field, item_type = ("input", "custom_tool_call") if custom else ("arguments", "function_call")
    first, later = (FIRST_CUSTOM, LATER_CUSTOM) if custom else (FIRST_FUNCTION, LATER_FUNCTION)
    expected_fragments = [first + later] if empty else [first, later]
    delta_kind = f"response.{kind}.delta"
    deltas = [e for e in events if e.get("type") == delta_kind]
    check([e.get("delta") for e in deltas] == expected_fragments,
          f"opening/later fragment missing or duplicated: {[e.get('delta') for e in deltas]!r}")
    raw_arguments = "".join(e["delta"] for e in deltas)
    check(raw_arguments == first + later, "argument concatenation changed bytes")
    added = [e for e in events if e.get("type") == "response.output_item.added"]
    finished = [e for e in events if e.get("type") == f"response.{kind}.done"]
    items = [e for e in events if e.get("type") == "response.output_item.done"]
    terminal = [e for e in events if e.get("type") == "response.completed"]
    check(len(added) == len(finished) == len(items) == len(terminal) == 1, "tool lifecycle was duplicated or omitted")
    check(added[0]["item"].get(field) == "", "opening item duplicated arguments outside delta events")
    check(events.index(added[0]) < events.index(deltas[0]) < events.index(finished[0])
          < events.index(items[0]) < events.index(terminal[0]), "tool event ordering changed")
    expected = json.loads(raw_arguments)["input"] if custom else raw_arguments
    check(finished[0].get(field) == expected, "done arguments do not match deltas exactly once")
    check(items[0]["item"].get(field) == expected, "completed item arguments mismatch")
    output = terminal[0]["response"].get("output", [])
    check(terminal[0]["response"].get("status") == "completed" and len(output) == 1,
          "Responses terminal envelope is not a single completed tool")
    check(output[0].get("type") == item_type and output[0].get(field) == expected,
          "final response lost/duplicated tool arguments")
    item_id = added[0]["item"]["id"]
    for event in [*deltas, *finished]:
        check(event.get("item_id") == item_id and event.get("output_index") == 0
              and event.get("call_id") == "call_sync", "tool event identity/index changed")
    check([e.get("sequence_number") for e in events] == list(range(1, len(events) + 1)),
          "Responses sequence numbers are not contiguous")
    one_chat(upstream, mark)
    return {"event_types": [e["type"] for e in events], "delta_fragments": expected_fragments,
            "terminal_value": expected, "upstream_chat_requests": 1}


def blocked_model(g: SyncGateway, upstream: SyncFixture) -> int:
    mark = upstream.choose("normal")
    reply = chat(g)
    # Selection exhaustion is 503 in the gateway; the original upstream response
    # remains 429. Either blocked status is valid, but never an upstream request.
    expect_status(reply, (429, 503))
    check(reply.json().get("error"), "model cooldown lacked an error envelope")
    check(not upstream.since(mark), "cooled model contacted upstream before reset")
    return reply.status


def successful_chat(g: SyncGateway, upstream: SyncFixture, model: str) -> None:
    mark = upstream.choose("normal")
    smoke.normal_result(chat(g, model), False)
    row = one_chat(upstream, mark)
    check(row["model"] == model, f"model unexpectedly rerouted to {row['model']}")


def model_window_restore(g: SyncGateway, upstream: SyncFixture) -> dict[str, Any]:
    # Establish a real first process, then stop and emulate an upstream/Python
    # sidecar update; no request sees an artificially edited in-memory cooldown.
    g.start()
    g.stop()
    reset = int(time.time()) + 9
    sidecar = g.accounts / "model_windows.json"
    row = {"uid": smoke.UID, "model": MODEL, "reset_at": reset,
           "attempts": 0, "last_try": 0, "last_result": ""}
    sidecar.write_text(json.dumps({"version": 1, "saved_at": "offline fixture", "windows": [row]}), encoding="utf-8")
    original = sidecar.read_bytes()
    g.start()
    check(reset > time.time() + 1, "startup consumed the seeded test window")
    statuses = [blocked_model(g, upstream)]
    successful_chat(g, upstream, OTHER_MODEL)
    statuses.append(blocked_model(g, upstream))
    check(sidecar.read_bytes() == original, "blocked/unrelated requests rewrote future window")
    mark = upstream.choose("normal")
    delay = max(0.0, reset + 0.25 - time.time())
    check(delay <= 10, "window wait exceeded the test bound")
    time.sleep(delay)
    check(not upstream.since(mark), "offline gateway probed upstream unsolicited at reset")
    successful_chat(g, upstream, MODEL)
    return {"model": MODEL, "unrelated_model": OTHER_MODEL, "reset_at": reset,
            "blocked_statuses": statuses, "window_wait_seconds": round(delay, 3),
            "upstream_before_reset_for_cooled_model": 0, "available_after_reset": True}


def recorded_window_restart(g: SyncGateway, upstream: SyncFixture) -> dict[str, Any]:
    g.start()
    upstream.reset_at = int(time.time()) + 90
    mark = upstream.choose("window_429")
    reply = expect_status(chat(g), 429)
    check(reply.json().get("error"), "upstream 429 lacked gateway error envelope")
    one_chat(upstream, mark)
    sidecar = g.accounts / "model_windows.json"
    check(sidecar.is_file(), "real upstream 429 did not persist model_windows.json")
    document = json.loads(sidecar.read_text(encoding="utf-8-sig"))
    windows = document.get("windows", [])
    rows = list(windows.values()) if isinstance(windows, dict) else windows
    matches = [r for r in rows if r.get("uid") == smoke.UID and r.get("model") == MODEL]
    check(len(matches) == 1 and float(matches[0]["reset_at"]) == upstream.reset_at,
          "429 persisted wrong/duplicate model window or parsed UTC+8 incorrectly")
    before = sidecar.read_bytes()
    account = json.loads(g.account_file.read_text(encoding="utf-8"))
    check(not account.get("modelCaps", {}).get(MODEL), "account modelCaps could mask sidecar restoration")
    blocked_model(g, upstream)
    g.restart()
    status = blocked_model(g, upstream)
    successful_chat(g, upstream, OTHER_MODEL)
    check(sidecar.read_bytes() == before, "restart or unrelated model moved reset timestamp")
    return {"persisted_window": matches[0], "restart_blocked_status": status,
            "upstream_429_requests": 1, "upstream_after_restart_for_cooled_model": 0,
            "unrelated_model_succeeded": True}


# Ignore only inherently fresh request IDs/timestamps, retaining header names and
# trace shape. Compatibility must not add arbitrary headers or change the body.
FRESH_HEADERS = {"x-conversation-id", "x-conversation-request-id", "x-conversation-message-id",
    "x-root-request-id", "x-request-id", "x-request-trace-id", "x-trace-id", "x-b3-traceid",
    "x-b3-spanid", "x-b3-parentspanid", "monitor_promptpreparestarttime", "monitor_httpsendtime"}
COMPAT_HEADERS = {"x-b3-parentspanid", "x-stainless-lang", "x-stainless-package-version",
    "x-stainless-os", "x-stainless-arch", "x-stainless-runtime", "x-stainless-runtime-version",
    "x-stainless-retry-count"}
# Upstream d1a8913:wb_accounts.py:1847-1866 selects the SDK, not web profile.
BROWSER_HEADERS = {"x-requested-with", "origin", "referer", "x-codebuddy-request", "accept-language"}


def comparable_headers(headers: dict[str, str]) -> dict[str, Any]:
    result: dict[str, Any] = dict(headers)
    for name in FRESH_HEADERS & result.keys():
        result[name] = "[fresh]"
    for name in ["b3", "traceparent"]:
        if name in result:
            result[name] = [len(part) for part in result[name].split("-")]
    return result


def capture_identity(g: SyncGateway, upstream: SyncFixture, compat: str | None,
                     product: str) -> dict[str, Any]:
    g.stop()
    g.compat = compat
    account = smoke.dummy_account()
    account["product"] = product
    g.account_file.write_text(json.dumps(account), encoding="utf-8")
    g.start()
    mark = upstream.choose("normal")
    smoke.normal_result(chat(g), False)
    return one_chat(upstream, mark)


def desktop_compat(g: SyncGateway, upstream: SyncFixture) -> dict[str, Any]:
    base = capture_identity(g, upstream, None, "workbuddy")
    check(not any(k.startswith("x-stainless-") for k in base["headers"]), "desktop compatibility default is not off")
    check(len(base["headers"]["b3"].split("-")) == 3, "default desktop b3 shape changed")
    # Upstream d1a8913:wb_identity.py:118-120 trims before comparing to "1".
    values: list[str | None] = [None, "", "0", "true", "yes", "01", "1", " 1", " 1 "]
    for value in values[1:]:
        current = capture_identity(g, upstream, value, "workbuddy")
        check(current["payload"] == base["payload"]
              and current["wire_body_sha256"] == base["wire_body_sha256"],
              f"desktop env={value!r} changed request body")
        h = current["headers"]
        if value.strip() != "1":
            check(comparable_headers(h) == comparable_headers(base["headers"]),
                  f"env value {value!r} does not trim to '1' but enabled compatibility or changed headers")
            continue
        check(set(h) - set(base["headers"]) == COMPAT_HEADERS, "exact1 added unexpected compatibility headers")
        check(set(base["headers"]) - set(h) == BROWSER_HEADERS, "exact1 did not remove exactly the browser headers")
        check(h.get("accept") == "application/json", "desktop SDK profile has wrong Accept header")
        check(h.get("x-stainless-lang") == "js" and h.get("x-stainless-runtime") == "node"
              and h.get("x-stainless-retry-count") == "0", "desktop SDK header values missing")
        check("x-stainless-timeout" not in h and "x-agent-runtime" not in h,
              "desktop adopted CLI timeout/runtime identity")
        b3 = h["b3"].split("-")
        check(len(b3) == 4 and b3[3] == h["x-b3-parentspanid"], "desktop b3 parent mismatch")
        check(b3[0] == h["x-b3-traceid"] and b3[1] == h["x-b3-spanid"], "trace header IDs disagree")
        unchanged = {k: v for k, v in comparable_headers(h).items() if k not in COMPAT_HEADERS | {"b3", "accept"}}
        original = {k: v for k, v in comparable_headers(base["headers"]).items()
                    if k not in BROWSER_HEADERS | {"b3", "accept"}}
        check(unchanged == original, "exact1 changed unrelated desktop request headers")
    return {"tested_env_values": values, "body_sha256": base["wire_body_sha256"],
            "exact1_added_headers": sorted(COMPAT_HEADERS), "body_unchanged": True,
            "env_rule": "strip(value) == '1'", "removed_browser_headers": sorted(BROWSER_HEADERS),
            "enabled_accept": "application/json",
            "upstream_references": ["d1a8913:wb_identity.py:118-120", "d1a8913:wb_accounts.py:1847-1866"]}


def non_desktop_compat(g: SyncGateway, upstream: SyncFixture) -> dict[str, Any]:
    details = {}
    for product in ["cli", "vscode"]:
        base = capture_identity(g, upstream, None, product)
        enabled = capture_identity(g, upstream, "1", product)
        check(base["payload"] == enabled["payload"]
              and base["wire_body_sha256"] == enabled["wire_body_sha256"], f"compat changed {product} body")
        check(comparable_headers(base["headers"]) == comparable_headers(enabled["headers"]),
              f"desktop compatibility changed {product} headers")
        check(enabled["headers"].get("x-ide-type", "").lower() == product,
              f"desktop compatibility switched {product} identity")
        details[product] = {"body_sha256": base["wire_body_sha256"], "headers_unchanged": True}
    return details


def logger_pipe(g: SyncGateway, upstream: SyncFixture, closed: bool) -> dict[str, Any]:
    g.stderr_mode = "closed" if closed else "unread"
    offered_bytes = 0
    for index in range(INVALID_FILES):
        path = g.accounts / f"invalid-{index:04d}.json"
        path.write_bytes(b"{not valid json")
        offered_bytes += len(f"[WARN] skipped account file: {path}\n".encode())
    check(offered_bytes > 65536, "startup log workload does not exceed 64KiB")
    mark = upstream.choose("normal")
    g.start()
    health = expect_status(smoke.request(g.port, "GET", "/health", timeout=2), 200).json()
    check(health.get("ok") is True and health.get("accounts") == 1, "log pressure broke startup/valid account loading")
    g.login()
    started = time.monotonic()
    logs = expect_status(smoke.request(g.port, "GET", "/logs?limit=20",
        headers={"X-Panel-Token": g.token}, timeout=2), 200).json()
    panel_seconds = time.monotonic() - started
    accounts = expect_status(smoke.request(g.port, "GET", "/accounts",
        headers={"X-Panel-Token": g.token}, timeout=2), 200).json()
    check(len(accounts.get("accounts", [])) == 1, "panel account view failed under stderr pressure")
    if closed:
        check(logs.get("stderr_closed") is True, "closed-pipe error was not observed by logger")
    else:
        check(g.process is not None and g.process.stderr is not None and not g.process.stderr.closed,
              "unread stderr pipe is not open")
        check(logs.get("stderr_closed") is False and logs.get("stderr_dropped", 0) > 0,
              "unread pipe did not fill bounded logger queue (no dropped lines observed)")
    check(logs.get("logs"), "panel in-memory logs disappeared when stderr was blocked/closed")
    check(not upstream.since(mark), "offline startup/panel/shutdown unexpectedly contacted upstream")
    shutdown = g.graceful_shutdown()
    return {"invalid_files": INVALID_FILES, "offered_warning_bytes": offered_bytes,
            "stderr_dropped": logs.get("stderr_dropped"), "stderr_closed": logs.get("stderr_closed"),
            "panel_response_seconds": round(panel_seconds, 3), "stderr_bytes_read": 0,
            "health_and_panel_responded": True, "shutdown": shutdown}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, required=True, help="already-built main-ready gateway; never built here")
    parser.add_argument("--report", type=Path, default=Path(__file__).with_name("upstream-sync-smoke-results.json"))
    parser.add_argument("--startup-timeout", type=float, default=20)
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"binary does not exist: {args.binary}")
    if not 0 < args.startup_timeout <= 120:
        parser.error("--startup-timeout must be in (0, 120]")
    binary = args.binary.resolve()
    if args.report.resolve() in {binary, Path(__file__).resolve(), Path(smoke.__file__).resolve()}:
        parser.error("report must not overwrite executable or smoke test sources")
    blocked = smoke.install_socket_guard()
    before_source, before_binary = source_version(), file_version(binary)
    started = time.monotonic()
    started_at = datetime.now(timezone.utc).isoformat()
    results: list[dict[str, Any]] = []
    launches: list[dict[str, Any]] = []
    upstream: SyncFixture | None = None
    interrupted = False

    with tempfile.TemporaryDirectory(prefix="wb2api-upstream-sync-") as temporary:
        root = Path(temporary)
        try:
            upstream = SyncFixture()

            @contextmanager
            def gateway(name: str) -> Iterator[SyncGateway]:
                directory = root / name
                directory.mkdir(mode=0o700)
                g = SyncGateway(binary, upstream, directory, args.startup_timeout)
                try:
                    yield g
                finally:
                    g.stop()
                    launches.extend(g.launches)

            def run(name: str, action: Callable[[SyncGateway, SyncFixture], dict[str, Any]],
                    start: bool = False) -> None:
                at = time.monotonic()
                row: dict[str, Any] = {"name": name}
                mark = len(upstream.since(0))
                with gateway(name) as g:
                    try:
                        if start:
                            g.start()
                        row["evidence"] = action(g, upstream)
                        check(not upstream.unexpected, str(upstream.unexpected))
                        check(not blocked, "; ".join(blocked))
                        # Each case explicitly accounts for its traffic; no refresh,
                        # billing, catalog or background endpoint belongs in this suite.
                        check(all(r["path"] == "/v2/chat/completions" for r in upstream.since(mark)),
                              "unexpected upstream traffic outside chat fixture")
                        row["status"] = "passed"
                    except Exception as exc:
                        row["status"] = "failed"
                        row["error"] = g.redact(f"{type(exc).__name__}: {exc}")
                        row["gateway_log_tail"] = g.log_tail()
                row["duration_seconds"] = round(time.monotonic() - at, 3)
                results.append(row)
                print(f"{row['status'].upper()} {name}" + (f": {row['error']}" if "error" in row else ""), flush=True)

            for custom in [False, True]:
                for empty in [False, True]:
                    name = f"responses_{'custom' if custom else 'function'}_{'empty_opening' if empty else 'first_and_later'}"
                    run(name, lambda g, u, c=custom, e=empty: responses_tool(g, u, c, e), start=True)
            run("model_window_sidecar_restart_until_reset", model_window_restore)
            run("upstream_429_persists_window_restart", recorded_window_restart)
            run("desktop_compat_exact1_headers_only", desktop_compat)
            run("desktop_compat_does_not_change_cli_vscode", non_desktop_compat)
            run("unread_stderr_startup_panel_graceful_shutdown", lambda g, u: logger_pipe(g, u, False))
            run("closed_stderr_startup_panel_graceful_shutdown", lambda g, u: logger_pipe(g, u, True))
        except KeyboardInterrupt:
            interrupted = True
            results.append({"name": "interrupted", "status": "failed", "error": "interrupted by user"})
        except Exception as exc:
            results.append({"name": "harness", "status": "failed", "error": f"{type(exc).__name__}: {exc}"})
        finally:
            if upstream is not None:
                upstream.close()

    after_source, after_binary = source_version(), file_version(binary)
    stable = before_source == after_source and before_binary == after_binary
    integrity_errors = []
    if not stable:
        integrity_errors.append("source/test/helper or binary changed during execution; result is not a stable version run")
    if blocked:
        integrity_errors.append("Python loopback audit guard rejected a network operation")
    if upstream and upstream.unexpected:
        integrity_errors.append("unexpected upstream fixture requests/handler failures")
    failed = sum(row["status"] == "failed" for row in results)
    report = {"suite": TEST_VERSION, "started_at_utc": started_at,
        "duration_seconds": round(time.monotonic() - started, 3),
        "python": sys.version, "platform": platform.platform(), "offline": True,
        "network_guard": "Imported Python loopback-only audit; child --offline + numeric-loopback --test-upstream (not an OS sandbox)",
        "binary": before_binary, "source_version": before_source, "versions_stable": stable,
        "build_provenance_note": "Hashes identify exact bytes executed and source present at run time; no claim that this script built the binary.",
        "passed": len(results) - failed, "failed": failed, "test_count": len(results),
        "status": "passed" if not failed and not integrity_errors and len(results) == 10 else "failed",
        "results": results, "integrity_errors": integrity_errors, "launches": launches,
        "blocked_python_network": blocked, "upstream_requests": upstream.records if upstream else [],
        "unexpected_fixture_requests": upstream.unexpected if upstream else []}
    if not stable:
        report["source_version_after"] = after_source
        report["binary_after"] = after_binary
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"Report: {args.report.resolve()}", flush=True)
    print(f"Upstream-sync smoke: {report['passed']} passed, {failed} failed; versions_stable={stable}", flush=True)
    return 130 if interrupted else int(report["status"] != "passed")


if __name__ == "__main__":
    raise SystemExit(main())
