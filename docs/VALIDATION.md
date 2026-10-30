# Validation record — Rust VPS alpha

Baseline: `477ba36ceea78ec4252c47a68e94a5a1612bab0b`, local `rust-port` branch. Rust **0.1.1 alpha** selectively synchronizes upstream through `4f7ae6f2efcae5e6ae85aefa732035206044514b` (v1.8.0); Python originals/dashboard remain unchanged. Validation performed 2026-10-27 on Windows x86-64 using Rust 1.99.1 GNU, Python 3.14.7, Zig 0.16.0 and cargo-zigbuild 0.24.0. See `UPSTREAM_SYNC.md` for scope.

## Final results

| Layer | Result | What was checked |
|---|---:|---|
| Python baseline | 20 suites passed; 0 failed/timeouts | 466 explicit checks and 116 unittest tests; synthetic accounts and loopback upstream |
| Rust workspace | 258 tests reported passed; 0 failed | Unit, HTTP/TLS, persistence and Python-oracle regression tests; conditional coverage caveat below |
| Gateway executable | 39 smoke cases passed; 0 failed | Real local HTTP sockets against the native Windows development executable |
| Upstream sync smoke | 10 cases passed; 0 failed | First tool deltas, passive window restore/persistence, desktop option, blocked/closed stderr |
| Linux build | Passed | Release cross-build for `x86_64-unknown-linux-musl` |
| Artifact inspection | Passed | ELF64 x86-64, statically linked, stripped |
| Formatting | Passed | `cargo fmt --all --check` |

Rust test breakdown: gateway43, accounts42+6 desktop integration, catalog21, chat27, core25+7 integration, HTTP23, settings23, translation29, usage12. Counts are Rust test functions, not the number of fixture rows checked inside them. One Windows symlink fixture returned early because symlink privilege1314 was unavailable; the runner reports it passed, but that branch was **not exercised**. Directory/non-file rejection was exercised. The previous230-test baseline and binary are retained under `dist/archive-0.1.0/`.

## Cross-language evidence

- `tests/baseline/golden.json`: 115 deterministic cases generated from pinned Python functions; regeneration byte-identical.
- Rust chat tests compare 39 selected Python golden cases.
- Protocol-adapter tests retain32baseline cases, explicitly adjust the two affected first-delta expectations, and compare14new exact per-chunk oracle fixtures generated from upstream4f7ae6f. Fixture regeneration is byte-identical.
- Account tests compare 22 selected Python golden cases.
- Catalog tests compare the complete shape of 31 advertised bundled model entries to Python output.
- Pricing tests validate 52 historical policy hashes and 496 cost-source fixture rows. The aggregate fixture totals 53,593,076 tokens and CNY16.129016759762507 within documented floating tolerance.
- Core Python fixtures cover 60 JSON/string/truthiness cases across seven JSON styles, 59 numeric-conversion cases and 2,988 rounding comparisons.

These selections overlap in purpose and should not be summed into a feature-coverage percentage.

## Gateway smoke coverage

The final 39-case harness includes:

- Health/model listing; API-key versus panel-session authorization.
- Account-bound key isolation on balance/pinned routes; revoking the last key remains fail-closed through restart; explicit anonymous mode only.
- Text, reasoning and tools; Chat, Responses, Messages opt-in; streaming/nonstreaming and compaction.
- Chunked/slow streams, stream error/truncation/idle timeout with no ambiguous POST replay.
- Token refresh on401,429cooldown,11140persistence and restart blocking.
- Noncanonical account filenames retain exactly one current credentials file after import/refresh/restriction/restart.
- Ordinary generated text containing restriction-like phrases does not quarantine accounts.
- International billing parse/cached detail/malformed-response balance preservation.
- OAuth local-state isolation, import, completed-poll caching and cancellation.

Synthetic tokens/accounts are temporary and are not real credentials. The Python socket guard blocks non-loopback operations. During baseline testing it blocked five background price-refresher attempts before DNS/connect; those attempts did not reach the external service. The native child gateway is controlled by `--offline --test-upstream` rather than by Python's audit hook.

## Additional v1.8.0 sync regressions

The10new black-box cases verify first and later tool fragments exactly once, empty opening chunks, seeded and Rust-written429sidecar windows across restart/expiry, unrelated nonfree models remaining usable, strict desktop option/header/body rules, CLI/VSCode isolation and logger failure modes.

Unread stderr test:2,600invalid account files offered431,600log bytes,577lines were dropped by the bounded queue, panel requests still answered, and graceful shutdown completed in0.467seconds. Closed-pipe shutdown completed in0.422seconds. These are failure-mode checks, not throughput benchmarks.

Two initial smoke test expectations were wrong: upstream trims the environment value before comparing to1, and desktop compatibility removes browser headers as well as adding SDK metadata. After checking `d1a8913` source, the tests were corrected without changing the binary or Rust source. Both9/10diagnostic runs are preserved in `tests/results-pre-expectation-fix.json`; the final unchanged implementation passed10/10.

## Artifact

File: `dist/wb2api-linux-x86_64-musl`

Size: **13,042,144 bytes** (approximately12.44MiB).

SHA-256:

```text
90e151e887b1d254b5f0f2e32e9d7951c14fa99b4c2ab697dff1a1315acc90a9
```

Companion files: `dist/SHA256SUMS`, `dist/validation.json`, `dist/cargo-test.log`, `dist/linux-build.log`, `dist/LICENSE`, `dist/NOTICE.md`. `dist/` is ignored by Git; this is a local development deliverable, not a published release.

## Reproduction

From the repository root:

```sh
cargo test --workspace --locked
cargo fmt --all --check
python tests/gateway_smoke.py --binary /absolute/path/to/wb2api --report tests/gateway-smoke-results.json
python tests/upstream_sync_smoke.py --binary /absolute/path/to/wb2api --report tests/upstream-sync-smoke-results.json
```

Windows Git Bash cross-build used `source tools/devenv.sh` followed by:

```sh
cargo zigbuild --release --target x86_64-unknown-linux-musl -p wb2api --locked --offline
```

Linux-specific crates were fetched once before the offline build. A short `CARGO_TARGET_DIR` was necessary to avoid Windows OpenSSL generated-object path failures.

## Explicitly not verified

- No real account, real upstream inference or actual quota consumption was used.
- The Linux binary was cross-compiled and inspected, **not executed** on Debian/Alpine. Unix-only permission/CA path tests did not run on the Windows host.
- Dockerfile/Compose/systemd received static checks only; no Docker/WSL runtime was available.
- The rewritten dashboard (`assets/web`) was walked through page by page in Chromium (dark/light, three palettes, 1360 px and 390 px, zh/en) against a gateway with demo data, and measured with a 300-account / 60,000-row dataset. There is no automated browser test suite for it yet.
- No production load test or Python-versus-Rust benchmark was performed. No performance multiplier, latency reduction or memory saving is claimed.
- The original background scheduler/reward tasks, automatic quota probes and other unsupported features are not counted as completed. See `MIGRATION.md`.

## Remaining operational limits

Account/settings writes are atomic but multi-account imports are not a multi-file transaction. Audit records on cancellation use synchronous finalization and can still be lost on storage failure or hard process kill. Usage absent from upstream is left unknown; aborted requests are not assigned fabricated token estimates. The inherited daily-total policy excludes `client_aborted` rows despite observed credits being deducted, so daily guards are not a strict cost ledger.
