# Rust migration scope and compatibility

## Baseline

The Python reference (branch `rust-port`) is based on `477ba36ceea78ec4252c47a68e94a5a1612bab0b`, tagged `py-baseline-1.7.32`. Python sources remain the reference implementation. Upstream tracking is not automatic. Rust CI/publication targets the standalone `rust` branch of `D3-3109/workbuddy2api-hub`, which contains only the Rust workspace; the original author's repository is not a push target.

The Rust implementation is a Linux-first **alpha**, not a claim of complete feature parity. Runtime Python is not required by the Rust gateway; Python remains a development/test oracle. Supported deployment target initially is x86-64 Linux musl (Debian/Alpine). Cross-compilation is not the same as execution on Linux; platform verification is recorded separately.

The selective Rust 0.1.1 update reviews upstream through `4f7ae6f` (v1.8.0) while retaining the original source snapshot. See `UPSTREAM_SYNC.md` for commit-by-commit disposition; this is not full v1.8.0 parity or automatic tracking.

## Implemented foundations

- Tokio/Hyper HTTP/1.1 server, OpenSSL verified HTTPS client, explicit HTTP CONNECT proxies, credential-isolated keepalive pool.
- OpenAI Chat Completions streaming/non-streaming, Responses and Anthropic Messages adapters, custom/namespace tools, compaction envelope, approximate count_tokens.
- Existing account JSON, settings JSON, usage JSONL and pricing history readers; atomic settings/account writes with restrictive Unix creation permissions.
- Account selection, affinity, weighted rotation, account/key/global concurrency, daily guards, cooldown/breaker/restriction state.
- Panel sessions and passwords, API keys, account JSON import/export/edit, model catalog, usage/pricing views, in-memory logs.
- Background near-expiry token refresh and model-catalog refresh. These are not the complete original scheduler.

OAuth login start/poll/cancel, single-account testing, manual catalog refresh and live credits handlers are integrated as separate gateway modules. Credits include international Tencent envelopes, CN summary/packages and enterprise/static-key quotas. Fake-upstream and unit-test evidence is recorded in the final validation report; none of these handlers has been tested with a real upstream account.

## Intentional differences

1. Account restrictions (11140) stop dispatch and survive concurrent success, refresh, import and restart. No automatic client-identity switching or content rewriting to get around upstream restrictions.
2. User/tool text is preserved. The original prompt-fingerprint sanitizer and content-review degradation retry are not ported.
3. Ambiguously delivered POST requests are not automatically replayed after transport failure; duplicate billing/tool side effects are more important than reproducing unsafe retry behavior.
4. All concurrency slots are RAII-managed and released on failure/cancellation. Updating limits mid-request cannot leak a slot.
5. Settings responses include key concurrency limits. Invalid messages return errors instead of closing the connection through a Python exception.
6. Malformed/in-band stream errors cannot become a successful completion. Generated content is never searched for restriction phrases: only structured upstream error fields can quarantine an account. EOF requires `[DONE]` or explicit nonerror finish reasons for observed choices. Output buffers, body size, SSE lines and slow downstream writes are bounded. Request cancellation writes one audit row with raw observed usage, without inventing usage on abort; reported credit is deducted even on failure/cancellation. The inherited daily-counter policy still excludes `client_aborted` rows, so it is not a strict billable-credit ledger.
7. Loaded account filenames are retained to prevent stale duplicate files after migration. Filename collisions with another UID are rejected.
8. Non-loopback listeners require API authentication and a nondefault panel password. Disabling auth or reverting to `admin` is rejected while publicly listening. Loopback APIs also fail closed when keys are absent/disabled; anonymous access requires explicit `auth_disabled:true` with no launcher key. Deleting the last key does not silently expose a reverse-proxied loopback deployment.
9. `bucket_seconds` is accepted as an alias for `bucket`. Historical malformed UTF-8 lines are skipped rather than terminating a complete usage scan.
10. Linux runtime trust bundles are loaded explicitly for cross-built OpenSSL binaries; TLS certificate validation remains mandatory.

## Not migrated / unsupported

These are explicit limitations, not silently successful stubs. Unsupported management operations return HTTP 501; enabling unavailable features through the panel is rejected.

- Fabricated growth telemetry. The scheduler runs CN check-in, cat travel, the streak bonus and growth-task accept/claim, and the international daily activity (desktop leg plus a real web-console ACP session). It never sends `/v2/report` events, so the CN "activity report" streak ping and the night-task (black_cat) event are not ported; tasks that need such events must be completed in the client, after which the scheduler claims them.
- Automatic quota/window probing and remaining-budget estimator. Passive persistence/restoration of known 429 reset windows is implemented; it does not trigger probing.
- Local web-search/web-fetch tool execution and citations.
- Automatic account-product switching; automatic cross-realm/paid-model fallbacks; egress backoff orchestration and remaining-quota priority orchestration.
- Live OpenRouter price refresh/mapping management; existing price snapshots/history remain readable.
- SQLite persistent runtime logs and activity history views (runtime logs are memory-only).
- Desktop credential import/decryption, local desktop client configurators, portable Python/update/rollback machinery.
- SOCKS/HTTPS proxy schemes, environment proxy discovery and `no_proxy` rules. Only explicit direct/HTTP proxy egress is implemented initially.
- Remote `previous_response_id` storage, fragmented tool names after their first nonempty fragment, complete multi-choice nonstream semantics.

The management dashboard was rewritten (`assets/web`, see `docs/UI.md`). Controls for the unsupported operations above are rendered disabled with an explanation instead of failing with 501 when clicked.

## Data migration and rollback

1. Stop the Python process before pointing Rust at its data directories. The two processes must not write the same account/settings/usage files concurrently.
2. Back up `accounts/`, `usage/`, any overridden pricing directory and custom prompts. Test Rust against copies first.
3. Keep the same timezone; day-boundary counters depend on local `TZ`.
4. Preserve API keys and panel password hash. Sessions are memory-only and require login after a restart.
5. Unknown settings keys are preserved; some unsupported options retain stored values but are shown as inactive by the Rust capability view.
6. If rolling back, stop Rust and restore the backed-up data before launching the pinned Python baseline. Do not assume generated runtime/cache files are interchangeable merely because the source JSON format is compatible.

## Validation boundaries

The test oracle uses synthetic accounts, loopback upstream servers and deterministic Python fixtures. It does not prove upstream acceptance of authentication headers, actual model availability, account entitlement, rate-limit policy or end-to-end performance on a VPS.

No real account credentials have been supplied. A real smoke test should use a least-privilege test account, one small request, and no scheduler/probe loops. Stop immediately on an upstream restriction response. Credentials must be stored locally outside Git and must never appear in logs or reports.
