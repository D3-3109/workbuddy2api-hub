"""Offline oracle for de4d86d, as shipped in 4f7ae6f (v1.8.0).

Run with Python -B; --check regenerates in memory without writing. Only reviewed
function/constant AST nodes are compiled from local Git objects: wb_proxy is
never imported, so account/config reads and server startup cannot run. Each
batch records start(), one upstream chunk, or EOF separately; UUIDs and time
are deterministic. No network, credentials, or third-party packages are used.
"""
import argparse
import os
import ast
import copy
import hashlib
import itertools
import json
from pathlib import Path
import re
import subprocess
import sys
import types
import uuid

# Git checkout of the original Python repository (any branch containing REVISION).
ROOT = Path(os.environ.get("WB_PY_ROOT") or sys.exit("set WB_PY_ROOT to the Python repository")).resolve()
TARGET = Path(__file__).with_name("responses_first_arguments.json")
REVISION = "4f7ae6f"
FUNCTIONS = {
    "stream_responses_events", "_new_id", "_responses_usage",
    "_unwrap_custom_input", "apply_namespace_to_calls", "build_citations",
    "estimate_tokens", "is_truncated_arguments", "parse_dsml_tool_calls",
    "resolve_namespaced_name", "stamp_namespace", "strip_data_prefix",
    "web_tools_active",
}
CONSTANTS = {
    "TAG_START", "DSML_CALLS_RE", "DSML_INVOKE_RE", "DSML_PARAM_RE",
    "CJK_RE", "_NS_SEP", "_CITATION_MD_RE",
}


def chunk(delta, finish=None):
    return {"choices": [{"delta": delta, "finish_reason": finish}]}


def call(index, identifier, name, arguments):
    return {"index": index, "id": identifier, "type": "function",
            "function": {"name": name, "arguments": arguments}}


def inputs(baseline):
    # Keep the exact original cases affected by de4d86d, independently of the
    # explicit expected-divergence adapter in oracle_tests.rs.
    for case in baseline["cases"]:
        if case["id"] in ("responses.stream_custom", "responses.stream_truncated"):
            yield case["id"], case["input"]

    def single(identifier, name, first, later, holder=None):
        tc = call(0, "call_first", name, first)
        if first is None:
            del tc["function"]["arguments"]
        chunks = [chunk({"tool_calls": [tc]})]
        chunks.extend(chunk({"tool_calls": [{"index": 0, "function": {"arguments": part}}]})
                      for part in later)
        chunks.extend([chunk({}, "tool_calls"), "[DONE]"])
        return identifier, {"chunks": chunks, "model": "m", "holder": holder or {"usage": None}}

    # Upstream tests/_test_custom_tools.py [7.1], plus missing/empty arguments.
    for label, first, later in [
        ("partial", '{"city":"', ['Beijing"}']),
        ("complete", '{"city":"Beijing"}', []),
        ("empty", "", ['{"city":"Beijing"}']),
        ("missing", None, ['{"city":"Beijing"}']),
        ("no_arguments", "", []),
    ]:
        yield single("function." + label, "get_weather", first, later)

    # Upstream [7.2]: both tools start together, one complete, one partial.
    yield "function.interleaved", {"model": "m", "holder": {"usage": None}, "chunks": [
        chunk({"tool_calls": [call(0, "call_a", "get_weather", '{"city":"'),
                               call(1, "call_b", "get_weather", '{"city":"Paris"}')]}),
        chunk({"tool_calls": [{"index": 0, "function": {"arguments": 'Beijing"}'}}]}),
        chunk({}, "tool_calls"), "[DONE]",
    ]}
    custom = {"usage": None, "custom_names": ["apply_patch"],
              "namespace_map": {"apply_patch": "functions"}}
    # Upstream [7.3], also complete/empty first chunks and literal freeform text.
    for label, first, later in [
        ("partial", '{"input":"', ['pwd"}']),
        ("complete", '{"input":"pwd"}', []),
        ("empty", "", ['{"input":"pwd"}']),
        ("raw", '*** Begin Patch\n+"hi"\\path', ['\n*** End Patch']),
        ("escaped", '{"input":"line\\n', ['\\"quoted\\"\\\\path"}']),
    ]:
        yield single("custom." + label, "apply_patch", first, later, custom)

    # Sparse/reversed tool indices, namespaced function and custom in one chunk,
    # repeated names/changed ids, and a later fragment without an id or name.
    yield "mixed.namespace_indices", {"model": "m", "holder": {
        "usage": None, "custom_names": ["apply_patch"],
        "namespace_map": {"get_weather": "functions", "apply_patch": "functions"},
    }, "chunks": [
        chunk({"tool_calls": [call(7, "call_a", "functions::get_weather", '{"city":"'),
                               call(2, "call_b", "apply_patch", '{"input":"pwd"}')]}),
        chunk({"tool_calls": [call(7, "changed", "get_weather", "Bei")]}),
        chunk({"tool_calls": [{"index": 7, "function": {"arguments": 'jing"}'}}]}),
        chunk({}, "tool_calls"), "[DONE]",
    ]}


