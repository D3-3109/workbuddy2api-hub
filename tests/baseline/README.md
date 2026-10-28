# Original Python offline baseline

This directory is an executable oracle, not a proposed Rust specification. Python source files and Rust crates are unchanged by this work.

## Recorded result

- **20 original suites passed, 0 failed, 0 timed out** on Python 3.14.7 / Windows.
- **466 explicit PASS checks + 116 unittest test methods** (different counting units; not an assertion total).
- Sequential suite wall time: **22.964 seconds**.
- `results.json` records exact commands, exit codes, timings, safety reviews, source SHA-256 values, per-suite counts and logs.
- `logs/*.log` retains original suite output; `logs/*.guard.json` records offline I/O enforcement.
- All selected suite sources and all top-level original Python source hashes remained unchanged.
- `validation.json` records successful byte-identical regeneration, structural/invariant checks and final source integrity. Full JSON Schema library validation was not run (`jsonschema` is not installed; no packages were downloaded).

### Important offline finding

The five loopback HTTP suites start the original gateway runtime, including its pricing refresher. Despite their no-network docstrings, each attempted to resolve **openrouter.ai**. The runner's CPython audit hook rejected all five attempts **before DNS lookup or connection**. These appear explicitly in guard evidence; they are not hidden or counted as successful external calls. All fake-upstream tests still passed.

The runner clears inherited credentials, proxy settings and `WB_*` switches, supplies fresh temporary HOME/USERPROFILE/APPDATA/LOCALAPPDATA/accounts/usage/catalog/pricing directories, disables bytecode writes, denies access to the repository's actual accounts directory, denies writes elsewhere in the repository, denies nested subprocesses, and permits socket operations only on loopback. This is an in-process test guard, not a general OS sandbox. Selected tests were read before execution; fake URL transports and temporary synthetic JWTs are the only account paths exercised. No browser/live-account suites ran.

## Reproduce

From the repository root (or replace the paths with absolute paths):

```sh
python -B rust/tests/baseline/run_baseline.py
python -B rust/tests/baseline/export_fixtures.py
python -B rust/tests/baseline/export_fixtures.py --check
```

The first command runs the unchanged original files through `runpy` in **separate, sequential processes**, 120 seconds maximum each. It deliberately uses a fixed 20-suite allowlist rather than the full test collection. The second regenerates `golden.json`; the third must reproduce it byte-for-byte. No third-party Python dependency is needed for execution/export. `golden.schema.json` is JSON Schema draft 2020-12 for optional consumer validation.

## Selected suites

| Area | Original suites |
|---|---|
| Tool schema/normalization | `_test_tool_parameters_normalization`, `_test_tool_choice_none`, `_test_stop_normalization` |
| Pairing / Responses tools | `_test_upstream_repairs`, `_test_custom_tools`, `_test_namespace_tools`, `_test_orphan_function_call_output` |
| SSE / chat handler | `_test_sse_read_batching`, `_test_chat_stream_usage_frame`, `_test_stream_abort_frame`, `_test_stream_single_id`, `_test_stream_retry` |
| Anthropic | `_test_anthropic_messages`, `_test_anthropic_http` |
| Pool policy | `_test_breaker_fairness`, `_test_pool_backoff`, `_test_restricted_403` |
| Concurrency | `_test_account_concurrency`, `_test_key_concurrency`, `_test_daily_chat_concurrency` |

## Golden fixture contract

`golden.json` contains **115 cases**, all expected values produced by calls into original Python functions. No expected protocol payload is handwritten. `export_fixtures.py` is the executable adapter for stateful scenarios.

```json
{
  "schema_version": 1,
  "oracle": {
    "implementation": "unchanged original Python functions",
    "source_sha256": {"wb_proxy.py": "..."},
    "determinism": {"epoch_seconds": 1700000000.0, "uuid4": "..."},
    "constants": {"FAIL_WINDOW": 600.0}
  },
  "case_count": 115,
  "cases": [{
    "id": "stop.0",
    "operation": "wb_proxy.normalize_stop",
    "kind": "mutated_arg0",
    "input": {"args": [{"stop": "END"}], "kwargs": {}},
    "expected": {"stop": ["END"]}
  }]
}
```

Use `id` as a stable test name and `kind` to select the Rust adapter. Compare JSON objects without key ordering; preserve array order, null-vs-absent, and strings exactly (especially serialized function arguments). Python tuple returns are JSON arrays. Numeric equality should not distinguish `600` from `600.0`.

