# Upstream sync: v1.8.0 / 4f7ae6f

Original migration baseline: `477ba36ceea78ec4252c47a68e94a5a1612bab0b` (v1.7.32).

Reviewed upstream target: `4f7ae6f2efcae5e6ae85aefa732035206044514b` (v1.8.0).

Rust release label: **0.1.1 alpha**, a selective compatibility update. This is not a claim of full upstream v1.8.0 feature parity. Python sources/dashboard remain at the original baseline; `origin/main` was fetched without merging or resetting the local Rust work.

## Applicable changes

| Upstream commit | Change | Rust implementation |
|---|---|---|
| `392410b` | A successful request clears the credential-expired marker | `wbaccounts::Account::note_success` clears the runtime expiry flag after actual success; active restriction/model-cap protections stay intact |
| `de4d86d` | Emit first tool-argument fragment in Responses streams | `wbtranslate` now emits the initial function/custom argument delta immediately after `response.output_item.added`, preserving raw strings and final accumulation |
| `a6bb76a` | Restore 429 model windows on restart | Read `accounts/model_windows.json` on pool load/reload, restore only future model deadlines, preserve longer active windows; add Rust-side atomic window writes because Rust has no Python probe scheduler |
| `d1a8913` | Optional desktop chat protocol consistency | `WB_DESKTOP_CHAT_COMPAT` trimmed value exactly `1`; default off, WorkBuddy desktop chat only; application headers change, not body/TLS/account product |
| `5c1b15c` | Stderr writes must not block request threads | One detached writer, bounded queue2000/line8KiB, nonblocking producers, overflow counters, broken-pipe handling and bounded500ms shutdown |

### Desktop chat opt-in

```sh
export WB_DESKTOP_CHAT_COMPAT=1
```

Restart is required. `true`, `yes`, `0`, and `01` do not enable it. The same option applies to normal chat and a manually requested single-account test.

For the WorkBuddy desktop product only, it replaces browser-oriented Accept/origin headers with the upstream SDK-compatible metadata set and adds consistent B3 parent-span fields. Device credential rules, account product, endpoints, JSON/gzip bodies, billing, TLS certificate checks, CLI and VSCode behavior remain unchanged. It does not enable identity switching or restriction bypass. No new settings/UI checkbox is invented: upstream exposes this switch only through the environment.

### Persistent model windows

Canonical sidecar format:

```json
{
  "version": 1,
  "saved_at": "2026-10-27 12:00:00",
  "windows": [
    {"uid": "account-uid", "model": "gpt-5.5", "reset_at": 1793100000,
     "attempts": 0, "last_try": 0, "last_result": ""}
  ]
}
```

Legacy list/map forms and UTF-8 BOM are accepted for migration. Restoring a window does not launch a probe or replay old errors. Rust writes finite future windows observed in429responses, preserving unknown sidecar fields. Corrupt files are not silently overwritten. Expired entries are pruned; active entries are not evicted just to meet the storage bound. Account-level11140restrictions remain separate and terminal.

### Logs

Memory logs remain available even if stderr is blocked or closed. `/logs` adds `stderr_dropped` and `stderr_closed`; overflow warnings enter only the in-memory ring, never recursively re-enter stderr. The bounded shutdown does not wait indefinitely for an OS pipe consumer. This does not add SQLite log storage.

## Scheduler and reward tasks (follow-up)

Ported from upstream (reviewed against v1.9.0 `a6a967e`): the hourly scheduler (`settings.schedule`, default checkin/travel 09:00 & 21:00, keepalive 22:00, growth claim 01:00), 48 h keepalive lead (`WB_KEEPALIVE_LEAD_HOURS`, unknown expiry counts as due), CN daily check-in with the `checked/already/pending/failed` classification, cat travel, streak bonus (makeup card, gift/compensation, tier redeem, lottery), growth-task accept and claim, intl daily activity (desktop leg, web conversation, ACP turn, `session/set_model`, retries, fallback models, 10-session prune, stop/delete release), HH:MM daily slots with a 12 h catch-up window and one start-up catch-up, activity history (`usage/activity_history.jsonl`), a one-account-per-30-min balance refresher, and the panel routes `/scheduler*`, `/tasks*`, `/accounts/checkin`, `/accounts/daily-chat(-web)`, `/activity/history`.

Every task call skips restricted accounts and realm holds; a structured 11140 answer quarantines the account exactly as on the chat path. Deliberate differences: no `/v2/report` telemetry is fabricated (no CN activity-report ping, no black_cat night event, no forged growth progress); an empty `schedule.daily_chat` follows the check-in hours instead of upstream's two random slots; the scheduler `enabled` switch is in memory, as upstream. Tested only against a loopback fake upstream (`tests/tasks_smoke.py`), not with real accounts.

## Not ported in this sync

The Rust alpha already declares daily activity/growth scheduling unsupported. These commits change that absent subsystem and do not justify presenting it as implemented:

- `e8cd987`: one budget + ordered candidate models, due windows/startup catch-up, random slots and associated UI.
- `a8fc0ae`: web-session cleanup test invariants.
- `e5875ce`: independent cleanup for stuck workers/orphans.
- `eddbc00`: bounds for live/abandoned worker registries.
- `c817a66`: cleanup ledger retention, budget-aware backoff, legacy daily-chat configuration migration.
- `644a03a`: opt-in activity reporting when the web sandbox has certificate failures. This is not a generic TLS/refresh fix and does not disable certificate verification.

The dashboard diff exclusively serves these daily-chat changes. Its original snapshot is retained. Stored task configuration is preserved, but `/settings/save` rejects task configuration changes with501 instead of silently accepting unknown new keys; the runtime view declares the scheduler inactive.

`6daaae4` only increases Python release-check timeout5→10seconds. Rust remote update checks/self-update remain unsupported; token-refresh timeout is unrelated and is not changed by this commit.

`39c7b66`, `d3830a1`, `882dead`, `58111c0` are merge bookkeeping; `47775c5`, `4f7ae6f` are releases. There are **18 commits** in the complete range, not18independent features. Changelog backfills for older issues are not counted as new runtime changes.

## Provenance and testing

- `BASELINE.json` retains the original baseline and records the reviewed sync target separately.
- `/updates` and `/settings` distinguish Rust version, upstream version and sync scope; they do not claim automatic tracking.
- Existing Python golden tests remain intact; changed Responses expectations are explicitly labeled with upstream commit provenance, with fresh v1.8.0 oracle fixtures.
- `tests/upstream_sync_smoke.py` adds targeted loopback black-box regressions, complementing `tests/gateway_smoke.py`.
- Actual final test counts/build hashes are recorded in `VALIDATION.md` and local `dist/validation.json` after execution.
- Two focused smoke expectations were corrected against upstream source: `" 1"` is enabled because `d1a8913:wb_identity.py:120` explicitly uses `.strip() == "1"`; the enabled desktop profile also intentionally removes browser headers and changes Accept, rather than only adding SDK headers. The test now strictly checks those rules. Both initial 9/10 reports and correction rationale are retained locally; the same binary/source passed the final 10/10 run.

No real credentials or upstream model calls were part of this sync. The sync was initially local; subsequent authorized publication is handled by the separate Rust Actions flow described in `deploy/GITHUB_ACTIONS.md`.
