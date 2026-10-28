# 00 — Rust port architecture and crate contracts

Baseline: Python tree at tag `py-baseline-1.7.32` (same repo, branch
`rust-port`). Behaviour specs: `01`..`07` in this directory. Target: Linux VPS
(Debian / Alpine), single static binary `wb2api`.

This file began as the implementation design. The actual alpha support boundaries and intentional differences are maintained in `../MIGRATION.md` (docs/); the Python behavior specifications are an oracle, not a claim that every feature has been ported.

## Workspace layout

```
./  (branch `rust`)
  Cargo.toml            workspace; all deps declared in [workspace.dependencies]
  crates/wbcore         shared helpers (DONE, tested) — see below
  crates/wbhttp         upstream HTTP/1.1 client (OpenSSL TLS, exact headers, pool, proxies)
  crates/wbsettings     settings.json schema/getters/setters, API keys, panel password, sessions
  crates/wbcatalog      model catalog (bundled snapshot, /v3/config curation, model_entry, tiers)
  crates/wbaccounts     Account model + file I/O, pool selection, penalties, identity headers,
                        sans-IO upstream request builders (chat headers, refresh, OAuth, billing)
  crates/wbchat         chat request normalization (T1-T24) + chat stream/non-stream output
  crates/wbtranslate    Responses API and Anthropic Messages <-> chat
  crates/wbusage        usage.jsonl rows, aggregations, pricing, activity, log store
  gateway/              the `wb2api` binary: hyper server, routing, auth, handlers, jobs
```

## Rules for every crate

* **Library crates are sans-IO for the network.** They never open sockets.
  They build `wbcore::types::OutRequest` values and parse
  `wbcore::types::InResponse` values; the gateway executes them with
  `wbhttp`. File I/O for their own data files is fine (std::fs, atomic write
  via temp file + rename).
* **No async in library crates** except `wbhttp`. Use `parking_lot` locks.
  Library code must be `Send + Sync`-friendly (the gateway calls it from tokio
  tasks; blocking file I/O is acceptable, it is small).
* **Byte compatibility where the spec says so.** Use `wbcore::pyjson`
  (`dumps`, `dumps_u`, `dumps_compact`, `dumps_sorted`, `dumps_pretty`) when
  output bytes / hashes must equal Python's `json.dumps` variants. The
  workspace builds `serde_json` with `preserve_order`, so `Value::Object`
  iteration = insertion order = Python dict order.
* Use `wbcore::pyval` (`truthy`, `get`, `py_int`, `py_float`, `py_str`,
  `py_round`) to keep Python truthiness / `or` semantics.
* Times: `wbcore::timeutil` (`now()` epoch f64, local midnight, `local_iso`,
  `next_local_4am`, `human_left`, ...). Local = process `TZ`.
* Ids: `wbcore::ids` (`derive_id`, `uuid7`-based `new_message_id`,
  `new_conversation_id`, `conversation_id_for`, `uuid4_hex`, `token_hex`,
  `token_urlsafe`, `sha256_hex`).
* Each crate ships unit tests; port the relevant Python test cases from
  `tests/_test_*.py` as Rust tests (golden fixtures from the specs).
* Keep public APIs small and documented; the gateway author will read your
  `lib.rs` docs, not your internals.
* Do not edit other crates' `src/`. If you need something from `wbcore`, add
  it to your own crate (or note it in your final report).
* Dependencies: only those already in `[workspace.dependencies]`; if you need
  another, add it there with a pinned major version and mention it.

## wbcore (done) — what exists

* `pyjson` — Python-compatible JSON formatting validated by differential fixtures; not a universal byte-identity guarantee for all floats/Unicode versions.
* `pyval` — Python truthiness/accessors.
* `timeutil`, `ids` — see above.
* `sse` — `strip_data_prefix`, `LineSplitter` (bytes -> lines), `clean_chunk`
  / `clean_chunk_value` (noise-field removal, drop rule), `chunk_error`.
* `usage` — `best_cached_tokens`, `normalize_cache_aliases`, `extract_usage`,
  `keep_best` (max by total_tokens), `total_tokens`, `estimate_tokens`.
* `toolcalls` — printed tool-call recovery: `parse_leaked_tool_calls`,
  `parse_dsml_tool_calls`, `parse_printed_calls`, `repair_leaked_tool_calls`,
  `LeakFilter` (stream filter; **DSML opening bug fixed**), `split_at_call_close`,
  `is_truncated_arguments`, `new_id(prefix)` (= prefix + uuid4 hex).
* `aggregate` — `Aggregator` (`push_line`/`push_chunk`/`finish`) = Python
  `aggregate_stream`, minus the leaked internal `elapsed_ms`/`first_chunk_at`
  response fields (exposed on `Aggregated` instead).
* `types` — `Realm`, `Product`, `Cred`, `HeaderCase`, `Egress`,
  `OutRequest`, `InResponse`, `is_static_api_key`.

## Decisions (apply everywhere)

1. Fix, don't replicate, these Python bugs: slot leaks on raised 4xx (release
   on every exit); key slot release re-reading the limit; DSML stream filter;
   `GET /settings` must include `api_keys[].concurrency_limit` and
   `daily_chat_web_concurrency`; `/usage/timeseries` accepts `bucket` **and**
   `bucket_seconds`; non-dict `messages[0]` / string `messages` -> 400;
   internal `elapsed_ms`/`first_chunk_at` are not sent to clients.
2. Keep `/v1/completions` as chat alias and Python truthiness for `stream`; non-object JSON body becomes `{}`. Preserve user text instead of the original fingerprint sanitizer. Synthesized tool indices do not collide. Restrictions return 403 with sanitized status/code rather than raw credential-bearing error bodies.
3. Dropped (Linux VPS): desktop credential import + at-rest key recovery,
   `--info`, `--import-desktop`, `--probe-only`, desktop catalog cache file,
   `/agents*`, `/updates/install|rollback|backups` (keep `GET /updates` as a
   stub reporting the current version), Windows console handler.
   `/console/*` stay as the constant legacy responses (cheap).
4. Header case: chat requests use exact names/order; every other upstream call
   reproduces urllib title-casing (`HeaderCase::Title`) for byte parity with
   the baseline.
5. TLS uses standard OpenSSL secure defaults, minimum TLS1.2, SNI and hostname verification. Runtime Linux trust bundles plus optional WB_CA_BUNDLE are loaded. No TLS fingerprint imitation or explicit session-resumption cache.
6. Proxies: explicit direct or authenticated HTTP CONNECT. SOCKS and HTTPS proxy schemes return explicit unsupported errors in this alpha.
7. Account/settings JSON, usage JSONL and historical pricing formats remain compatible. SQLite runtime log writing is not implemented. Use copied data directories for migration testing; never run Python and Rust against the same writable files concurrently.