| `kind` | Input / expected semantics |
|---|---|
| `return` | Call `operation` with `input.args` and `input.kwargs`; compare returned JSON value. Pairing helpers return `[messages, changed]`. |
| `mutated_arg0` | Call original mutator; compare **mutated first argument**, not its return value. |
| `utf8_return` | Returned bytes decoded as UTF-8, preserving compact JSON formatting. |
| `map_counts` | Apply named backoff helper to each `input.counts` value; expected is ordered seconds. |
| `aggregate` | Encode `input.chunks` as SSE (`[DONE]` is a literal sentinel); aggregate with model/resp_id. |
| `sse` | Feed chunks into named translator with initial holder; compare `{frames, holder}`. |
| `handler_scenario` | Execute real `_chat_stream_response` using memory sink; optional abort raises `IncompleteRead(0 bytes read, 4096 more expected)`, optional retry chunks supply reopened stream. Expected includes status, headers, frames, captured accounting records and reopen count. |
| `account_scenario` | Fresh synthetic account; sequential `steps` call methods with args/kwargs. `advance` increments fixed wall clock before the step. Expected rows expose return, selected state, readiness, throttle wait. |
| `slot_scenario` | Account/key `scope`, limit and acquire/release action sequence; compare each return and inflight map. |
| `response_slot_scenario` | Acquire limit-one account slot, bind fake response, then close repeatedly; compare snapshots. |
| `restriction_wave` | Apply events with realm/UID and epoch-relative offset; compare returned hold epoch and remaining seconds. |
| `rate_limit_scenario` | Compare original soft-shape/code/reset/account-level classification helpers. |
| `dispatch_scenario` | Real `open_upstream` with synthetic account pool and scripted in-memory HTTP results; compare selection/error, total upstream attempts, restriction hold and account state. No outbound transport. |

Frames have shape `{"event": null | "event.name", "data": {...} | "[DONE]"}`. Event order and sequence numbers are meaningful. SSE comments and transport write boundaries are not part of this parsed representation; the original SSE batching suite separately validates exact bytes.

### Determinism and scope

Every case starts at wall-clock epoch **1700000000** and UUID counter **1**. UUID hex is `format(n, '016x')` repeated twice, incrementing per original UUID call; thus truncated 16-character chunk IDs remain distinct. Generated identifiers are not post-hoc scrubbed. A Rust implementation can inject matching ID/time providers or normalize generated IDs consistently while preserving every cross-reference and uniqueness relation.

Fixtures use isolated empty settings and existing bundled model data. Handler fixtures disable optional egress-backoff accounting and capture only accounting writes in memory; the original socket-based retry suite separately covers actual egress streaks. Dispatch fixtures prove routing/error policy but do not export credential headers. Restriction persistence is tested by the original suite; synthetic fixture accounts are deliberately not persisted. The suite's Anthropic HTTP feature-gate/auth checks are evidence in logs, not a claim that Rust implements them. Daily-task concurrency has original-suite evidence but no timing-sensitive golden fixture.

## Baseline facts useful for Rust

- Empty/invalid parameter schemas become object schemas with properties; recursive Python type aliases and null schema metadata are normalized. Function argument serialization spacing is observable.
- `tool_choice="none"` **keeps tools**. Stop strings become arrays; empty/non-string entries are discarded, whitespace-only stop strings survive.
- Interleaved tool results are repacked, orphan calls/results dropped, adjacent empty parallel-call messages merged with reasoning preserved.
- Chat streams use one generated ID for reasoning/text/finish/usage. Usage appears once, in `choices: []`, after finish and before `[DONE]`.
- Mid-body upstream abort emits `upstream_stream_error` and **no `[DONE]`**; one transparent retry is allowed only before client-visible output. The pre-byte retry fixture records one accounting row.
- Responses custom tools are downgraded outbound and reinflated inbound with namespace preserved; truncated calls yield incomplete output, reasoning-only streams fail rather than silently complete.
- Anthropic cached reads/writes are subtracted from prompt tokens: input 10, read 4, creation 2 becomes `input_tokens: 4`, `output_tokens: 5`. Incoming thinking blocks are dropped; outgoing reasoning becomes thinking blocks.
- Consecutive hard-failure window is 600s; breaker threshold 3, first 1800s, capped 21600s. Unknown-failure degradation starts at 5, 600s, capped 7200s.
- Verified account soft backoff is 600s→7200s; unscoped/model backoff is 60s→600s with a separate counter. Success clears both.
- Restricted 403/code 11140 defaults to **one account attempt, no rotation**, 7200s restriction. Two distinct restricted UIDs in the 900s realm window trigger 1800s pool hold. Repeated same UID does not count twice; content-review 403 does not impose that account penalty.
- Account/key concurrency limit zero is untracked unlimited. Account capacity lasts until response close, and repeated close/release does not underflow.
