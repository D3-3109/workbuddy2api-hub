#!/usr/bin/env python3
r"""Offline blackbox smoke checks for an already-built wb2api (stdlib only).

    python tests/gateway_smoke.py --binary /absolute/path/to/wb2api \
        --report tests/gateway-smoke-results.json

Never builds the binary or reads real accounts. All state is temporary. The child
gets --offline, a numeric loopback --test-upstream, and a sanitized environment.
The Python socket audit guard rejects non-loopback connections and DNS lookups.
IMPORTANT: Python audit hooks do not sandbox a native child. Native egress safety
also depends on the gateway honoring those flags; use an OS network namespace or
firewall if you need an independent process-level network isolation guarantee.
"""
from __future__ import annotations

import argparse
import base64
import gzip
import http.client
import ipaddress
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable
from urllib.parse import parse_qs, urlsplit

HOST = "127.0.0.1"
API_KEY = "test-key"
UID = "test-user"
OAUTH_UID = "oauth-test-user"
OAUTH_START_PATH = "/v2/plugin/auth/state"
OAUTH_POLL_PATH = "/v2/plugin/auth/token"
MODEL = "deepseek-v4.1-flash"
TEXT = "Hello from the offline fixture."
HARMLESS_TEXT = "The example text says unusual activity 已被限制, but this is ordinary assistant content."
REFRESH_PATH = "/v2/plugin/auth/token/refresh"
RESOURCE_PATH = "/v2/billing/meter/get-user-resource"
SUMMARY_PATH = "/billing/meter/get-user-resource-summary"
MAX_BODY = 4 * 1024 * 1024


