"""Execute only the reviewed original offline suites; never alter their sources."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import re
import runpy
import subprocess
import sys
import tempfile
import time
import traceback

from offline import HERE, ROOT, activate, isolated_environment

SUITES = {
    "tool_parameters_normalization": "Pure schema/argument transforms; isolated data directories.",
    "tool_choice_none": "Pure body and DSML transforms; isolated data directories.",
    "stop_normalization": "Pure stop/body transforms; no live call despite historical live-test docstring.",
    "namespace_tools": "Pure expansion/return stamping; settings switch patched.",
    "custom_tools": "Synthetic Responses objects and byte-stream iterators.",
    "orphan_function_call_output": "Synthetic Responses inputs, including inline image data.",
    "upstream_repairs": "Pure pairing/body repairs and synthetic SSE iterators.",
    "sse_read_batching": "BytesIO/FakeStream equivalence; no sockets.",
    "chat_stream_usage_frame": "Two loopback servers, synthetic unsigned JWT, all identity endpoints redirected.",
    "stream_abort_frame": "Loopback fake upstream deliberately truncates body; synthetic unsigned JWT.",
    "stream_single_id": "Loopback fake upstream emits synthetic reasoning/content/usage frames.",
    "stream_retry": "Loopback fake upstream truncates first body then responds; synthetic unsigned JWT.",
    "anthropic_messages": "Synthetic protocol conversion and byte-stream iterators.",
    "anthropic_http": "Loopback fake upstream; synthetic unsigned JWT; native route opt-in/auth/error coverage.",
    "breaker_fairness": "Fake accounts/pool, mocked URL transport, mocked DNS probes; no real connections.",
    "pool_backoff": "Synthetic account files and mocked URL transport/response aggregation.",
    "restricted_403": "Scripted fake upstream and synthetic account files; no real transport.",
    "account_concurrency": "Temporary settings, fake responses, fake pool and URL transport.",
    "key_concurrency": "In-memory key-slot operations only.",
    "daily_chat_concurrency": "MagicMock accounts and patched scheduler task runner, no daily live activity.",
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def child(name, report_path):
    with tempfile.TemporaryDirectory(prefix="wb-baseline-child-") as tmp:
        stats = activate(tmp)
        path = ROOT / "tests" / ("_test_" + name + ".py")
        sys.argv = [str(path)]
        code = 0
        try:
            runpy.run_path(str(path), run_name="__main__")
        except SystemExit as exc:
            code = exc.code if isinstance(exc.code, int) else (0 if exc.code is None else 1)
        except BaseException:
            traceback.print_exc()
            code = 1
        finally:
            Path(report_path).write_text(json.dumps(stats, indent=2), encoding="utf-8")
        return code


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", choices=SUITES)
    parser.add_argument("--guard-report")
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    if args.suite:
        return child(args.suite, args.guard_report)
    logs = HERE / "logs"
    logs.mkdir(exist_ok=True)
    started = datetime.now(timezone.utc).isoformat()
    original_hashes = {p.name: digest(p) for p in ROOT.glob("*.py")}
    items = []
    for name, review in SUITES.items():
        suite = ROOT / "tests" / ("_test_" + name + ".py")
        log = logs / (suite.stem + ".log")
        guard = logs / (suite.stem + ".guard.json")
        command = [sys.executable, "-B", str(Path(__file__).resolve()), "--suite", name,
                   "--guard-report", str(guard)]
        before = digest(suite)
        at = time.monotonic()
        timed_out = False
        with tempfile.TemporaryDirectory(prefix="wb-baseline-parent-") as work:
            with log.open("w", encoding="utf-8") as output:
                proc = subprocess.Popen(command, cwd=work, env=isolated_environment(work),
                                        stdout=output, stderr=subprocess.STDOUT)
                try:
                    code = proc.wait(timeout=args.timeout)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    proc.kill()
                    code = proc.wait()
        text = log.read_text(encoding="utf-8", errors="replace")
        matches = re.findall(r"(?:SUMMARY:\s*)?PASS=(\d+)\s+FAIL=(\d+)", text)
        unit = re.search(r"Ran (\d+) tests? in", text)
        guard_data = json.loads(guard.read_text(encoding="utf-8")) if guard.exists() else None
        passed_checks = int(matches[-1][0]) if matches else len(re.findall(r"\[PASS\]", text))
        failed_checks = int(matches[-1][1]) if matches else len(re.findall(r"\[FAIL\]", text))
        item = dict(name=suite.name, command=command, original_command=[sys.executable, str(suite)],
                    exit_code=code, status="passed" if code == 0 else "failed",
                    timed_out=timed_out, seconds=round(time.monotonic() - at, 3),
                    checks={"passed": passed_checks, "failed": failed_checks},
                    unittest_tests=int(unit[1]) if unit else None,
                    source_sha256=before, source_unchanged=before == digest(suite),
                    safety_review=review, network_guard=guard_data,
                    log=str(log.relative_to(HERE)).replace("\\", "/"),
                    summary=text.strip().splitlines()[-1] if text.strip() else "")
        items.append(item)
        print("%s %-40s %6.2fs %s" % (item["status"].upper(), name, item["seconds"], item["summary"]), flush=True)
    try:
        commit = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True).strip()
    except Exception:
        commit = None
    result = {
        "schema_version": 1, "started_utc": started,
        "finished_utc": datetime.now(timezone.utc).isoformat(),
        "repository_commit": commit, "python": sys.version, "platform": platform.platform(),
        "runner_command": [sys.executable, "-B", str(Path(__file__).resolve())],
        "isolation": {"environment": "allowlisted OS launch variables only; no inherited keys or proxies",
                      "data": "fresh temporary HOME/USERPROFILE/APPDATA/LOCALAPPDATA/accounts/usage/catalog/pricing",
                      "network": "CPython audit hook permits loopback only and rejects non-loopback DNS/connect/sendto",
                      "processes": "nested subprocesses forbidden inside suite",
                      "original_accounts": "file access denied",
                      "original_sources": "writes outside baseline directory denied; top-level Python hashes checked",
                      "pycache": "disabled"},
        "summary": {"suites": len(items), "passed": sum(i["exit_code"] == 0 for i in items),
                    "failed": sum(i["exit_code"] != 0 for i in items),
                    "timeouts": sum(i["timed_out"] for i in items),
                    "explicit_checks_passed": sum(i["checks"]["passed"] for i in items),
                    "explicit_checks_failed": sum(i["checks"]["failed"] for i in items),
                    "unittest_tests": sum(i["unittest_tests"] or 0 for i in items)},
        "original_python_unchanged": all(digest(ROOT / n) == h for n, h in original_hashes.items()),
        "source_sha256": original_hashes, "suites": items,
    }
    (HERE / "results.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result["summary"]))
    return int(result["summary"]["failed"] > 0)


if __name__ == "__main__":
    raise SystemExit(main())