def capture(source, cases):
    tree = ast.parse(source)
    selected = []
    found = set()
    for node in tree.body:
        if isinstance(node, ast.FunctionDef) and node.name in FUNCTIONS:
            selected.append(node)
            found.add(node.name)
        elif isinstance(node, ast.Assign):
            names = {n.id for n in node.targets if isinstance(n, ast.Name)}
            if names & CONSTANTS:
                assert names <= CONSTANTS
                selected.append(node)
                found.update(names)
    assert found == FUNCTIONS | CONSTANTS, found
    helpers = ast.Module(body=selected, type_ignores=[])
    helper_source = "\n\n".join(ast.get_source_segment(source, n) for n in selected)
    namespace = {"json": json, "re": re, "time": types.SimpleNamespace(time=lambda: 1700000000)}
    exec(compile(helpers, REVISION + ":wb_proxy.py", "exec"), namespace)
    results = []
    for identifier, original in cases:
        value = copy.deepcopy(original)
        assert not value["holder"].get("base_body")
        counter = itertools.count(1)
        namespace["uuid"] = types.SimpleNamespace(
            uuid4=lambda: uuid.UUID(hex=("%016x" % next(counter)) * 2))
        phase = 0
        batches = [[] for _ in range(len(value["chunks"]) + 2)]

        def wire():
            nonlocal phase
            for phase, part in enumerate(value["chunks"], 1):
                text = part if isinstance(part, str) else json.dumps(part, ensure_ascii=False)
                yield ("data: " + text + "\n\n").encode("utf-8")
            phase = len(value["chunks"]) + 1

        for raw in namespace["stream_responses_events"](wire(), value["model"], value["holder"]):
            lines = raw.decode("utf-8").splitlines()
            assert lines[0].startswith("event: ") and lines[1].startswith("data: ")
            batches[phase].append({"event": lines[0][7:], "data": json.loads(lines[1][6:])})
        results.append({"id": identifier, "input": original,
                        "expected": {"batches": batches, "holder": value["holder"]}})
    return {"oracle": {
        "revision": REVISION, "fix_commit": "de4d86d", "version": "1.8.0",
        "implementation": "unchanged AST-extracted Python stream_responses_events and pure helpers",
        "source_sha256": hashlib.sha256(source.encode("utf-8")).hexdigest(),
        "helpers_sha256": hashlib.sha256(helper_source.encode("utf-8")).hexdigest(),
        "batches": "start, one batch per input chunk, finish; empty batches retained",
        "comparison": "exact event/data arrays; generated ids compared by bijection; time fixed at 1700000000",
    }, "case_count": len(results), "cases": results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    source = subprocess.check_output(["git", "-C", str(ROOT), "show", REVISION + ":wb_proxy.py"]).decode("utf-8")
    baseline = json.loads((ROOT / "rust/tests/baseline/golden.json").read_text(encoding="utf-8"))
    cases = list(inputs(baseline))
    # After the two explicit source reads, the helper evaluation may only write
    # this fixture (or read it for --check). Every socket/process event is denied.
    def guard(event, detail):
        if event.startswith("socket.") or event in ("subprocess.Popen", "os.system"):
            raise PermissionError("offline oracle denied " + event)
        if event == "open" and isinstance(detail[0], (str, bytes)):
            if Path(detail[0]).resolve() != TARGET:
                raise PermissionError("offline oracle denied file access: " + str(detail[0]))
    sys.addaudithook(guard)
    result = capture(source, cases)
    # One exact case per line keeps the fixture compact without altering strings.
    serialized = ('{\n  "oracle": ' + json.dumps(result["oracle"], ensure_ascii=False) +
                  ',\n  "case_count": ' + str(result["case_count"]) + ',\n  "cases": [\n' +
                  ',\n'.join("    " + json.dumps(c, ensure_ascii=False) for c in result["cases"]) +
                  '\n  ]\n}\n')
    if args.check:
        assert TARGET.read_text(encoding="utf-8") == serialized, "upstream fixture changed"
        print("Verified %d byte-identical offline Python cases" % len(cases))
    else:
        TARGET.write_text(serialized, encoding="utf-8")
        print("Captured %d offline Python cases" % len(cases))


if __name__ == "__main__":
    main()
