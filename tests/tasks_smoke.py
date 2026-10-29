"""Loopback smoke test for the scheduler and reward tasks.

Starts a fake upstream and the gateway with --test-upstream, then drives the
panel endpoints: CN check-in, cat travel, streak bonus, growth tasks, intl daily
activity (desktop leg + web console + ACP turn), activity history and the
scheduler controls. No real upstream is contacted.

usage: python tests/tasks_smoke.py <path-to-wb2api-binary>
"""
import base64
import http.client
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
CALLS = []
STATE = {"prompted": False}


def jwt(uid):
    def part(v):
        return base64.urlsafe_b64encode(json.dumps(v).encode()).rstrip(b"=").decode()
    return "%s.%s.sig" % (part({"alg": "none"}), part({"sub": uid, "exp": int(time.time()) + 30 * 86400}))


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, value, status=200, headers=None):
        body = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def record(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        CALLS.append((self.command, self.path, dict(self.headers), raw))
        return raw

    def do_GET(self):
        self.record()
        p = self.path
        if p.startswith("/acp/"):
            return self.sse()
        if p == "/activity/growth/buddy/travel/status":
            return self.reply({"code": 0, "data": {"state": "idle", "daily_limit_reached": False}})
        if p == "/activity/growth/buddy/travel/config":
            return self.reply({"code": 0, "data": {"locations": [{"id": 7}]}})
        if p == "/activity/growth/heatmap":
            return self.reply({"code": 0, "data": {"cells": []}})
        if p == "/activity/growth/streak":
            return self.reply({"code": 0, "data": {"streak": {"days": 3}, "makeup_cards": {"balance": 0},
                                                   "redemption_status": {"tiers": [{"tier": "7d"}], "tier_7d_status": "available"}}})
        if p == "/activity/growth/lottery/summary":
            return self.reply({"code": 0, "data": {"chances": 1}})
        if p == "/v2/activity/growth/energy":
            return self.reply({"code": 0, "data": {"balance": 42}})
        if p == "/v2/activity/growth/tasks":
            return self.reply({"code": 0, "data": {"tasks": [
                {"task_code": "skill_1", "title": "体验技能", "accept_status": "completed", "progress": {"current": 1, "target": 1}, "reward_credit": 100},
                {"task_code": "chat_5", "title": "发起 5 次对话", "accept_status": "not_accepted", "progress": {"current": 0, "target": 5}}]}})
        if p.endswith("/session"):
            return self.reply({"code": 0, "data": {"link": "https://sandbox.example/acp/s1", "token": "sandbox-token", "sessionId": "sess-1"}})
        if p.startswith("/console/as/conversations/conv1"):
            return self.reply({"code": 0, "data": {"status": "completed" if STATE["prompted"] else "working"}})
        self.reply({"code": 404, "msg": "unknown " + p}, 404)

    def sse(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Acp-Connection-Id", "conn-1")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        for i in range(3):
            event = {"jsonrpc": "2.0", "method": "session/update", "params": {"update": {"sessionUpdate": "agent_message_chunk", "content": i}}}
            data = ("data: %s\n\n" % json.dumps(event)).encode()
            self.wfile.write(b"%x\r\n%s\r\n" % (len(data), data))
            self.wfile.flush()
        self.wfile.write(b"0\r\n\r\n")

    def do_POST(self):
        raw = self.record()
        p = self.path
        body = json.loads(raw or b"{}") if raw[:1] in (b"{", b"[") else {}
        if p.startswith("/acp/"):
            if body.get("method") == "session/prompt":
                STATE["prompted"] = True
            return self.reply({}, 202)
        if p == "/v2/billing/meter/daily-checkin":
            return self.reply({"code": 0, "msg": "签到成功"})
        if p == "/v2/chat/completions":
            return self.reply({"choices": [{"delta": {"content": "Hi"}, "finish_reason": "stop"}]})
        if p == "/console/as/conversations/":
            return self.reply({"code": 0, "data": {"id": "conv1"}})
        if p.startswith("/console/as/conversations/conv1/"):
            return self.reply({"code": 0})
        if p == "/activity/growth/buddy/travel/depart":
            return self.reply({"code": 0, "data": {"location": {"name": "海边"}}})
        if p in ("/billing/meter/claim-gift", "/billing/meter/claim-compensation"):
            return self.reply({"code": 1, "msg": "already claimed"})
        if p in ("/activity/growth/redeem", "/activity/growth/lottery/draw", "/v2/activity/growth/tasks/accept"):
            return self.reply({"code": 0, "data": {}})
        if p == "/activity/growth/tasks/skill_1/claim":
            return self.reply({"code": 0, "data": {"credit": 100}})
        self.reply({"code": 404, "msg": "unknown " + p}, 404)


def call(port, method, path, body=None, token=""):
    conn = http.client.HTTPConnection(HOST, port, timeout=60)
    headers = {"Content-Type": "application/json"}
    if token:
        headers["X-Panel-Token"] = token
    conn.request(method, path, json.dumps(body).encode() if body is not None else None, headers)
    resp = conn.getresponse()
    data = resp.read()
    conn.close()
    return resp.status, json.loads(data or b"null")


def check(condition, message):
    if not condition:
        raise SystemExit("FAIL: " + message)
    print("ok  ", message)


def main():
    binary = sys.argv[1]
    upstream = ThreadingHTTPServer((HOST, 0), Upstream)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    root = tempfile.mkdtemp(prefix="wb-tasks-")
    accounts, usage = os.path.join(root, "accounts"), os.path.join(root, "usage")
    os.makedirs(accounts)
    for uid, realm in (("cn-user", "cn"), ("intl-user", "intl")):
        with open(os.path.join(accounts, uid + ".json"), "w", encoding="utf-8") as f:
            json.dump({"uid": uid, "realm": realm, "accessToken": jwt(uid), "refreshToken": "refresh-" + uid + "-0123456789",
                       "nickname": uid, "enabled": True, "tasksEnabled": True}, f)
    port = 18000 + os.getpid() % 2000
    env = {k: v for k, v in os.environ.items() if not k.startswith("WB_")}
    proc = subprocess.Popen([binary, "--host", HOST, "--port", str(port), "--accounts-dir", accounts,
                             "--usage-dir", usage, "--test-upstream", "http://%s:%d" % (HOST, upstream.server_port)],
                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        for _ in range(100):
            try:
                if call(port, "GET", "/health")[0] == 200:
                    break
            except OSError:
                time.sleep(0.1)
        token = call(port, "POST", "/panel/login", {"password": "admin"})[1]["token"]

        status, r = call(port, "POST", "/accounts/checkin", {}, token)
        check(status == 200 and r["results"][0]["ok"] is True, "CN check-in succeeds")
        saved = json.load(open(os.path.join(accounts, "cn-user.json"), encoding="utf-8"))
        check(saved.get("lastCheckin", "").startswith(time.strftime("%Y-%m-%d")), "lastCheckin is persisted")

        status, r = call(port, "POST", "/tasks/travel", {}, token)
        check(status == 200 and r["results"][0]["action"] == "depart", "cat travel departs an idle cat")
        depart = [c for c in CALLS if c[1] == "/activity/growth/buddy/travel/depart"]
        check(json.loads(depart[-1][3]) == {"location_id": 7}, "depart uses the configured location")

        status, r = call(port, "POST", "/tasks/streak-bonus", {}, token)
        check(status == 200 and "兑换连登 7d" in r["msg"] and "抽奖 1 次" in r["msg"], "streak bonus redeems and draws")

        status, r = call(port, "POST", "/tasks/run", {}, token)
        check(status == 200 and r["credit_added"] == 100, "growth run claims only the completed task")
        check(any(c[1] == "/v2/activity/growth/tasks/accept" for c in CALLS), "growth run accepts open tasks")

        status, r = call(port, "GET", "/tasks", None, token)
        check(status == 200 and r["supported"] and r["summary"]["energy"] == 42, "growth snapshot renders")

        status, r = call(port, "POST", "/accounts/daily-chat", {}, token)
        res = r["results"][0]
        check(status == 200 and res["ok"] is True and res["web"]["chunks"] >= 1, "intl daily activity completes the web turn")
        rpc = [json.loads(c[3])["method"] for c in CALLS if c[0] == "POST" and c[1].startswith("/acp/")]
        check(rpc == ["initialize", "session/load", "session/set_model", "session/prompt"], "ACP calls in order")
        acp = [c for c in CALLS if c[1].startswith("/acp/")][0]
        check(acp[2].get("Authorization") == "Bearer sandbox-token", "ACP uses the sandbox token")
        check(any(c[1] == "/console/as/conversations/conv1/stop" for c in CALLS), "web session is released")
        status, r = call(port, "POST", "/accounts/daily-chat", {}, token)
        check(r["results"][0].get("skipped") is True, "second daily activity of the day is skipped")

        status, r = call(port, "GET", "/activity/history?range=today", None, token)
        tasks = sorted(row["task"] for row in r["rows"])
        check(status == 200 and tasks == ["checkin", "daily_chat"], "activity history records each attempt once")

        status, r = call(port, "GET", "/scheduler", None, token)
        check(status == 200 and r["supported"] and r["enabled"], "scheduler status")
        status, r = call(port, "POST", "/scheduler/toggle", {}, token)
        check(r["enabled"] is False, "scheduler toggle pauses")
        status, r = call(port, "POST", "/scheduler/trigger", {}, token)
        check(status == 200 and r["ok"] is True, "manual trigger starts a cycle")

        check(not any(c[1] == "/v2/report" for c in CALLS), "no fabricated telemetry was sent")
        status, _ = call(port, "GET", "/usage?realm=all", None, token)
        check(status == 200, "usage view answers")
        print("all task smoke checks passed")
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        upstream.shutdown()
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