def check(condition: Any, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def fake_jwt(generation: str = "initial", uid: str = UID) -> str:
    def part(value: Any) -> str:
        return base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")
    return ".".join((part({"alg": "HS256", "typ": "JWT"}),
                     part({"sub": uid, "exp": 4102444800, "generation": generation}),
                     "offline-fake-signature"))


def dummy_account() -> dict[str, Any]:
    return {"uid": UID, "realm": "intl", "accessToken": fake_jwt(),
            "refreshToken": "offline-dummy-refresh", "product": "workbuddy",
            "nickname": "Offline fixture", "enabled": True}


def install_socket_guard() -> list[str]:
    blocked: list[str] = []

    def loopback(host: Any) -> bool:
        if isinstance(host, bytes):
            host = host.decode("ascii", "strict")
        try:
            return ipaddress.ip_address(str(host)).is_loopback
        except ValueError:
            return False

    def audit(event: str, args: tuple[Any, ...]) -> None:
        host = None
        if event in {"socket.connect", "socket.bind"}:
            address = args[1]
            host = address[0] if isinstance(address, tuple) else address
        elif event == "socket.sendto":
            address = args[-1]
            host = address[0] if isinstance(address, tuple) else address
        elif event in {"socket.getaddrinfo", "socket.gethostbyname", "socket.gethostbyaddr"}:
            host = args[0]
        elif event == "socket.getnameinfo":
            host = args[0][0]
        else:
            return
        if not loopback(host):
            message = f"offline guard blocked {event} to non-loopback host {host!r}"
            blocked.append(message)
            raise PermissionError(message)

    sys.addaudithook(audit)
    return blocked


@dataclass
class Reply:
    status: int
    headers: dict[str, str]
    body: bytes
    incomplete: bool = False

    @property
    def text(self) -> str:
        return self.body.decode("utf-8", "replace")

    def json(self) -> Any:
        check(not self.incomplete, "incomplete HTTP response")
        try:
            return json.loads(self.body)
        except (ValueError, UnicodeError) as exc:
            raise AssertionError(f"HTTP {self.status} was not JSON: {self.text[:300]}") from exc


def request(port: int, method: str, path: str, payload: Any = None,
            headers: dict[str, str] | None = None, timeout: float = 10) -> Reply:
    conn = http.client.HTTPConnection(HOST, port, timeout=timeout)
    body = None if payload is None else json.dumps(payload).encode()
    supplied = {"Connection": "close", **(headers or {})}
    if body is not None:
        supplied["Content-Type"] = "application/json"
    try:
        conn.request(method, path, body=body, headers=supplied)
        response = conn.getresponse()
        incomplete = False
        try:
            data = response.read(MAX_BODY + 1)
        except http.client.IncompleteRead as exc:
            data, incomplete = exc.partial, True
        check(len(data) <= MAX_BODY, "gateway response exceeded smoke-test bound")
        return Reply(response.status, dict((k.lower(), v) for k, v in response.getheaders()),
                     data, incomplete)
    finally:
        conn.close()


def expect_status(reply: Reply, expected: int | tuple[int, ...]) -> Reply:
    allowed = (expected,) if isinstance(expected, int) else expected
    check(reply.status in allowed,
          f"expected HTTP {allowed}, got {reply.status}: {reply.text[:400]}")
    return reply


def sse(reply: Reply) -> tuple[list[dict[str, Any]], int]:
    expect_status(reply, 200)
    check("text/event-stream" in reply.headers.get("content-type", ""), "expected SSE content type")
    events, done = [], 0
    for block in reply.text.replace("\r\n", "\n").split("\n\n"):
        data = "\n".join(line[5:].lstrip(" ") for line in block.splitlines() if line.startswith("data:"))
        if not data:
            continue
        if data == "[DONE]":
            done += 1
        else:
            value = json.loads(data)
            check(isinstance(value, dict), "SSE event must contain a JSON object")
            events.append(value)
    return events, done


def fixture_frames(model: str, mixed: bool = False) -> list[bytes]:
    def frame(delta: dict[str, Any], finish: str | None = None) -> bytes:
        return encode({"id": "chatcmpl-fixture", "object": "chat.completion.chunk",
                       "created": 1700000000, "model": model,
                       "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    frames = [b": offline fixture\r\n\r\n", frame({"role": "assistant"})]
    if mixed:
        frames.append(frame({"reasoning_content": "Fixture reasoning."}))
    frames.extend([frame({"content": "Hello from "}), frame({"content": "the offline fixture."})])
    if mixed:
        frames.extend([
            frame({"tool_calls": [{"index": 0, "id": "call_fixture", "type": "function",
                                    "function": {"name": "lookup", "arguments": '{"city":'}}]}),
            frame({"tool_calls": [{"index": 0, "function": {"arguments": '"Paris"}'}}]}),
        ])
    frames.append(frame({}, "tool_calls" if mixed else "stop"))
    frames.append(encode({"id": "chatcmpl-fixture", "object": "chat.completion.chunk",
                          "model": model, "choices": [],
                          "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}}))
    frames.append(b"data: [DONE]\n\n")
    return frames


def encode(value: Any) -> bytes:
    return ("data: " + json.dumps(value, separators=(",", ":")) + "\n\n").encode()


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False

    def __init__(self) -> None:
        self.mode = "normal"
        self.records: list[dict[str, Any]] = []
        self.unexpected: list[str] = []
        self.lock = threading.Lock()
        super().__init__((HOST, 0), FixtureHandler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self) -> int:
        return self.server_address[1]

    def choose(self, mode: str) -> int:
        with self.lock:
            self.mode = mode
            return len(self.records)

    def since(self, mark: int) -> list[dict[str, Any]]:
        with self.lock:
            return self.records[mark:].copy()

    def close(self) -> None:
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)

    def handle_error(self, request: Any, client_address: Any) -> None:
        with self.lock:
            self.unexpected.append(f"fixture handler exception: {sys.exc_info()[1]}")


class FixtureHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server: FixtureServer

    def log_message(self, format: str, *args: Any) -> None:
        pass

    def send_json(self, status: int, value: Any, extra: dict[str, str] | None = None) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        for name, value in (extra or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_GET(self) -> None:
        path = urlsplit(self.path).path
        with self.server.lock:
            self.server.records.append({"path": path, "mode": self.server.mode, "method": "GET"})
        if path == "/v3/config":
            self.send_json(200, {"code": 0, "data": {"models": []}})
        elif path == OAUTH_POLL_PATH:
            if parse_qs(urlsplit(self.path).query).get("state") != ["remote-state"]:
                self.send_json(400, {"code": 400, "msg": "wrong fixture upstream state"})
            else:
                self.send_json(200, {"code": 0, "data": {"accessToken": fake_jwt(uid=OAUTH_UID),
                    "refreshToken": "offline-oauth-refresh", "expiresAt": 4102444800}})
        else:
            with self.server.lock:
                self.server.unexpected.append(f"GET {path}")
            self.send_json(404, {"error": "unknown offline fixture path"})

    def do_POST(self) -> None:
        try:
            self.handle_post()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            # Intentional truncated streams and downstream cancellation close sockets.
            pass

    def handle_post(self) -> None:
        self.connection.settimeout(10)
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 <= length <= MAX_BODY:
            self.send_json(413, {"error": "fixture input too large"})
            return
        body = self.rfile.read(length)
        if self.headers.get("Content-Encoding", "").lower() == "gzip":
            body = gzip.decompress(body)
        payload = json.loads(body or b"{}")
        path = urlsplit(self.path).path
        with self.server.lock:
            mode = self.server.mode
            self.server.records.append({"path": path, "mode": mode, "model": payload.get("model"),
                                        "stream": payload.get("stream"),
                                        "refreshed_auth": fake_jwt("refreshed") in self.headers.get("Authorization", "")})
            refreshed = any(r["path"] == REFRESH_PATH for r in self.server.records)
        if path == OAUTH_START_PATH:
            if parse_qs(urlsplit(self.path).query).get("platform") != ["CLI"]:
                self.send_json(400, {"code": 400, "msg": "wrong fixture OAuth platform"})
            else:
                self.send_json(200, {"code": 0, "data": {"state": "remote-state",
                    "authUrl": "https://example.invalid/login"}})
            return
        if path == RESOURCE_PATH:
            if mode == "billing_malformed":
                self.send_json(200, {"code": 0, "data": {"Response": {"Data": {"Accounts": [
                    {"PackageName": "Broken fixture", "CapacityRemain": "not-a-number"}
                ], "TotalCount": 1}}}})
            else:
                packages = [
                    {"AccountId": "offline-pro", "PackageName": "Offline Pro", "PackageCode": "fixture-pro",
                     "CapacitySize": 999, "CapacityRemain": 999,
                     "CycleCapacitySizePrecise": "100.00", "CycleCapacityRemainPrecise": "75.25",
                     "CycleCapacityUsedPrecise": "24.75", "CapacityUnit": "credits",
                     "CycleEndTime": "2099-01-01 00:00:00", "Status": 0, "InUsage": True},
                    {"AccountId": "offline-bonus", "PackageName": "Offline Bonus", "PackageCode": "fixture-bonus",
                     "CapacitySize": 20, "CapacityRemain": 10, "CapacityUsed": 10,
                     "CycleEndTime": "2099-01-01 00:00:00", "Status": 0},
                ]
                self.send_json(200, {"code": 0, "data": {"Response": {"Data": {
                    "Accounts": packages, "TotalCount": len(packages)}}}})
            return
        if path == SUMMARY_PATH:
            self.send_json(200, {"code": 0, "data": {"IsPaidUser": True,
                "SubscriptionPackageName": "Offline Pro", "SubscriptionPackageCode": "fixture-pro",
                "IsProtectedPriceUser": False, "ProTrialStatus": 0, "Packages": []}})
            return
        if path == REFRESH_PATH:
            self.send_json(200, {"code": 0, "data": {"accessToken": fake_jwt("refreshed"),
                                                     "refreshToken": "offline-refreshed-refresh"}})
            return
        if path != "/v2/chat/completions":
            with self.server.lock:
                self.server.unexpected.append(f"POST {path}")
            self.send_json(404, {"error": "unknown offline fixture path"})
            return
        if mode == "restricted":
            self.send_json(403, {"code": 11140, "msg": "unusual activity (offline fixture)"})
            return
        if mode == "rate_limit":
            self.send_json(429, {"code": 429, "msg": "offline rate limit"}, {"Retry-After": "2"})
            return
        if mode == "refresh" and not refreshed:
            self.send_json(401, {"code": 401, "msg": "offline expired token"})
            return
        frames = fixture_frames(payload.get("model", MODEL), mixed=mode == "mixed")
        if mode == "harmless_restriction_text":
            # Finish and DONE stay valid: keywords in ordinary content are not errors.
            frames[2] = encode({"id": "chatcmpl-fixture", "object": "chat.completion.chunk",
                "model": payload.get("model", MODEL), "choices": [{"index": 0,
                "delta": {"content": HARMLESS_TEXT}, "finish_reason": None}]})
            del frames[3]
        if mode == "sse_error":
            frames = frames[:4] + [encode({"error": {"code": "offline_error", "message": "fixture stream error"}})]
        truncated = mode in {"truncated", "chunked_truncated"}
        chunked = mode in {"chunked", "chunked_truncated"}
        raw = b"".join(frames[:4] if truncated else frames)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Connection", "close")
        if chunked:
            self.send_header("Transfer-Encoding", "chunked")
        else:
            self.send_header("Content-Length", str(len(raw) + (500 if truncated else 0)))
        self.end_headers()
        self.close_connection = True
        if chunked:
            # Split CRLF, JSON, and data prefixes across arbitrary HTTP chunk boundaries.
            offset = 0
            for size in [1, 2, 7, 3, 17] * (len(raw) // 30 + 1):
                chunk = raw[offset:offset + size]
                if not chunk:
                    break
                self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                offset += len(chunk)
            if not truncated:
                self.wfile.write(b"0\r\n\r\n")
        elif mode in {"slow", "stall"}:
            for index, frame in enumerate(frames):
                self.wfile.write(frame)
                self.wfile.flush()
                if index == 2 and mode == "stall":
                    time.sleep(2.5)
                elif mode == "slow":
                    time.sleep(0.06)
        else:
            self.wfile.write(raw)
        self.wfile.flush()
        if truncated:
            self.connection.shutdown(socket.SHUT_WR)


class Gateway:
    def __init__(self, binary: Path, upstream: FixtureServer, root: Path, startup_timeout: float,
                 launcher_key: bool = True) -> None:
        self.binary, self.upstream, self.root = binary, upstream, root
        self.startup_timeout = startup_timeout
        self.launcher_key = launcher_key
        self.accounts = root / "accounts"
        self.usage = root / "usage"
        self.accounts.mkdir(mode=0o700)
        self.usage.mkdir(mode=0o700)
        # Match the library's safe-UID canonical persistence filename.
        self.account_file = self.accounts / f"{UID}.json"
        self.account_file.write_text(json.dumps(dummy_account()), encoding="utf-8")
        self.account_file.chmod(0o600)
        self.process: subprocess.Popen[bytes] | None = None
        self.log: Any = None
        self.port = 0
        self.token = ""
        self.secrets = [API_KEY, fake_jwt(), fake_jwt("refreshed"), "offline-dummy-refresh", "offline-refreshed-refresh"]

    def start(self) -> None:
        with socket.socket() as reservation:
            reservation.bind((HOST, 0))
            self.port = reservation.getsockname()[1]
        # Do not inherit real credentials, proxy routing, realm switches, or TLS overrides.
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith(("WB_", "CODEBUDDY_", "WORKBUDDY_"))
               and key.upper() not in {"API_KEY", "ACCOUNTS_DIR", "HOST", "PORT", "HTTP_PROXY",
                                       "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR"}}
        env.update({"NO_PROXY": "127.0.0.1,::1", "TZ": "UTC"})
        if self.launcher_key:
            env["API_KEY"] = API_KEY
        self.log = (self.root / "gateway.log").open("ab")
        args = [str(self.binary), "--host", HOST, "--port", str(self.port),
                "--accounts-dir", str(self.accounts), "--usage-dir", str(self.usage),
                "--offline", "--test-upstream", f"http://{HOST}:{self.upstream.port}"]
        try:
            self.process = subprocess.Popen(args, cwd=self.root, env=env, stdin=subprocess.DEVNULL,
                                            stdout=self.log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + self.startup_timeout
            last = "listener not ready"
            while time.monotonic() < deadline:
                check(self.process.poll() is None, f"gateway exited with code {self.process.returncode}: {self.log_tail()}")
                try:
                    reply = request(self.port, "GET", "/health", timeout=0.5)
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

    def stop(self) -> None:
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if self.log is not None:
            self.log.close()
            self.log = None

    def restart(self) -> None:
        self.stop()
        self.token = ""
        self.start()

    def redact(self, message: str) -> str:
        for secret in self.secrets:
            message = message.replace(secret, "[fixture-secret]")
        return message

    def log_tail(self) -> str:
        path = self.root / "gateway.log"
        return self.redact(path.read_text(encoding="utf-8", errors="replace")[-3000:]) if path.exists() else ""

    def call(self, method: str, path: str, body: Any = None, auth: str = "api") -> Reply:
        headers = {"X-Realm": "intl"}
        if auth == "api":
            headers["Authorization"] = f"Bearer {API_KEY}"
        elif auth == "panel":
            headers["X-Panel-Token"] = self.token
        elif auth == "panel_as_key":
            headers["Authorization"] = f"Bearer {self.token}"
        elif auth == "key_as_panel":
            headers["X-Panel-Token"] = API_KEY
        return request(self.port, method, path, body, headers)

    def login(self) -> None:
        value = expect_status(self.call("POST", "/panel/login", {"password": "admin"}, "none"), 200).json()
        check(value.get("using_default_password") is True, "isolated gateway did not use baseline default password")
        self.token = value.get("token", "")
        check(self.token and self.token != API_KEY, "panel login did not issue an independent token")
        self.secrets.append(self.token)

    def settings(self, values: dict[str, Any]) -> Any:
        return expect_status(self.call("POST", "/settings/save", values, "panel"), 200).json()

    def chat(self, stream: bool = False, auth: str = "api") -> Reply:
        return self.call("POST", "/v1/chat/completions",
                         {"model": MODEL, "messages": [{"role": "user", "content": "Offline smoke only."}],
                          "stream": stream}, auth)


def chat_calls(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [row for row in rows if row["path"] == "/v2/chat/completions"]


def normal_result(reply: Reply, streaming: bool, mixed: bool = False) -> None:
    expect_status(reply, 200)
    if streaming:
        events, done = sse(reply)
        check(not reply.incomplete and done == 1, "successful stream must end with exactly one DONE")
        check(not any("error" in event for event in events), "unexpected stream error")
        chunks = [choice for event in events for choice in event.get("choices", [])]
        text = "".join(choice.get("delta", {}).get("content", "") or "" for choice in chunks)
        check(text == TEXT, f"stream text mismatch: {text!r}")
        ids = {event["id"] for event in events if "id" in event}
        check(len(ids) == 1, f"stream changed completion id: {ids}")
        usage = next((event["usage"] for event in events if event.get("usage")), {})
        if mixed:
            check(any(choice.get("delta", {}).get("reasoning_content") for choice in chunks), "reasoning delta missing")
            calls = [tc for choice in chunks for tc in choice.get("delta", {}).get("tool_calls", [])]
            check(any(tc.get("function", {}).get("name") == "lookup" for tc in calls), "tool name missing")
            arguments = "".join(tc.get("function", {}).get("arguments", "") for tc in calls)
            check(json.loads(arguments) == {"city": "Paris"}, "tool argument fragments did not round-trip")
    else:
        value = reply.json()
        check(value.get("object") == "chat.completion", "nonstream object is not chat.completion")
        message = value["choices"][0]["message"]
        check(message.get("content") == TEXT, f"nonstream text mismatch: {message.get('content')!r}")
        usage = value.get("usage", {})
        if mixed:
            check(message.get("reasoning_content") == "Fixture reasoning.", "nonstream reasoning missing")
            tool = message["tool_calls"][0]["function"]
            check(tool["name"] == "lookup" and json.loads(tool["arguments"]) == {"city": "Paris"}, "nonstream tool mismatch")
    check(usage.get("prompt_tokens") == 11 and usage.get("completion_tokens") == 7 and usage.get("total_tokens") == 18,
          f"upstream usage not preserved: {usage}")


def run_suite(g: Gateway, upstream: FixtureServer, run: Callable[[str, Callable[[], Any]], None]) -> None:
    def health() -> None:
        reply = expect_status(g.call("GET", "/health", auth="none"), 200)
        value = reply.json()
        check(value.get("ok") is True and value.get("api_key_required") is True, "unexpected health flags")
        check(value.get("accounts") == 1, "dummy account was not loaded")
        for secret in [*g.secrets, UID, "credential_file", "accessToken", "refreshToken", "settings.json"]:
            check(secret not in reply.text, f"unauthenticated health exposed {secret[:20]!r}")

    def unauthorized() -> None:
        mark = upstream.choose("normal")
        expect_status(g.chat(auth="none"), 401)
        for path in ["/accounts", "/settings", "/v1/models"]:
            expect_status(g.call("GET", path, auth="none"), 401)
        expect_status(g.call("POST", "/accounts/import", {"data": dummy_account()}, "none"), 401)
        check(not upstream.since(mark), "unauthenticated request reached upstream")

    def separation() -> None:
        mark = upstream.choose("normal")
        expect_status(g.call("GET", "/accounts", auth="api"), 401)
        expect_status(g.call("GET", "/accounts", auth="key_as_panel"), 401)
        expect_status(g.call("POST", "/settings/save", {"enable_anthropic_messages": True}, "api"), 401)
        expect_status(g.chat(auth="panel_as_key"), 401)
        value = expect_status(g.call("GET", "/accounts", auth="panel"), 200).json()
        check(any(a.get("uid") == UID for a in value.get("accounts", [])), "panel account listing missing test user")
        check(fake_jwt() not in json.dumps(value), "account listing exposed access token")
        check(not upstream.since(mark), "rejected credentials reached upstream")

    def models() -> None:
        value = expect_status(g.call("GET", "/v1/models"), 200).json()
        check(value.get("object") == "list" and value.get("data"), "model catalog is empty")
        check(any(row.get("id") == MODEL for row in value["data"]), f"fixture model {MODEL} missing")

    def importing() -> None:
        mark = upstream.choose("normal")
        value = expect_status(g.call("POST", "/accounts/import",
                                    {"data": {"accounts": [dummy_account()]}, "overwrite": True}, "panel"), 200).json()
        check(value.get("ok", True) is not False, "account import reported failure")
        accounts = expect_status(g.call("GET", "/accounts", auth="panel"), 200).json()["accounts"]
        check(len(accounts) == 1 and accounts[0]["uid"] == UID, "same-UID import duplicated account")
        check(not upstream.since(mark), "offline import contacted upstream")

    def chat(mode: str, stream: bool, auth: str = "api") -> None:
        mark = upstream.choose(mode)
        normal_result(g.chat(stream, auth), stream, mode == "mixed")
        calls = chat_calls(upstream.since(mark))
        check(len(calls) == 1, f"chat unexpectedly replayed {len(calls)} times")
        check(calls[0]["stream"] is True, "gateway must request upstream SSE even for nonstream clients")

    def responses(stream: bool) -> None:
        mark = upstream.choose("normal")
        reply = expect_status(g.call("POST", "/v1/responses",
                                    {"model": MODEL, "input": "Offline smoke only.", "stream": stream}), 200)
        if stream:
            events, _ = sse(reply)
            check(any(event.get("type") == "response.completed" for event in events), "Responses terminal event missing")
            check(any(event.get("type") == "response.output_text.delta" for event in events), "Responses text delta missing")
        else:
            value = reply.json()
            check(value.get("object") == "response" and value.get("status") == "completed", "Responses result not completed")
            text = "".join(part.get("text", "") for item in value.get("output", []) for part in item.get("content", []))
            check(text == TEXT, f"Responses text mismatch: {text!r}")
        check(len(chat_calls(upstream.since(mark))) == 1, "Responses replayed or never reached fixture")

    def compaction(stream: bool) -> None:
        mark = upstream.choose("normal")
        reply = expect_status(g.call("POST", "/v1/responses/compact", {
            "model": MODEL, "input": [{"role": "user", "content": "Summarize this offline conversation."}],
            "stream": stream}), 200)
        if stream:
            events, _ = sse(reply)
            check([event.get("type") for event in events] == ["response.created", "response.in_progress",
                  "response.output_item.done", "response.completed"], "compaction SSE envelope mismatch")
            value = events[-1]["response"]
        else:
            value = reply.json()
        check(value.get("object") == "response" and value.get("status") == "completed", "compaction not completed")
        check(len(value.get("output", [])) == 1, "compaction must return one summary item")
        item = value["output"][0]
        check(item.get("type") == "compaction", "compaction item type missing")
        summary = json.loads(base64.b64decode(item["encrypted_content"], validate=True))
        check(summary == {"v": 1, "summary": TEXT}, "compaction portable summary payload mismatch")
        check(value.get("output_text") == "", "compaction unexpectedly returned ordinary output text")
        check(value.get("usage", {}).get("input_tokens") == 11, "compaction usage missing")
        check(len(chat_calls(upstream.since(mark))) == 1, "compaction replayed or did not reach fixture")

    def billing_detail() -> None:
        mark = upstream.choose("billing")
        before = json.loads(g.account_file.read_text(encoding="utf-8"))
        expect_status(g.call("POST", "/accounts/credits/detail", {"uid": UID}), 401)
        check(not upstream.since(mark), "API key was allowed to request panel billing refresh")
        value = expect_status(g.call("POST", "/accounts/credits/detail", {"uid": UID}, "panel"), 200).json()
        check(value.get("ok") is True and value.get("realm") == "intl", f"billing refresh failed: {value}")
        credits = value["credits"]
        check((credits.get("size"), credits.get("remain"), credits.get("used")) == (120, 85.25, 34.75),
              f"billing aggregate totals mismatch: {credits}")
        packages = {p["package_code"]: p for p in credits.get("packages", [])}
        check(set(packages) == {"fixture-pro", "fixture-bonus"}, "billing package list missing fixture entries")
        check(packages["fixture-pro"]["size"] == 100 and packages["fixture-pro"]["remain"] == 75.25,
              "cycle capacity did not take precedence over lifetime capacity")
        check(credits.get("is_paid_user") is True and value.get("tier", {}).get("package") == "Offline Pro",
              "billing summary tier was not applied")
        check([row["path"] for row in upstream.since(mark)] == [RESOURCE_PATH, SUMMARY_PATH],
              "intl billing did not use expected resource and summary endpoints")
        after = json.loads(g.account_file.read_text(encoding="utf-8"))
        check(after.get("credits", {}).get("remain") == 85.25, "billing credits were not persisted")
        for key in ["accessToken", "refreshToken", "uid", "realm"]:
            check(after.get(key) == before.get(key), f"billing update overwrote {key}")
        check(fake_jwt() not in json.dumps(value), "billing response exposed access token")

    def billing_cached() -> None:
        mark = upstream.choose("billing")
        value = expect_status(g.call("POST", "/accounts/credits/detail",
                                    {"uid": UID, "refresh": False}, "panel"), 200).json()
        check(value.get("ok") is True and value.get("credits", {}).get("remain") == 85.25,
              "cached billing detail did not preserve balance")
        check(not upstream.since(mark), "cached billing detail unexpectedly contacted upstream")

    def billing_malformed() -> None:
        before = json.loads(g.account_file.read_text(encoding="utf-8"))
        mark = upstream.choose("billing_malformed")
        value = expect_status(g.call("POST", "/accounts/credits/detail",
                                    {"uid": UID, "refresh": True}, "panel"), 200).json()
        check(value.get("ok") is False, "malformed billing payload was accepted")
        check(value.get("credits") == before.get("credits"), "failed billing refresh replaced known balance")
        after = json.loads(g.account_file.read_text(encoding="utf-8"))
        check(after.get("credits") == before.get("credits"), "malformed billing overwrote persisted credits")
        for key in ["accessToken", "refreshToken"]:
            check(after.get(key) == before.get(key), f"malformed billing changed {key}")
        check([row["path"] for row in upstream.since(mark)] == [RESOURCE_PATH],
              "malformed billing response triggered an unexpected follow-up/replay")

    def messages_gate() -> None:
        mark = upstream.choose("normal")
        reply = expect_status(g.call("POST", "/v1/messages", {"model": MODEL, "max_tokens": 32,
                              "messages": [{"role": "user", "content": "Offline only."}]}), 400)
        check("disabled" in reply.text.lower(), "Messages rejected for a reason other than explicit opt-in")
        check(not upstream.since(mark), "disabled Messages contacted upstream")
        settings = g.settings({"enable_anthropic_messages": True})
        check(settings.get("enable_anthropic_messages") is True, "Messages opt-in was not saved")

    def messages(stream: bool) -> None:
        mark = upstream.choose("normal")
        reply = expect_status(g.call("POST", "/v1/messages", {"model": MODEL, "max_tokens": 32,
                              "messages": [{"role": "user", "content": "Offline only."}], "stream": stream}), 200)
        if stream:
            events, _ = sse(reply)
            check(any(e.get("type") == "message_start" for e in events), "Messages start missing")
            check(any(e.get("type") == "message_stop" for e in events), "Messages stop missing")
            text = "".join(e.get("delta", {}).get("text", "") for e in events)
        else:
            value = reply.json()
            check(value.get("type") == "message" and value.get("role") == "assistant", "Messages envelope mismatch")
            text = "".join(part.get("text", "") for part in value.get("content", []))
        check(text == TEXT, f"Messages text mismatch: {text!r}")
        check(len(chat_calls(upstream.since(mark))) == 1, "Messages replayed or never reached fixture")

    def broken(mode: str, stream: bool) -> None:
        mark = upstream.choose(mode)
        reply = g.chat(stream)
        if stream and reply.status == 200:
            events, done = sse(reply)
            check(done == 0, "aborted stream emitted success DONE")
            check(reply.incomplete or any("error" in e or e.get("type") == "error" for e in events),
                  "aborted stream silently presented a successful response")
            check(not any(c.get("finish_reason") == "stop" for e in events for c in e.get("choices", [])),
                  "aborted stream emitted successful finish_reason")
        else:
            expect_status(reply, 502)
            check(reply.json().get("error"), "failed upstream lacks error envelope")
        check(len(chat_calls(upstream.since(mark))) == 1, "ambiguous/truncated POST was replayed")

    def idle_timeout() -> None:
        g.settings({"upstream": {"idle_timeout_seconds": 1}})
        try:
            broken("stall", True)
        finally:
            g.settings({"upstream": {"idle_timeout_seconds": 300}})

    def refresh() -> None:
        mark = upstream.choose("refresh")
        normal_result(g.chat(), False)
        rows = upstream.since(mark)
        check([r["path"] for r in rows] == ["/v2/chat/completions", REFRESH_PATH, "/v2/chat/completions"],
              f"401 refresh/retry sequence mismatch: {[r['path'] for r in rows]}")
        check(rows[-1]["refreshed_auth"], "retry did not use refreshed credentials")
        doc = json.loads(g.account_file.read_text(encoding="utf-8"))
        check(doc["accessToken"] == fake_jwt("refreshed"), "refreshed token was not persisted")

    def rate_limit() -> None:
        mark = upstream.choose("rate_limit")
        reply = expect_status(g.chat(), 429)
        check(int(reply.headers.get("retry-after", "0")) > 0, "429 missing positive Retry-After")
        check(reply.json().get("error"), "429 missing error envelope")
        check(len(chat_calls(upstream.since(mark))) == 1, "rate-limited account was replayed")

    def restriction() -> None:
        # Restart clears unrelated transient 429 cooldown, not persisted restrictions.
        g.restart()
        mark = upstream.choose("restricted")
        expect_status(g.chat(), 403)
        check(len(chat_calls(upstream.since(mark))) == 1, "11140 request was replayed")
        doc = json.loads(g.account_file.read_text(encoding="utf-8"))
        until = doc.get("restrictedUntil", 0)
        check(until > time.time(), "11140 restriction was not saved to account file")
        mark = upstream.choose("normal")
        reply = g.chat()
        check(reply.status >= 400, "restricted account was reused in-process")
        check(not upstream.since(mark), "restricted account retried upstream in-process")
        g.restart()
        g.login()
        mark = upstream.choose("normal")
        reply = g.chat()
        check(reply.status >= 400, "restricted account was reused after restart")
        check(not upstream.since(mark), "persisted restriction did not prevent upstream POST")
        after = json.loads(g.account_file.read_text(encoding="utf-8"))["restrictedUntil"]
        check(after == until, "blocked requests changed the persisted restriction deadline")
        accounts = expect_status(g.call("GET", "/accounts", auth="panel"), 200).json()["accounts"]
        check(accounts[0].get("restrictedUntil", 0) == until, "panel does not expose persisted restriction")

    def noncanonical_restart() -> None:
        root = g.root / "noncanonical"
        root.mkdir(mode=0o700)
        other = Gateway(g.binary, upstream, root, g.startup_timeout)
        other.secrets = g.secrets
        original = other.accounts / "operator-chosen-name.json"
        other.account_file.rename(original)
        other.account_file = original
        try:
            other.start()
            other.login()
            expect_status(other.call("POST", "/accounts/import",
                          {"data": {"accounts": [dummy_account()]}, "overwrite": True}, "panel"), 200)
            mark = upstream.choose("restricted")
            expect_status(other.chat(), 403)
            check(len(chat_calls(upstream.since(mark))) == 1, "noncanonical account request was replayed")
            documents = [(path.name, json.loads(path.read_text(encoding="utf-8")))
                         for path in other.accounts.glob("*.json")]
            copies = [(name, doc) for name, doc in documents if doc.get("uid") == UID]
            other.restart()
            mark = upstream.choose("normal")
            reply = other.chat()
            check(reply.status >= 400, "restart resurrected unrestricted noncanonical account")
            check(not upstream.since(mark), "stale account file allowed upstream POST after restart")
            other.login()
            accounts = expect_status(other.call("GET", "/accounts", auth="panel"), 200).json()["accounts"]
            check(len(accounts) == 1 and accounts[0]["uid"] == UID, "restart duplicated the imported account")
            check(accounts[0].get("restrictedUntil", 0) > time.time(), "restart lost noncanonical restriction")
            check(len(copies) == 1, f"import/save left duplicate credential files: {[name for name, _ in copies]}")
            check(copies[0][1].get("restrictedUntil", 0) > time.time(), "noncanonical account file is stale")
        finally:
            other.stop()

    def key_scope_and_revocation() -> None:
        root = g.root / "key-isolation"
        root.mkdir(mode=0o700)
        other = Gateway(g.binary, upstream, root, g.startup_timeout, launcher_key=False)
        other.secrets = g.secrets
        account_a = dummy_account()
        account_a["credits"] = {"remain": 11, "used": 0, "size": 11}
        account_b = dict(dummy_account(), uid="other-test-user", accessToken=fake_jwt(uid="other-test-user"),
                         credits={"remain": 99, "used": 0, "size": 99})
        other.account_file.write_text(json.dumps(account_a), encoding="utf-8")
        (other.accounts / "other-test-user.json").write_text(json.dumps(account_b), encoding="utf-8")
        scoped_key = "offline-account-a-key"
        g.secrets.append(scoped_key)
        try:
            other.start()
            other.login()
            other.settings({"api_keys": [{"id": "scope-a", "name": "Fixture scope A", "key": scoped_key,
                                          "enabled": True, "accounts": [UID], "realm": "intl"}]})
            headers = {"Authorization": f"Bearer {scoped_key}"}
            value = expect_status(request(other.port, "GET", "/v1/balance-summary", headers=headers), 200).json()
            check(value.get("balance") == 11 and value.get("accounts_enabled") == 1
                  and value.get("accounts_with_credit_data") == 1, f"scoped key saw another account's balance: {value}")
            mark = upstream.choose("normal")
            for path in ["/u/other-test-user/v1/balance-summary", "/u/other-test-user/v1/models"]:
                expect_status(request(other.port, "GET", path, headers=headers), 403)
            expect_status(request(other.port, "POST", "/u/other-test-user/v1/chat/completions",
                          {"model": MODEL, "messages": [{"role": "user", "content": "scope test"}]}, headers), 403)
            check(not upstream.since(mark), "forbidden pinned account reached upstream")
            other.settings({"api_keys": [{"id": "scope-a", "enabled": False}]})
            for auth in [None, headers]:
                expect_status(request(other.port, "GET", "/v1/models", headers=auth), 401)
                expect_status(request(other.port, "POST", "/v1/chat/completions",
                              {"model": MODEL, "messages": [{"role": "user", "content": "revoked key"}]}, auth), 401)
            health = expect_status(other.call("GET", "/health", auth="none"), 200).json()
            check(health.get("api_key_required") is True, "disabling the last key silently enabled anonymous access")
            other.restart()
            expect_status(request(other.port, "GET", "/v1/models"), 401)
            expect_status(request(other.port, "GET", "/v1/models", headers=headers), 401)
            other.login()
            other.settings({"auth_disabled": True})
            expect_status(request(other.port, "GET", "/v1/models"), 200)
            other.settings({"auth_disabled": False})
            expect_status(request(other.port, "GET", "/v1/models"), 401)
        finally:
            other.stop()

    def harmless_restriction_text() -> None:
        mark = upstream.choose("harmless_restriction_text")
        events, done = sse(g.chat(True))
        text = "".join(c.get("delta", {}).get("content", "") or ""
                       for e in events for c in e.get("choices", []))
        check(done == 1 and text == HARMLESS_TEXT, "ordinary assistant content triggered a restriction/error")
        check(not any("error" in e for e in events), "ordinary assistant content produced an error envelope")
        doc = json.loads(g.account_file.read_text(encoding="utf-8"))
        check(doc.get("restrictedUntil", 0) <= time.time(), "ordinary assistant text quarantined account")
        check(len(chat_calls(upstream.since(mark))) == 1, "ordinary assistant text caused a replay")
        upstream.choose("normal")
        normal_result(g.chat(), False)

    def oauth_login_and_cancel() -> None:
        g.secrets.extend([fake_jwt(uid=OAUTH_UID), "offline-oauth-refresh"])
        mark = upstream.choose("oauth")
        expect_status(g.call("POST", "/accounts/login/start", {"realm": "intl"}), 401)
        check(not upstream.since(mark), "API key started panel OAuth login")
        value = expect_status(g.call("POST", "/accounts/login/start",
                                     {"realm": "intl", "platform": "CLI"}, "panel"), 200).json()
        local_state = value.get("state", "")
        check(local_state and local_state != "remote-state", "OAuth did not issue a separate local state")
        check(value.get("authUrl") == "https://example.invalid/login", "OAuth authorization URL changed")
        # Authorization URL is opaque test data: never navigate or request it.
        poll_path = f"/accounts/login/poll?state={local_state}"
        expect_status(g.call("GET", poll_path, auth="api"), 401)
        value = expect_status(g.call("GET", poll_path, auth="panel"), 200).json()
        check(value.get("status") == "ok" and value.get("account", {}).get("uid") == OAUTH_UID,
              f"OAuth account import failed: {value}")
        check(fake_jwt(uid=OAUTH_UID) not in json.dumps(value), "OAuth poll exposed access token")
        persisted = json.loads((g.accounts / f"{OAUTH_UID}.json").read_text(encoding="utf-8"))
        check(persisted.get("accessToken") == fake_jwt(uid=OAUTH_UID), "OAuth token was not persisted")
        check(persisted.get("tasksEnabled") is False, "OAuth account unexpectedly enabled background tasks")
        check([row["path"] for row in upstream.since(mark)] == [OAUTH_START_PATH, OAUTH_POLL_PATH],
              "OAuth used unexpected upstream routes")
        mark = upstream.choose("oauth")
        repeated = expect_status(g.call("GET", poll_path, auth="panel"), 200).json()
        check(repeated.get("status") == "ok" and not upstream.since(mark), "completed OAuth poll replayed upstream")
        second = expect_status(g.call("POST", "/accounts/login/start", {"realm": "intl"}, "panel"), 200).json()
        cancelled = expect_status(g.call("POST", "/accounts/login/cancel",
                                         {"state": second["state"]}, "panel"), 200).json()
        check(cancelled.get("status") == "cancelled", "OAuth cancellation failed")
        mark = upstream.choose("oauth")
        polled = expect_status(g.call("GET", f"/accounts/login/poll?state={second['state']}", auth="panel"), 200).json()
        check(polled.get("status") == "error" and not upstream.since(mark), "cancelled OAuth was polled upstream")
        accounts = expect_status(g.call("GET", "/accounts", auth="panel"), 200).json()["accounts"]
        check(sum(a["uid"] == OAUTH_UID for a in accounts) == 1, "OAuth created duplicate accounts")

    def offline_background_quiet() -> None:
        time.sleep(0.2)
        check(not upstream.since(0), "offline gateway made unsolicited upstream requests")

    run("offline_background_quiet", offline_background_quiet)
    run("health_nonsecret", health)
    run("unauthorized_requests", unauthorized)
    run("panel_default_login", g.login)
    run("panel_api_credential_separation", separation)
    run("models_catalog", models)
    run("account_import_local", importing)
    for mode, streaming in [("normal", False), ("normal", True), ("mixed", False),
                            ("mixed", True), ("chunked", True), ("slow", True)]:
        run(f"chat_{mode}_{'stream' if streaming else 'nonstream'}",
            lambda mode=mode, streaming=streaming: chat(mode, streaming))
    run("panel_token_dashboard_chat", lambda: chat("normal", False, "panel"))
    run("responses_nonstream", lambda: responses(False))
    run("responses_stream", lambda: responses(True))
    run("compaction_nonstream", lambda: compaction(False))
    run("compaction_stream", lambda: compaction(True))
    run("billing_intl_detail_persisted", billing_detail)
    run("billing_cached_detail_no_network", billing_cached)
    run("billing_malformed_preserves_balance", billing_malformed)
    run("messages_gate_and_opt_in", messages_gate)
    run("messages_nonstream", lambda: messages(False))
    run("messages_stream", lambda: messages(True))
    for mode, streaming in [("sse_error", True), ("truncated", False), ("truncated", True),
                            ("chunked_truncated", True)]:
        run(f"{mode}_{'stream' if streaming else 'nonstream'}_no_replay",
            lambda mode=mode, streaming=streaming: broken(mode, streaming))
    run("idle_timeout_no_replay", idle_timeout)
    run("harmless_restriction_text_not_quarantined", harmless_restriction_text)
    run("key_scope_pinning_revocation_fail_closed", key_scope_and_revocation)
    run("upstream_401_refresh", refresh)
    run("upstream_429", rate_limit)
    run("restriction_persists_restart_no_replay", restriction)
    run("noncanonical_import_restriction_restart", noncanonical_restart)
    # Last: importing another usable account must not affect restriction selection checks.
    run("oauth_local_state_import_and_cancel", oauth_login_and_cancel)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, required=True, help="existing gateway executable; no build is performed")
    parser.add_argument("--report", type=Path, help="optional JSON result file")
    parser.add_argument("--startup-timeout", type=float, default=20, help="seconds to wait for each local gateway startup")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"binary does not exist: {args.binary}")
    if args.startup_timeout <= 0:
        parser.error("--startup-timeout must be positive")
    binary = args.binary.resolve()
    blocked = install_socket_guard()
    results: list[dict[str, Any]] = []
    started = time.monotonic()
    gateway: Gateway | None = None
    upstream: FixtureServer | None = None
    interrupted = False

    def run(name: str, action: Callable[[], Any]) -> None:
        at = time.monotonic()
        row: dict[str, Any] = {"name": name}
        try:
            action()
            row["status"] = "passed"
            print(f"PASS {name}", flush=True)
        except Exception as exc:
            row["status"] = "failed"
            detail = f"{type(exc).__name__}: {exc}"
            row["error"] = gateway.redact(detail) if gateway else detail
            print(f"FAIL {name}: {row['error']}", flush=True)
        row["duration_seconds"] = round(time.monotonic() - at, 3)
        results.append(row)

    with tempfile.TemporaryDirectory(prefix="wb2api-offline-smoke-") as temporary:
        try:
            upstream = FixtureServer()
            gateway = Gateway(binary, upstream, Path(temporary), args.startup_timeout)
            run("gateway_startup", gateway.start)
            if results[-1]["status"] == "passed":
                run_suite(gateway, upstream, run)
                run("fixture_routes_known", lambda: check(not upstream.unexpected, str(upstream.unexpected)))
            run("python_loopback_guard", lambda: check(not blocked, "; ".join(blocked)))
        except KeyboardInterrupt:
            interrupted = True
            results.append({"name": "interrupted", "status": "failed", "error": "interrupted by user"})
        except Exception as exc:
            results.append({"name": "harness", "status": "failed", "error": str(exc)})
            print(f"FAIL harness: {exc}", flush=True)
        finally:
            if gateway:
                gateway.stop()
            if upstream:
                upstream.close()
        failed = [row for row in results if row["status"] == "failed"]
        report = {"binary": str(binary), "offline": True,
                  "network_guard": "Python loopback-only audit; native child uses --offline and loopback --test-upstream (not an OS sandbox)",
                  "duration_seconds": round(time.monotonic() - started, 3),
                  "passed": len(results) - len(failed), "failed": len(failed), "results": results,
                  "upstream_requests": upstream.records if upstream else [],
                  "blocked_python_network": blocked}
        if failed and gateway:
            report["gateway_log_tail"] = gateway.log_tail()
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
            print(f"Report: {args.report.resolve()}", flush=True)
        print(f"Offline smoke: {report['passed']} passed, {report['failed']} failed", flush=True)
        return 130 if interrupted else int(bool(failed))


if __name__ == "__main__":
    raise SystemExit(main())
