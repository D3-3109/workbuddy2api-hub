# 06 — Usage log, analytics endpoints, pricing, activity & run logs

Baseline: tag `py-baseline-1.7.32`. Citations are `file:line` in that tree.
Items marked **[verified]** were confirmed by running the Python functions
against the fixture data in `tests/fixtures/pricing_tooltip/` (copied to a temp
dir, no Python file modified). **[quirk]** = faithful description of current
behaviour the port may keep for parity or fix on purpose. **[skip]** = caching
machinery the port does not need (section 9 lists all of it).

Design premise for the port: every aggregate below is a pure function of
`usage/usage.jsonl` (+ price files + account pool + settings). Python carries
offsets, checkpoints, day/hour buckets, row memos and indexes only to make
those functions cheap. The Rust port may keep compact in-memory aggregates or
rescan; it must reproduce the **file formats** and the **output semantics**
described here.

---

## 0. Files in the data directory

`USAGE_DIR` = `$WB_PROXY_USAGE_DIR` or `<app>/usage` (wb_proxy.py:528-530),
overridable with `--usage-dir`. All of these live there:

| File | Owner | Format | Needed by port? |
|---|---|---|---|
| `usage.jsonl` | wb_proxy.py:530 | append-only JSONL, one row per request | **yes** (source of truth) |
| `limit-events.jsonl` | wb_proxy.py:2141 | append-only JSONL, one row per upstream 6004 cap | **yes** |
| `pricing-policies.jsonl` | wb_pricing.py:1465 | JSONL, de-duplicated price policies | **yes** |
| `pricing-timeline.jsonl` | wb_pricing.py:1466 | append-only JSONL, model->policy assignment over time | **yes** |
| `pricing-overrides.json` | wb_pricing.py:1468 | JSON map hub model -> OpenRouter id (panel-entered) | **yes** |
| `pricing-markup.json` | wb_pricing.py:1538 | JSON map model -> positive float factor (hand-edited) | **yes** |
| `activity_history.jsonl` | wb_activity.py:22 | JSONL, check-in / daily-chat attempts | **yes** |
| `logs.sqlite3` | wb_proxy.py:22781 | SQLite, optional persistent run log | **yes** (when enabled) |
| `estimate-daily.json` | wb_proxy.py:2145 | JSON, archived per-day remaining-budget estimates | optional (derivable, see 5.6) |
| `usage-aggregate-cache-<kind>.json`, `usage-aggregate-cache.json` | wb_proxy.py:8715-8716 | restart checkpoints | **[skip]** |
| `usage-recent-index.json` | wb_proxy.py:2149 | line-offset index for /usage/recent | **[skip]** |
| `usage-summary.json` | legacy, never read (usage/README.txt) | — | **[skip]** |

`pricing/pricing.json` lives next to the code (or `$WB_PRICING_FILE`),
wb_pricing.py:783-790. Pricing data files can be redirected with
`$WB_PRICING_DIR` (wb_pricing.py:1431-1439) but wb_proxy forces them into
`USAGE_DIR` at startup (`wb_pricing.set_data_dir(USAGE_DIR)`, wb_proxy.py:22821).
`wb_activity.set_data_dir(USAGE_DIR)` likewise (wb_proxy.py:22723).
`model.json` (models.dev cache) lives in the **accounts** dir (wb_modelsdev.py:79,
wb_proxy.py:18811).

**Rotation / retention of usage.jsonl: none.** The gateway only ever appends
(wb_proxy.py:929-943). It never rotates, truncates or prunes it. Readers must
tolerate the file being truncated, replaced or copy-truncated externally
(the Python folds detect this via `(size, dev, ino)` + a 64-byte tail
signature and refold from zero, wb_proxy.py:3367-3437). `limit-events.jsonl`
is also never trimmed on disk (comment wb_proxy.py:2160-2162; only the
in-memory list is capped at 1000 events, wb_proxy.py:2163, 2375-2381).

---

## 1. `usage/usage.jsonl` row schema

### 1.1 Writing

One JSON object per line, `json.dumps(row, ensure_ascii=False) + "\n"`,
appended under no lock other than the OS append (wb_proxy.py:938-941). Key
order is insertion order (see 1.2/1.3); readers must not depend on it, but
**rows this process writes always start with `{"at": `** and the Python window
pre-filter exploits that (wb_proxy.py:4762-4809) **[skip]**. A write failure
only logs `"usage persist failed: ..."` / `"error persist failed: ..."`; the
request is unaffected.

Exactly **one row per client request**, written when the request reaches a
terminal state. A stream that breaks writes only the error row (never an
error row plus a usage row) (wb_proxy.py:954-957).

Two writers:

* `record_usage(...)` (wb_proxy.py:836-920) — success, client abort, and
  "finished but failed" (empty reasoning) for paths that land on it.
  `record_upstream_usage` (wb_proxy.py:923-926) is a thin wrapper: `model` =
  the model actually billed upstream (`upstream._wb_billed_model`, falling back
  to the requested name), `requested_model` = the client's alias.
* `record_error(...)` (wb_proxy.py:945-1000) — every failure.

### 1.2 Success / client-abort row (`record_usage`)

Fields in write order (wb_proxy.py:860-908):

| Field | Type | Always? | Meaning |
|---|---|---|---|
| `at` | float | yes | `time.time()` at completion (epoch seconds, sub-second). **All windowing uses this.** |
| `iso` | string | yes | `time.strftime("%Y-%m-%dT%H:%M:%S")` — **host local time, no offset** |
| `model` | string | yes | billed model id (after alias resolution) |
| `stream` | bool | yes | client asked for SSE |
| `outcome` | string | yes | `completed` (default) / `client_aborted` / `failed` (see 1.4) |
| `elapsed_ms` | int\|null | yes (may be null) | wall time from request arrival to end |
| `ttft_ms` | int\|null | yes (may be null) | ms from request arrival to first upstream chunk |
| `gen_ms` | int\|null | yes (may be null) | `elapsed_ms - ttft_ms` |
| `requested_model` | string | only if != `model` | client-facing name / alias |
| `usage_missing` | true | only when the upstream gave no usage block | row still counts as a request |
| `prompt_tokens` | int | when usage present | `usage.prompt_tokens or 0` |
| `completion_tokens` | int | when usage present | `usage.completion_tokens or 0` (**includes** reasoning) |
| `reasoning_tokens` | int | when usage present | `usage.completion_tokens_details.reasoning_tokens or 0` |
| `cached_tokens` | int | when usage present | best cache-hit alias, see 1.5 |
| `total_tokens` | int | when usage present | `usage.total_tokens or 0` (as reported, not recomputed) |
| `credit` | number | when usage present | `usage.credit or 0` — WorkBuddy credits consumed |
| `msgs_sha`, `n_msgs`, `system_sha`, `prefix_sha` | str/int/str/str | when prompt fingerprint present | sha256(json sort_keys)[:12] of messages / count / first system msg ("" if none) / all-but-last ("" if 1 msg) (wb_proxy.py:10359-10375) |
| `account` | string | when bound to an account | account uid |
| `realm` | string | yes | account's realm, else `CURRENT_REALM` (wb_proxy.py:881-882) |
| `key` | string | yes | settings id of the client API key, `""` when none (panel session / no keys configured). Launcher key id is `"launcher"` (wb_settings.py:730). **Never the secret.** |
| `cost_policy` | string\|null | yes | price policy id in force for `model` at write time (`wb_pricing.current_policy_id`, wb_proxy.py:896). `null` when pricing is off or unpriced |
| `reasoning_effort` | string | only when the request ran with one | resolved effort |
| `tokens_per_sec` | float | only when `gen_ms > 0` | `round(completion_tokens / (gen_ms/1000), 2)` |
| `cache_hit_pct` | float | only when `prompt_tokens > 0` | `round(cached_tokens*100/prompt_tokens, 1)` |

Side effect: if the account exists and `credit` is non-zero,
`acc.consume_credit(credit)` (wb_proxy.py:886-887).

When a streamed chat response had no usage (or `total_tokens == 0`) but
produced text, the chat path **synthesizes** a usage block before recording
(wb_proxy.py:22549-22559): `completion = estimate_tokens(text)`,
`prompt = max(1, completion//2)`, `total = prompt + completion`, reasoning 0,
cached 0. Such rows are indistinguishable from real ones **[quirk]**.

Missing-key semantics matter: analytics distinguishes "no `key` field at all"
(row written before the feature existed) from `key == ""` (section 3.5).

### 1.3 Error row (`record_error`)

Fields in write order (wb_proxy.py:967-991):

| Field | Type | Always? | Meaning |
|---|---|---|---|
| `at`, `iso`, `model` | as above | yes | `model` may be `"unknown"` when the payload had none |
| `error` | `true` | yes | marks an error row |
| `outcome` | string | yes | `failed` (default) / `upstream_aborted` / `empty_reasoning` |
| `status` | int | yes | HTTP status returned to the client (429, 502, 503, 403, upstream code...) |
| `message` | string | yes | `str(message)[:200]` |
| `elapsed_ms` | int\|null | yes | |
| `stream` | bool | only if known | |
| `ttft_ms`, `gen_ms` | int | only if not None | |
| usage fields | | only if the upstream had already reported usage (aborted stream) | same extraction as 1.2 |
| fingerprint fields | | if the call site passed `fp` | |
| `account`, `realm` | | **only when an account uid is known** | error rows without an account have **no `realm`** |
| `key` | string | yes | as above |

Error rows carry **no `cost_policy`**, no `tokens_per_sec`, no
`cache_hit_pct`. When priced (they are, if they have tokens) they fall back to
the timeline (section 6.6).

Realm of a row lacking `realm` (`row_realm`, wb_proxy.py:664-683): account's
current realm from the pool → `detect_model_realm(model)` (wb_proxy.py:192) →
`"intl"`. Both filters and per-realm breakdowns use this one function.

### 1.4 Outcome values and where they come from

`row_outcome(row)` (wb_proxy.py:795-805): `row.outcome` if present, else
`"failed"` if `row.error` else `"completed"`.

| outcome | Written by | When |
|---|---|---|
| `completed` | record_usage | normal end (stream or not) |
| `client_aborted` | record_usage | client socket broke mid-stream (BrokenPipe/ConnectionReset/Aborted), with whatever usage was seen so far (wb_proxy.py:21751-21758, 22000-22004, 22489-22493) |
| `failed` | record_usage | Responses stream ended with only reasoning (`holder.empty_reasoning`, wb_proxy.py:21786) |
| `failed` | record_error | every pre-stream failure (no account, 429, 403, 503, HTTP error, compaction failure, key concurrency 429 at wb_proxy.py:22198, chat slot 503 at 22211, upstream stream finish_reason=error at 22562) |
| `upstream_aborted` | record_error | upstream stream raised mid-way; carries partial usage, `status` 502, message `"stream aborted: ..."` (wb_proxy.py:21759-21766, 22008-22011, 22528-22532) |
| `empty_reasoning` | record_error | non-stream Responses path returned reasoning without output text, status 502 (wb_proxy.py:21831-21835) |

Counting rules derived from outcome (used everywhere below):

* **completed** — the only outcome that counts as a "request"/success and
  contributes tokens to `/usage`, timeseries, by-model buckets.
* **client_aborted** — not a failure: excluded from success rate, from
  analytics entirely, from daily counters, from credit sums; counted in perf
  as `client_aborted` and in `/usage`/timeseries `errors`. **[quirk]** `/usage`
  and timeseries count it as an error while perf and analytics do not.
* anything else — an error.

### 1.5 Cached-token extraction

`_best_cached_tokens(usage)` (wb_proxy.py:806-833): first **positive** value of
`prompt_tokens_details.cached_tokens`, `prompt_cache_hit_tokens`,
`cache_read_input_tokens`, `cached_tokens`, `input_tokens_details.cached_tokens`,
`completion_tokens_details.cached_tokens`; coerced `int(float(v))`; else 0.

### 1.6 Example rows

Real rows from `tests/fixtures/pricing_tooltip/usage_sample.jsonl` (keys are
sorted and accounts anonymised in the fixture; live rows use write order):

```json
{"account": "acct-0017", "at": 1790908296.9407575, "cache_hit_pct": 12.2, "cached_tokens": 512, "completion_tokens": 1060, "cost_policy": "09bbbab71283", "credit": 0, "elapsed_ms": 6214, "gen_ms": 4679, "iso": "2026-10-02T10:31:36", "model": "deepseek-v4.1-flash", "msgs_sha": "0b50e3f688e0", "n_msgs": 2, "outcome": "completed", "prefix_sha": "51b8adb047d1", "prompt_tokens": 4187, "realm": "intl", "reasoning_tokens": 391, "stream": true, "system_sha": "07e11c1aeb24", "tokens_per_sec": 226.54, "total_tokens": 5247, "ttft_ms": 1535}
{"account": "acct-0001", "at": 1790418153.1707697, "cache_hit_pct": 0.0, "cached_tokens": 0, "completion_tokens": 64, "credit": 0, "elapsed_ms": 2337, "gen_ms": 1052, "iso": "2026-09-26T18:22:33", "model": "glm-5.3-flash", "msgs_sha": "df6e3ee440f2", "n_msgs": 2, "outcome": "completed", "prefix_sha": "a39c9f8770c3", "prompt_tokens": 32, "realm": "intl", "reasoning_tokens": 62, "stream": true, "system_sha": "8678f2a2240c", "tokens_per_sec": 60.84, "total_tokens": 96, "ttft_ms": 1285}
{"at": 1790889144.3274026, "elapsed_ms": 600112, "error": true, "iso": "2026-10-02T05:12:24", "message": "", "model": "deepseek-v4.1-flash", "outcome": "failed", "status": 524}
```

(The second row predates `key`/`cost_policy`; the third predates `key` and has
no account, so its realm is derived.)

Illustrative rows in **write order** as the current code produces them
(constructed from wb_proxy.py:862-908 / 968-991, values invented):

```json
{"at": 1792731000.12, "iso": "2026-10-22T14:50:00", "model": "deepseek-v4.1-flash", "stream": true, "outcome": "completed", "elapsed_ms": 5337, "ttft_ms": 2161, "gen_ms": 3176, "requested_model": "fast-model", "prompt_tokens": 74292, "completion_tokens": 700, "reasoning_tokens": 473, "cached_tokens": 73984, "total_tokens": 74992, "credit": 0, "msgs_sha": "e07eccbf8688", "n_msgs": 85, "system_sha": "895c557d4473", "prefix_sha": "60581c1fca86", "account": "f793acc1-...", "realm": "intl", "key": "k_3f2a", "cost_policy": "833aa685e15a", "reasoning_effort": "high", "tokens_per_sec": 220.4, "cache_hit_pct": 99.6}
{"at": 1792731100.5, "iso": "2026-10-22T14:51:40", "model": "deepseek-v4.1-flash", "error": true, "outcome": "failed", "status": 429, "message": "upstream 429: ... your usage will reset at 2026-10-23 00:50:16 UTC+8 ...", "elapsed_ms": 812, "account": "f793acc1-...", "realm": "intl", "key": ""}
{"at": 1792731200.9, "iso": "2026-10-22T14:53:20", "model": "glm-5.3", "error": true, "outcome": "upstream_aborted", "status": 502, "message": "stream aborted: timed out", "elapsed_ms": 41022, "stream": true, "ttft_ms": 1310, "gen_ms": 39712, "prompt_tokens": 1200, "completion_tokens": 300, "reasoning_tokens": 120, "cached_tokens": 0, "total_tokens": 1500, "credit": 0.4, "msgs_sha": "...", "n_msgs": 3, "system_sha": "", "prefix_sha": "...", "account": "a1b2c3d4-...", "realm": "cn", "key": "k_3f2a"}
```

### 1.7 Reading rules every aggregator shares

From `_scan_usage_from` (wb_proxy.py:3837-3933):

* blank lines skipped; lines that are not valid JSON skipped;
* a trailing line without `\n` is **not** read (writer race), it is picked up
  next time;
* a line that is not valid UTF-8 **stops** the scan (rows after it are not
  counted) **[quirk]**; the port may skip it instead.
* numeric fields: `row.get(f) or 0` everywhere (null → 0).

---

## 2. Common request conventions for usage endpoints

### 2.1 Auth

Every path starting with `/usage`, `/v1/usage`, `/activity`, `/logs`,
`/pricing` is a **panel route** (`_is_panel_route`, wb_proxy.py:18561-18592):
`do_GET` rejects it with 401 `{"error":{"message":"panel password required","type":"invalid_request_error","code":401}}`
unless the `X-Panel-Token` header carries a valid panel session
(wb_proxy.py:18654-18655). An API key alone is **not** enough, even for
`/v1/usage`. (Each handler additionally calls `_authorized()`, which a panel
session always passes, wb_proxy.py:18441-18446.)

### 2.2 Response envelope

`_json` (wb_proxy.py:18090-18117): `json.dumps(obj, ensure_ascii=False)`
(default separators `", "` / `": "`), `Content-Type: application/json; charset=utf-8`,
`Content-Length`. CORS header only for `/v1`, `/chat`, `/completions`,
`/models`, `/responses` prefixes (wb_proxy.py:375-388) — so `/v1/usage` gets
`Access-Control-Allow-Origin: *`, the others do not.

Panel endpoints go through `_json_panel` / `_json_cached_panel`
(wb_proxy.py:18175-18182), which apply `_redact_account_names`
(wb_proxy.py:10438-10478): any value under a `nickname` key that is a Chinese
mobile number (`^1[3-9]\d{9}$`) becomes `138****1234`; every other string
value has embedded phone numbers masked (separators ` \t-.·` tolerated
between 3-4-4 groups, digit boundaries required), **except** values of
identity keys `uid, account, accountId, id, file, path, machineId, sessionId,
key, token, accessToken, refreshToken` (wb_proxy.py:10408-10411).
`GET /pricing` uses plain `_json` (not redacted).

### 2.3 ETag / 304 (observable)

Endpoints using `_json_cached_panel`: `/usage`, `/v1/usage`,
`/usage/analytics`, `/usage/by-account`, `/usage/remaining`, `/usage/perf`,
`/usage/timeseries`. Behaviour (`_json_cached`, wb_proxy.py:18118-18168):

* ETag value `"<hex(int(built_at*1e6))>-<sha1(kind + "\x1f" + key)[:16]>"`
  (wb_proxy.py:17913-17933), i.e. derived from the cache entry's build time,
  **not** from the body.
* Sent only when the cache entry existed both before and after computing the
  body and was the same entry. So the **first request after a rebuild gets no
  ETag**; subsequent requests within the TTL get it.
* With an ETag: headers `Cache-Control: no-cache`, `ETag: ...`.
* `If-None-Match` matching (weak compare, `W/` ignored, `*` matches,
  comma-separated list, wb_proxy.py:17895-17910) → `304` with only
  `Cache-Control: no-cache` + `ETag`, no body, no Content-Length.

The dashboard fetches these with `cache: 'no-cache'` (dashboard.html:7916-7931)
and quantises `until` to 15 s buckets so URLs repeat (dashboard.html:16489-16498).
**Port guidance:** ETag is optional for correctness (without it the browser
simply gets 200s). If kept, any stable validator works (e.g. hash of body, or
generation counter); keep the 304 header set above.

### 2.4 TTLs that are observable as staleness

| Data | TTL | Source |
|---|---|---|
| `/usage`, `/usage/perf`, `/usage/analytics`, `/usage/by-account`, `/usage/timeseries`, daily counters | `WB_STATS_TTL` default 15 s | wb_proxy.py:1820 |
| `/usage/remaining` | `WB_REMAINING_TTL` default 15 s | wb_proxy.py:2166 |
| `/usage/recent` `total` | `WB_COUNT_TTL` default 30 s | wb_proxy.py:4660 |
| pricing gap report | 30 s | wb_pricing.py:2506 |

A port that is always fresh is acceptable; it just shows numbers sooner.

### 2.5 Time-range parameters (`range`, `since`, `until`)

`range_query` reads `range`, `since`, `until` (first value each,
wb_proxy.py:784-794). `range_window` (wb_proxy.py:747-781) resolves them to
`(lo, hi)` epoch bounds; `None` = unbounded:

| `range` (case-insensitive, trimmed) | lo | hi |
|---|---|---|
| `today`, `day`, `1d` | local midnight today | None |
| `week`, `w` | local midnight of this **Monday** | None |
| `month`, `m` | local midnight on the 1st of this month | None |
| `custom` | `since` | `until` |
| anything else / missing (incl. `7d`, `30d`, `all`) | None | None (all history) |

* `since`/`until` parse as float; empty, unparsable or negative → None
  (wb_proxy.py:730-744). Reversed bounds are swapped.
* There is **no `7d`/`30d` here** (deliberately, wb_proxy.py:758-761). The
  dashboard expresses 3/30/365-day views as `range=custom&since=now-N*86400&until=now`
  (dashboard.html:16499-16506). `7d`/`30d` exist only in `/activity/history`.
* Local midnight = `mktime(y, m, d, 0,0,0, isdst=-1)` in the **host timezone**
  (wb_proxy.py:708-715). The port must use the host's local zone (TZ env) the
  same way; DST days are 23/25 h.
* Bounds are **inclusive on both sides** in every aggregator:
  rows kept iff `lo <= at <= hi` (wb_proxy.py:1139-1142, 4159-4163,
  4916, 7103-7106). Note `since=0` behaves as "no bound" in `/usage` and perf
  (`if since and ...`) **[quirk]**.

### 2.6 Realm parameter

`realm_scope(realm, fallback)` (wb_proxy.py:692-705): `"all"` → no filter;
empty → `fallback`; else filter rows with `row_realm(row) == realm`.

| Endpoint | realm source | default |
|---|---|---|
| `/usage`, `/usage/perf`, `/usage/recent`, `/usage/timeseries` | `?realm=` → `X-Realm` header → `CURRENT_REALM` | current exit |
| `/usage/analytics` | `?realm=` only | **all realms** |
| `/usage/by-account`, `/usage/remaining` | none | all |

`CURRENT_REALM` = `$WB_PROXY_DEFAULT_REALM` or `"intl"`, switchable via
`POST /realm` (wb_proxy.py:191, 6018-6035).

---

## 3. Usage endpoints

All GET. Route table at wb_proxy.py:18670-18697.

### 3.1 `GET /usage` and `GET /v1/usage` — totals snapshot

Handler `_get_v1_usage` (wb_proxy.py:18861-18873) → `usage_snapshot`
(wb_proxy.py:3288-3305) → `_usage_snapshot_uncached` (wb_proxy.py:4571-4603).

Query: `realm`, `range`, `since`, `until` (section 2). Header `X-Realm`.

Per-row fold (`_fold_usage_snapshot`, wb_proxy.py:4130-4219), for rows that pass
realm and `lo <= at <= hi`:

* outcome **not completed** → `errors += 1`; if outcome is not
  `client_aborted`, also `credit += row.credit` and the row's cost is folded
  (failed requests that consumed upstream tokens still cost money). Nothing
  else. No by-model contribution.
* outcome **completed** → `requests += 1`; each of `prompt_tokens,
  completion_tokens, reasoning_tokens, cached_tokens, total_tokens, credit`
  that is **present** in the row is added (`row[k] or 0`); cost folded; and the
  same numbers go into three nested buckets:
  `by_model[model]`, `by_model_realm[model][row_realm]`,
  `by_model_acct[model][row_realm][account or "(unattributed)"]`.
  `model` missing → `"unknown"`. Each bucket also counts
  `accounts[uid] += 1` when the row has an account.
* Cost fold (`_fold_cost`, wb_proxy.py:3321-3334): priced → `cost_cny += cny`
  (bucket-level `cost_cny` only for completed rows); unpriced and pricing not
  disabled → `cost_missing[model or "unknown"] += 1`. Pricing disabled → neither.

Output (top-level key order as produced; **[verified]** against the fixture):

```json
{
  "requests": 495, "errors": 1,
  "prompt_tokens": 51818553, "completion_tokens": 1774523,
  "reasoning_tokens": 1441178, "cached_tokens": 48629248,
  "total_tokens": 53593076, "credit": 6.81,
  "cost_cny": 16.129016759762507,
  "cost_missing": {},
  "started": 1792739802.3328428,
  "by_model": {
    "deepseek-v4.1-flash": {"requests": 492, "accounts": {"acct-0002": 32, "acct-0003": 198},
      "cost_cny": 16.1226864452, "prompt_tokens": 51816656, "completion_tokens": 1774399,
      "reasoning_tokens": 1441064, "cached_tokens": 48629248, "total_tokens": 53591055, "credit": 6.81}
  },
  "by_model_realm": {"deepseek-v4.1-flash": {"intl": { "...same bucket shape..." : 0 }}},
  "by_model_acct":  {"deepseek-v4.1-flash": {"intl": {"acct-0002": {"requests": 32, "accounts": {"acct-0002": 32}, "cost_cny": 1.75422100013, "prompt_tokens": 4650989, "completion_tokens": 210687, "reasoning_tokens": 193048, "cached_tokens": 3434240, "total_tokens": 4861676, "credit": 0}}}},
  "ttft_ms_sum": 0, "ttft_samples": 0, "gen_ms_sum": 0, "gen_samples": 0,
  "wall_ms_sum": 0, "wall_samples": 0,
  "since": "2026-10-23 07:16:42",
  "log_file": "/app/usage/usage.jsonl",
  "realm": "all",
  "usd_cny": 7.1,
  "accounts_map": {"<uid>": {"nickname": "138****1234", "realm": "intl"}},
  "account": {"uid": "", "domain": "", "issuer": "", "credential_file": "",
              "expires_at": 0, "accounts": 0, "accounts_ready": 0}
}
```

Field semantics:

* `requests` = completed rows; `errors` = all other rows (incl. client aborts).
* `credit` = sum over completed + non-aborted failed rows.
* `cost_cny` = estimated OpenRouter-equivalent cost in **CNY**; USD =
  `cost_cny / usd_cny` (panel divides).
* `ttft_ms_sum` … `wall_samples` are **always 0** (legacy accumulators kept in
  the shape, `_empty_stats` wb_proxy.py:600-614) **[quirk]**.
* `started` = process start epoch; `since` = it formatted local
  `%Y-%m-%d %H:%M:%S` (wb_proxy.py:4586-4587). **Not** the window start.
* `log_file` absolute path of usage.jsonl.
* `realm` = resolved filter or `"all"`.
* `usd_cny` = `wb_pricing.usd_cny()` (section 6.7).
* `accounts_map` = every pool account (unfiltered) → `{nickname, realm}`.
* `account` = representative account of the realm (first with an access
  token, else first) (wb_accounts.py:4479-4484): `issuer` from the JWT,
  `credential_file` basename, `accounts`/`accounts_ready` pool counts.

Windowed totals in Python are computed by summing day/hour buckets; the only
visible difference vs. a row-by-row sum is float association order in
`credit`/`cost_cny` (last ulp). The port should just sum rows.

### 3.2 `GET /usage/perf` — latency percentiles

Handler wb_proxy.py:19080-19094 → `perf_stats` → `_perf_stats_uncached`
(wb_proxy.py:1095-1277).

Query: `sample` (int, clamped `[10, 20000]`, default 5000; unparsable → 5000),
`realm`, `range`, `since`, `until`; header `X-Realm`.

**Sampling:** only the **last `sample` non-blank lines** of usage.jsonl are
considered (`_tail_lines`, wb_proxy.py:4604-4651), *then* realm/window filters
apply. So a week window with > `sample` rows describes only its newest slice.

* `sample_capped` = `number_of_tail_lines >= sample`.
* `sample_from` = `at` of the **first parsable line** of the tail (before any
  realm/window filter) — the start of the sampled slice. `null` if none.

Per kept row (`total += 1`), grouped also by model (`model or "unknown"`),
`(model, row_realm)`, and `(model, row_realm, account or "(unattributed)")`:

* `client_aborted` → `client_aborted += 1`; `elapsed_ms` (if truthy) added to wall samples. Stop.
* outcome != completed → `errors += 1`; truthy `elapsed_ms` → wall samples. Stop.
* completed → `success += 1`; append `ttft_ms` (if not null), `gen_ms` (if not
  null), `elapsed_ms` (if not null), `tokens_per_sec` (if truthy),
  `cache_hit_pct` (if not null) to the respective sample lists.

Metric block (`_perf_block`, wb_proxy.py:1010-1034) — `null` when no samples:

```
avg = round(sum/count, 1)
pXX = sorted[ clamp(int(round((XX/100)*(count-1))), 0, count-1) ]   # nearest rank, Python round = banker's
max = max(samples); samples = count
```

`round()` is Python's round-half-to-even both for the index and for `avg`
**[quirk]**; percentiles return the raw sample value (int stays int).

`success_rate_pct` = `round(ok*100/(ok+err), 1)` or `null` when `ok+err == 0`
(client aborts excluded from both).

Output **[verified]**:

```json
{
  "sampled": 496, "sample_from": 1790418153.1707697, "sample_capped": false,
  "success": 495, "errors": 1, "client_aborted": 0, "success_rate_pct": 99.8,
  "ttft_ms":        {"avg": 2559.4, "p50": 2324, "p90": 4047, "p99": 6721, "max": 9745, "samples": 495},
  "generation_ms":  {"avg": 13581.6, "p50": 3176, "p90": 43931, "p99": 126435, "max": 159159, "samples": 495},
  "wall_ms":        {"avg": 17318.3, "p50": 5947, "p90": 47486, "p99": 128864, "max": 600112, "samples": 496},
  "tokens_per_sec": {"avg": 268.2, "p50": 259.06, "p90": 328.32, "p99": 385.54, "max": 1400.0, "samples": 495},
  "cache_hit_pct":  {"avg": 82.6, "p50": 98.8, "p90": 99.8, "p99": 99.9, "max": 100.0, "samples": 495},
  "by_model": {
    "deepseek-v4.1-flash": {"requests": 493, "errors": 1, "client_aborted": 0, "success_rate_pct": 99.8,
      "ttft_ms": {"...": 0}, "generation_ms": {}, "wall_ms": {}, "tokens_per_sec": {}, "cache_hit_pct": {}}
  },
  "by_model_realm": {"<model>": {"<realm>": {"...same per-model shape..."}}},
  "by_model_acct":  {"<model>": {"<realm>": {"<account>": {"...same per-model shape..."}}}}
}
```

Note `sampled` counts rows after filters; per-model `requests` = all rows of
that model incl. errors and aborts. `cache_hit_pct` avg is a mean of per-row
percentages (82.6) — different from the token-weighted ratio in analytics (93.8).

### 3.3 `GET /usage/timeseries` — bucketed series

Handler wb_proxy.py:19060-19078 → `usage_timeseries` (wb_proxy.py:4717-4742)
→ scope wb_proxy.py:4676-4714 → scan wb_proxy.py:4863-4958.

Query: `realm` (+`X-Realm`, default current), `range`/`since`/`until`,
`bucket` (int seconds). **[quirk]** The handler reads `bucket`, but the
dashboard's heat-map sends `bucket_seconds=86400` (dashboard.html:16583),
which is ignored; it still gets 86400 because its custom span exceeds 14 days.
Port: accept both names.

Window: `(lo, hi) = range_window(...)`; then `hi = now` if None,
`lo = hi - 86400` if None (so no range = last 24 h, **not** all history).

Step: explicit `bucket` → `max(60, int(bucket))`; else span
`<= 6h` → 60, `<= 14d` → 3600, else 86400.

Bucket grid origin: step 86400 → local midnight of `lo`; 3600 → local hour
start of `lo`; 60 → `floor(lo/60)*60`; any other step → `lo` itself. Origin
keeps `lo`'s numeric type (int when lo is an int, else float), so `at` values
serialize as `1790870400` vs `1790870400.0` **[quirk]**. In practice `lo` is
always a float (range_window returns floats), so `at` is a float.
Bucket index `k = floor((at - origin)/step)`; bucket `at = origin + k*step`.
Only buckets with at least one row are emitted (**sparse**), sorted by `at`.

Rows kept: realm match and `lo <= at <= hi`. Per row:
completed → `requests += 1` and the five token fields; otherwise `errors += 1`;
`credit += row.credit` unless client_aborted. No cost in the series.

`credits`: the **50 most recent rows in the window with `credit > 0`**
(any outcome, including client aborts **[quirk]**), sorted by `at`
descending, ties in log order; entry
`{"at", "iso", "model", "account", "credit", "total_tokens"}`
(wb_proxy.py:8113-8123).

Output **[verified]**:

```json
{"ok": true, "realm": "all", "bucket_seconds": 3600,
 "since": 1790870400.0, "until": 1790956800.0,
 "series": [
   {"at": 1790870400.0, "requests": 1, "errors": 0, "prompt_tokens": 113952, "completion_tokens": 328,
    "reasoning_tokens": 56, "cached_tokens": 113664, "total_tokens": 114280, "credit": 0.0}
 ],
 "credits": [{"at": 1792731000.1, "iso": "2026-10-22T14:50:00", "model": "glm-5.3",
              "account": "a1b2...", "credit": 0.4, "total_tokens": 1500}]}
```

`since`/`until` are the resolved numeric bounds (until = now when open).

### 3.4 `GET /usage/recent` — paginated raw rows

Handler wb_proxy.py:18875-18887 → `recent_usage` (wb_proxy.py:5818-5895).

Query: `limit` (int, clamped `[1, 1000]`, default 100; unparsable → 100),
`page` (int ≥ 1, default 1), `realm` (+`X-Realm`, default current; `all` = none).

Semantics: page 1 = newest. Matching rows (realm filter only, **no time
filter**) in reverse file order; page `p` = items `[(p-1)*limit, p*limit)`
(fallback reader wb_proxy.py:5767-5815; the index path returns identical
rows). `total` = number of matching rows. **[quirk]** Without a realm filter
`total` counts every non-blank line including unparsable ones
(wb_proxy.py:5035-5072); with a realm filter it counts lines whose literal
`"realm": "<x>"` / `"realm":"<x>"` text matches, parsing + `row_realm` only for
lines with no `realm` key. `total_pages = max(1, ceil(total/limit))`;
`page` is clamped to `total_pages`.

Each returned row is the raw JSON object from the log **plus** these keys
appended (wb_proxy.py:5863-5887), from `wb_pricing.cost_for_row(row)` with
full details (section 6.6):

| Key | Value when priced | Unpriced |
|---|---|---|
| `key_name` | display name of `key` (name, else id; `""` for empty key; unknown id echoed) (wb_proxy.py:577-597) | same |
| `cost_cny` | `round(cny, 6)` | null |
| `cost_band` | band index or null | null |
| `cost_source` | policy id or `"builtin"` | null |
| `cost_source_at` | policy `first_seen` (float), null for builtin | null |
| `cost_backfilled` | bool | false |
| `cost_rates` | `{"input_cache_hit","input_cache_miss","output"}` USD per unit | null |
| `cost_unit` | float `1000000.0` | null |
| `cost_currency` | `"USD"` | null |
| `cost_usd_cny` | float | null |
| `cost_or_id` | OpenRouter id | null |
| `cost_via` | `direct` / `override` / `variant` | null |
| `cost_inherited_from` | base model (variant) or null | null |
| `cost_override_from` | hub model name (override) or null | null |
| `cost_band_note` | Chinese description of the band or null | null |
| `cost_via_derived` | bool (via inferred for legacy policies) | null |

`markup`/`markup_from` are computed by `cost_for_row` but **not** copied onto
the row; `cost_cny` already includes the markup factor.

Output **[verified]**:

```json
{"total": 496, "page": 1, "limit": 2, "total_pages": 248, "usd_cny": 7.1,
 "rows": [
  {"account": "acct-0015", "at": 1790922220.4000738, "cache_hit_pct": 99.6, "cached_tokens": 73984,
   "completion_tokens": 700, "cost_policy": "833aa685e15a", "credit": 0, "elapsed_ms": 5337, "gen_ms": 3176,
   "iso": "2026-10-02T14:23:40", "model": "deepseek-v4.1-flash", "msgs_sha": "e07eccbf8688", "n_msgs": 85,
   "outcome": "completed", "prefix_sha": "60581c1fca86", "prompt_tokens": 74292, "realm": "intl",
   "reasoning_tokens": 473, "stream": true, "system_sha": "895c557d4473", "tokens_per_sec": 220.4,
   "total_tokens": 74992, "ttft_ms": 2161,
   "key_name": "", "cost_cny": 0.01164, "cost_band": null, "cost_source": "833aa685e15a",
   "cost_source_at": 1790920226.6980796, "cost_backfilled": false,
   "cost_rates": {"input_cache_hit": 0.015, "input_cache_miss": 0.015, "output": 0.75},
   "cost_unit": 1000000.0, "cost_currency": "USD", "cost_usd_cny": 7.1,
   "cost_or_id": "deepseek/deepseek-v4.1-flash", "cost_via": "direct",
   "cost_inherited_from": null, "cost_override_from": null, "cost_band_note": null,
   "cost_via_derived": false}
 ]}
```

Check: `(74292-73984)*0.015 + 73984*0.015 + 700*0.75 = 1639.38` → `/1e6*7.1 = 0.011640`.

### 3.5 `GET /usage/analytics` — metrics page (accounts / models / keys)

Handler wb_proxy.py:19034-19044 → `compute_usage_analytics`
(wb_proxy.py:6539-6560) → `_compute_usage_analytics_uncached`
(wb_proxy.py:8634-8683).

Query: `realm` (**default: all realms**, no X-Realm fallback), `range`,
`since`, `until`. The dashboard calls it as `?realm=all&<range params>`.

The payload has two halves: **all_time** (every row in the realm, ignoring the
window) and **window** (rows with `lo <= at <= hi`; equals all_time when no
window). Both use the same per-row fold (`_fold_usage_analytics`,
wb_proxy.py:6686-6778):

1. Realm filter (`row_matches_realm`).
2. `client_aborted` rows are **skipped entirely** (wb_proxy.py:6717-6718).
3. `is_err = outcome != "completed"`.
4. `cost_cny` = priced cny or 0.0.
5. Values: `prompt_tokens, completion_tokens, reasoning_tokens, cached_tokens,
   total_tokens, credit` (`or 0`), `ttft_ms`, `tokens_per_sec`, `elapsed_ms`.
6. Feed (`_feed_analytics`, wb_proxy.py:6636-6665) into: the summary stat,
   the account entry (`account or "(unattributed)"`), the model entry
   (`model or "(unknown)"` — note: different placeholder from `/usage`'s
   `"unknown"` **[quirk]**), the key entry.
   * `errors += 1` if is_err else `requests += 1`;
   * **tokens, credit and cost are added for error rows too** (unlike `/usage`);
   * `ttft_sum/ttft_n` only when `ttft_ms` truthy (0 is skipped);
     same for `speed_*` (`tokens_per_sec`) and `elapsed_*` (`elapsed_ms`).
7. Model distribution `{model: {requests, tokens, reasoning, cost_cny}}` for
   accounts and keys: **completed rows only** (`_bump_analytics_models`,
   wb_proxy.py:6668-6683); `tokens` = total_tokens, `reasoning` = reasoning_tokens.
8. Key id: row has no `key` field → `"__before_keys__"`; `key == ""` →
   `"__no_key__"`; else the id (wb_proxy.py:6730-6733). Per key also
   `realms[row_realm or ""] += 1` and `last_at = max(at)`.

Stat object (`_new_analytics_stat` + `_finalize_analytics_stat`,
wb_proxy.py:6576-6586, 8622-8632):

```json
{"requests": 495, "errors": 1,
 "prompt_tokens": 51818553, "completion_tokens": 1774523, "reasoning_tokens": 1441178,
 "cached_tokens": 48629248, "total_tokens": 53593076, "credit": 6.81, "cost_cny": 16.129016759762507,
 "ttft_sum": 1266886.0, "ttft_n": 495, "speed_sum": 132747.34, "speed_n": 495,
 "elapsed_sum": 8589896.0, "elapsed_n": 496,
 "cache_hit_pct": 93.8, "reasoning_ratio": 81.2,
 "ttft_ms_avg": 2559, "speed_avg": 268.2, "elapsed_ms_avg": 17318}
```

* `cache_hit_pct = round(cached/prompt*100, 1)` (0.0 when prompt == 0) — token-weighted.
* `reasoning_ratio = round(reasoning/completion*100, 1)` (0.0 when completion == 0).
* `ttft_ms_avg = round(ttft_sum/ttft_n)` → **int** (0 when n == 0).
* `speed_avg = round(speed_sum/speed_n, 1)` (0.0).
* `elapsed_ms_avg = round(elapsed_sum/elapsed_n)` → int (0).
* `credit`, `cost_cny`, the `*_sum` fields are floats (accumulators start at 0.0).

Top-level output:

```json
{
  "window": {"since": null, "until": null},
  "realm": "all",
  "usd_cny": 7.1,
  "summary": {"window": {"...stat..."}, "all_time": {"...stat..."}},
  "accounts": [
    {"uid": "acct-0003", "nickname": "acct-0003", "realm": "intl", "domain": "",
     "window": {"...stat..."}, "all_time": {"...stat..."},
     "window_models": {"deepseek-v4.1-flash": {"requests": 198, "tokens": 21205467, "reasoning": 262693, "cost_cny": 4.698}},
     "all_models": {"...": {}},
     "credits": {"...account.credits snapshot, only for pool accounts..."}}
  ],
  "models": [{"model": "deepseek-v4.1-flash", "window": {"...stat..."}, "all_time": {"...stat..."}}],
  "keys": [ "...see below..." ]
}
```

* `window.since/until` = resolved bounds (null when open).
* `accounts`: every account seen in the log (realm-filtered) **plus** every
  pool account of that realm with zero traffic (`_enrich_accounts_from_pool`,
  wb_proxy.py:8597-8619). For pool accounts `nickname`, `realm`, `domain`,
  `credits` (the account's credits dict or `{}`) are overwritten from the pool;
  accounts only in the log keep `nickname = uid`, `domain = ""`, `realm` = the
  realm field of the **first** row seen (may be `""`), and have **no `credits`
  key**. Sorted by `(-window.total_tokens, -all_time.total_tokens)`, stable.
* `models`: sorted the same way.
* Sorting is stable over dict insertion order = first appearance in the log.

Key rows (`_build_key_rows`, wb_proxy.py:8493-8594):

```json
{"key": "__before_keys__", "name": "(切换前)", "realm": "", "enabled": false, "source": "bucket",
 "window": {"...stat..."}, "all_time": {"...stat..."},
 "window_models": {"...": {}}, "all_models": {"...": {}},
 "realms": {"intl": 485, "cn": 11}, "last_at": 1790922220.4000738,
 "models": [{"model": "deepseek-v4.1-flash", "requests": 492, "tokens": 53591055, "reasoning": 1441064},
            {"model": "hy4-preview-f", "requests": 1, "tokens": 967, "reasoning": 24}],
 "models_other": null,
 "cross_realm": true}
```

Row set:

1. One row per configured panel key (`wb_settings.api_keys`), `source: "panel"`,
   `name = entry.name or id`, `realm` = declared binding, `enabled` =
   `entry.enabled is not False`. Keys bound to a different realm than the
   query realm are omitted.
2. `launcher` row (`name "启动参数"`, `source "launcher"`) only if the log has
   traffic for id `launcher` and no panel key uses that id.
3. `__before_keys__` → name `(切换前)`, `__no_key__` → name `(无 key)`, both
   `source "bucket"`, `enabled false`.
4. Every other id seen in the log but not in the roster is merged into one row
   `__unknown_key__` named `(未知 key)` (stats summed; its model maps sum
   `requests/tokens/reasoning` only, **no cost_cny** **[quirk]**).
5. Panel keys that are disabled **and** have zero window requests and errors
   are dropped.
6. Rows with no traffic carry zero stats, `{}` model maps, `realms {}`, `last_at 0`.

Derived: `models` = top 5 window models by `(-tokens, -requests, model)`,
entries without cost; `models_other` = `null` or
`{"model": "(其他)", "other": true, "models": n, "requests", "tokens", "reasoning"}`
(wb_proxy.py:8470-8490); `cross_realm` = more than one non-empty realm in
`realms`. Sort: `(-window.total_tokens, -all_time.total_tokens, name)`.

### 3.6 `GET /usage/by-account`

Handler wb_proxy.py:19046-19051 → `_usage_by_account_uncached`
(wb_proxy.py:6510-6521). No parameters, **no realm or time filter**: whole log.

Fold (`_fold_usage_by_account`, wb_proxy.py:6448-6472): skip rows with
`error` truthy (**client_aborted and failed record_usage rows are counted**,
since they carry no `error` field **[quirk]**). Per `account or "(unattributed)"`:
`requests += 1`, sum the five token fields, `models[model or "?"] += 1`.

Output **[verified]** — sorted by `-total_tokens`, `models` = top 5 as
`[model, count]` pairs:

```json
{"accounts": [
  {"account": "acct-0003", "requests": 198, "prompt_tokens": 20850024, "completion_tokens": 355443,
   "reasoning_tokens": 262693, "cached_tokens": 20358912, "total_tokens": 21205467,
   "models": [["deepseek-v4.1-flash", 198]]}
]}
```

### 3.7 `GET /usage/remaining` — see section 5.

---

## 4. Daily usage counters (feed the daily guards; not an endpoint)

`daily_usage_stats()` (wb_proxy.py:1857-1943) folds rows with
`at >= local midnight today` into three maps, used by
`apply_daily_guards` (wb_proxy.py:2008-2067) to park accounts
(`AccountPool.apply_daily_token_limit/credit/model`, wb_accounts.py:4024-4130):

| Map | Key | Value |
|---|---|---|
| `tokens` | account uid | Σ `total_tokens` |
| `credits` | account uid | Σ `credit` (only non-zero added) |
| `models` | uid → model | Σ `total_tokens` |

Row rules (wb_proxy.py:1882-1899): skip `client_aborted`; skip rows without
`account`; **error rows count** (their tokens/credit, usually 0). Day
boundary = local midnight; the key is `time.strftime("%Y-%m-%d")`, and a day
change resets the maps.

Consumers: settings `daily_token_limit`, `daily_credit_limit`,
`model_daily_token_limit` (per realm, `wb_settings.limit_values`). The fold
only runs when at least one limit is > 0; an unreadable log yields `None`
(never park) rather than zero (wb_proxy.py:1905-1910). The credit guard
exempts free models (`free_models_by_realm`, catalog credits string in
`x0.00, x0, 0, 0.00`, wb_proxy.py:3231-3271). Observable through `GET /accounts`
(daily limit badge; out of scope here) — `seconds_until_local_midnight()` is
at least 60 s (wb_proxy.py:1958-1962).

Port: a compact per-day `HashMap<uid, ...>` updated on every appended row,
reset at local midnight, rebuilt from the log at startup, gives identical
numbers.

---

## 5. Remaining-usage estimate and `limit-events.jsonl`

Big comment block: wb_proxy.py:2070-2139. Endpoint `GET /usage/remaining`
(wb_proxy.py:19053-19058), no parameters.

### 5.1 Concept

The upstream gives each (account, model) a free window; when exhausted it
answers 429 code 6004 with a reset wall clock. The budget is unknown, so it
is **learned**: at the moment of a cap, the usage of that (account, model)
since the window start is a budget *sample*. Remaining = budget − used since
the current window start.

### 5.2 `limit-events.jsonl` format

Written by `note_limit_event(account, model, reset_at)` (wb_proxy.py:2470-2509),
called from the request path when a 429 is classified as model-scoped and the
body carried a reset time (wb_proxy.py:14674-14680). One line per cap:

```json
{"at": 1792732216.33, "account": "f793acc1-...", "model": "deepseek-v4.1-flash", "realm": "intl", "reset": 1792775416.0, "sample": 190044211}
```

| Field | Meaning |
|---|---|
| `at` | when the 429 was handled |
| `account`, `model`, `realm` | the capped pair; realm of the account (`""` if unknown) |
| `reset` | epoch of the upstream reset clock |
| `sample` | tokens used by (account, model) in `[window_start, at]` (see 5.4) |

Reset clock parsing (`parse_rate_limit_reset`, wb_proxy.py:14200-14240):
`(?:reset at|将在)\s*(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})`, fallback
`(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})\s*UTC[+-]\d{1,2}`; offset from
`UTC([+-]\d{1,2})(?::?(\d{2}))?` (default 0); epoch =
`timegm(stamp) - offset`. Body is unescaped first (`_unescape_body`).

### 5.3 Inputs folded

* Usage buffer per (uid, model): `(at, total_tokens)` of rows with an
  account and a model that are **not** `error`, **not** `usage_missing`,
  **not** `client_aborted` (wb_proxy.py:2512-2547). Only the last 26 h are
  needed (`_REMAINING_KEEP_SECONDS`, wb_proxy.py:2159). Range sums are
  inclusive `lo <= at <= hi` (wb_proxy.py:2317-2329).
* Events, deduplicated by `(account, model, reset)`, first source wins:
  (a) every line of `limit-events.jsonl`; (b) every usage row with
  `status == 429` whose `message` parses to a reset time (account+model
  required) — its sample computed at fold time from the buffer up to that
  row. Only the newest 1000 events are kept in memory.
* Anchors per (uid, model): `{reset: at}` from events plus the account's
  persisted `model_caps[model] = {"at", "reset"}` (credential file, see
  wb_accounts); same reset → keep the later `at` (wb_proxy.py:2403-2419,
  2944-2964).

### 5.4 Window rules

* `LIMIT_WINDOW_SECONDS = 86400`.
* Window start for a reset `R` (`_cap_window_start`, wb_proxy.py:2431-2443):
  `max(previous anchor reset < R (0 if none), R - 86400)`.
* Sample at cap = usage in `[window_start(R), at]`.

### 5.5 Budget per (realm, model)

`_remaining_budgets` (wb_proxy.py:2807-2860), 3-day weighted:

* Day keys `[today, yesterday, day-before]` (local `YYYY-MM-DD`), weights
  `6, 3, 1` (wb_proxy.py:2152).
* For a day: events with `sample > 0` whose `at` falls on that local day,
  realm = event.realm or the account's realm; per account the mean of its
  samples, then across accounts: `avg = round(mean of account means)`,
  `min`, `max` of account means, `n` = number of accounts
  (`_budget_means`, wb_proxy.py:2587-2595; every mean rounded with Python
  `round`, half-to-even).
* Today is computed live; yesterday and the day before come from
  `estimate-daily.json` (5.6). A Rust port without that file can compute
  them live from the events if it retains ≥ 3 days of events and usage — the
  numbers are the same as long as the inputs are.
* Combined: `avg = round(Σ w*avg_d / Σ w)` over days that have data;
  `min`/`max` over those days' `min`/`max`; `n = Σ n_d`;
  `days = [{"day","w","avg","n"}...]` in today→older order.

### 5.6 `estimate-daily.json`

Written atomically (`.tmp` + replace, compact separators) when the local date
changes during a payload build, or on the first build after start
(archives "yesterday", idempotent: a day already in the file is never
overwritten) (wb_proxy.py:2618-2775):

```json
{"schema":1,"days":{
  "2026-10-21":{"budgets":{"intl\u001fdeepseek-v4.1-flash":{"avg":190044211,"min":179700000,"max":201000000,"n":7}},
                "segments":{"f793acc1-...\u001fdeepseek-v4.1-flash":{"pre":120000000,"post":35000000}}}}}
```

Keys are `realm + "\x1f" + model` and `uid + "\x1f" + model`. `segments`:
per (uid, model) with any usage that day, `post` = usage after each reset
falling on that day up to the next cap or day end, `pre = total - post`
(half-open `[lo, hi)` day ranges, wb_proxy.py:2777-2804). Retention: only
the last 3 day keys are kept; `segments` only for the last 2
(wb_proxy.py:2689-2726). Nothing reads `segments` back except the file
itself (not in any payload) **[skip-able]**.

### 5.7 Payload rows

`_remaining_payload_locked` (wb_proxy.py:2928-3120). Accounts = pool accounts
that are enabled and have a uid.

**Activity filter:** `floor` = local midnight of *yesterday*
(wb_proxy.py:2446-2455). (realm, model) is *active* if any account of that
realm has a buffered usage row with `at >= floor`, or an anchor with
`max(at, reset) >= floor`.

For each account, the model set = models with a budget in its realm that are
active ∪ models the account itself used (in buffer) that are active. Per
(uid, model), sorted by model:

* `past` anchors = reset ≤ now; `future` = reset > now.
* **Cooling** (a future anchor exists, take the earliest `upcoming`):
  `reset_at = upcoming.reset`, `window_start = cap_window_start(reset_at)`,
  `end = min(now, upcoming.at or reset_at)`, `used = sum[window_start, end]`,
  one segment `{start, end, used, capped: true, current: true}`,
  `cooling = true`, `estimated = false`, `remaining = 0`.
* **After a reset** (latest past anchor `last`): `reset_at = last.reset`,
  `window_start = max(reset_at, now-86400)`, `used = sum[window_start, now]`,
  `estimated = reset_at < now - 86400`. If not estimated: segments =
  (if `last.at > 0`) previous segment `{start: cap_window_start(reset_at),
  end: min(last.at, reset_at), used, capped: true, current: false}` then
  current `{start: reset_at, end: now, used, capped: false, current: true}`.
  If estimated: `segments = []`.
* **Never capped**: `window_start = now-86400`, `used = sum[window_start, now]`,
  `reset_at = null`, `estimated = true`, `segments = []`.
* `remaining`: cooling → 0; else budget → `max(0, budget.avg - used)`; else null.
* `used_pct = round(used*100/budget.avg, 1)` when budget and avg; else null
  (also computed for cooling rows).

Each segment also has `start_iso`, `end_iso` (local `%Y-%m-%d %H:%M:%S`).

Row:

```json
{"uid": "f793acc1-...", "nickname": "138****1234", "realm": "intl", "model": "deepseek-v4.1-flash",
 "used": 35123456, "budget": 190044211, "budget_min": 179700000, "budget_max": 201000000, "samples": 7,
 "remaining": 154920755, "used_pct": 18.5,
 "window_start": 1792775416.0, "window_iso": "2026-10-23 00:50:16",
 "reset_at": 1792775416.0, "reset_iso": "2026-10-23 00:50:16",
 "cooling": false, "estimated": false,
 "segments": [
   {"start": 1792689016.0, "end": 1792732216.33, "used": 190044211, "capped": true, "current": false,
    "start_iso": "...", "end_iso": "..."},
   {"start": 1792775416.0, "end": 1792790000.0, "used": 35123456, "capped": false, "current": true,
    "start_iso": "...", "end_iso": "..."}]}
```

Without a budget: `budget`, `budget_min`, `budget_max` are null and
`samples` is 0. Rows sorted by
`(remaining is null, remaining or 0, realm, nickname, model)`.

### 5.8 Payload top level

```json
{"window_seconds": 86400,
 "generated_at": 1792790000.0, "generated_iso": "2026-10-23 04:53:20",
 "activity_floor": 1792598400.0, "activity_floor_iso": "2026-10-21 00:00:00",
 "budgets": [
   {"avg": 190044211, "min": 179700000, "max": 201000000, "n": 7,
    "days": [{"day": "2026-10-23", "w": 6, "avg": 190000000, "n": 2}],
    "realm": "intl", "model": "deepseek-v4.1-flash", "active": true,
    "samples_list": [{"account": "...", "at": 1792732216.33, "at_iso": "...", "reset": 1792775416.0,
                      "reset_iso": "...", "sample": 190044211}]}],
 "models": [
   {"realm": "intl", "model": "deepseek-v4.1-flash", "total_remaining": 1540000000, "budget": 190044211,
    "pairs": 12, "available": 9, "cooling": 2, "unknown": 1, "estimated": 3}],
 "rows": ["...see 5.7..."]}
```

* `budgets` sorted by `(realm, model)`; `samples_list` = in-memory events of
  that (realm, model), matched on `event.realm` (events with an empty realm
  are not listed), sorted by `(at, account)` (wb_proxy.py:2863-2880).
* `models` aggregates rows per (realm, model): `pairs` = row count;
  `cooling`; `estimated`; `unknown` = rows with `remaining` null;
  `available` = rows with a number that are not cooling; `total_remaining` =
  Σ non-null remaining (null when none). Sorted by
  `(total_remaining is null, -total_remaining, realm, model)`.

### 5.9 Scheduling weights (internal)

When setting `remaining_priority_enabled` is on, rows with
`remaining/budget <= 0.05 → 4`, `<= 0.15 → 3`, `<= 0.30 → 2` get dispatch
weight > 1 (wb_proxy.py:3170-3224). Not an endpoint.

---

## 6. Pricing (OpenRouter-equivalent cost estimate)

Module docstring: wb_pricing.py:1-40. The cost is **an estimate of what the
same tokens would cost at OpenRouter's model-level list price**, in CNY. It
is unrelated to the `credit` field (WorkBuddy's own billing).

### 6.1 Master switches (settings.json in the accounts dir)

| Key | Default | Effect |
|---|---|---|
| `pricing_enabled` | true (missing = on; only literal `false` disables) | off → no fetch, no policy minted, `cost_policy` written as null, every row unpriced with `disabled: true` (so not listed in `cost_missing`) (wb_settings.py:1481-1490, wb_pricing.py:2310-2317) |
| `pricing_variant_inherit` | true | allow suffix-stripping inheritance (6.4) (wb_settings.py:1456-1468) |
| `pricing_refresh_minutes` | 5.0 (0 = off; max 43200; legacy `pricing_refresh_hours` migrated ×60) | refresher interval (wb_settings.py:25-32, 1386-1440) |

Set via `POST /settings` (`pricing_enabled` must be a JSON bool → else 400;
`pricing_refresh_minutes` number ≥ 0, legacy `pricing_refresh_hours` ×60)
(wb_proxy.py:19590-19623). Changing either wakes the refresher.

### 6.2 `pricing/pricing.json` (factory snapshot)

Loaded from `$WB_PRICING_FILE` or `<app>/pricing/pricing.json`; if missing,
unreadable, or lacking a `models` object, the copy embedded in wb_pricing.py
(`_JSON`, wb_pricing.py:55-775) is used. Reloaded when mtime changes
(wb_pricing.py:793-824). Generated by `tools/_fetch_pricing.py` (6.8).

```json
{
  "meta": {
    "generated_at": "2026-10-02T01:39:39",
    "base_currency": "CNY",
    "usd_cny": 7.1,
    "sources": {"openrouter": "https://openrouter.ai/api/v1/models"},
    "note": "单价均为「每百万 token」的美元价 ..."
  },
  "models": {
    "deepseek-v4.1-flash": {"display": "deepseek-v4.1-flash", "source": "openrouter", "currency": "USD",
      "unit": 1000000, "or_id": "deepseek/deepseek-v4.1-flash",
      "flat": {"input_cache_hit": 0.01, "input_cache_miss": 0.03, "output": 0.5}},
    "gpt-6-astra": {"display": "gpt-6-astra", "source": "openrouter", "currency": "USD", "unit": 1000000,
      "or_id": "openai/gpt-6-astra",
      "flat": {"input_cache_hit": 1.0, "input_cache_miss": 10.0, "output": 50.0},
      "bands": [{"flat": {"input_cache_hit": 2.0, "input_cache_miss": 20.0, "output": 75.0},
                 "min_prompt_tokens": 272000}]},
    "hy4-preview-f": {"display": "hy4-preview-f", "source": "openrouter", "currency": "USD", "unit": 1000000,
      "or_id": "tencent/hy4-preview",
      "flat": {"input_cache_hit": 0.0378, "input_cache_miss": 0.7506, "output": 2.2509},
      "bands": [{"flat": {"input_cache_hit": 0.042, "input_cache_miss": 0.834, "output": 2.501},
                 "days": null, "start": 0, "end": 1600},
                {"flat": {"input_cache_hit": 0.0378, "input_cache_miss": 0.7506, "output": 2.2509},
                 "days": null, "start": 1600, "end": 2400}]}
  }
}
```

Model entry fields: `flat` = USD per `unit` (1,000,000) tokens for
`input_cache_hit`, `input_cache_miss`, `output` (any may be null);
optional `bands` (6.5); optional `via` (`variant`/`override`),
`inherited_from`, `override_from` (display only). The shipped file has 42
models.

### 6.3 Policies and timeline (runtime price history)

**`pricing-policies.jsonl`** — one JSON object per line, keyed by content hash
(`load_policies`: later duplicate id wins, wb_pricing.py:1677-1693):

```json
{"id": "09bbbab71283", "model": "deepseek-v4.1-flash", "or_id": "deepseek/deepseek-v4.1-flash", "currency": "USD", "unit": 1000000, "usd_cny": 7.1, "flat": {"input_cache_hit": 0.02185, "input_cache_miss": 0.02185, "output": 0.63}, "bands": null, "first_seen": 1790908275.292091}
{"id": "9513f9c1f441", "model": "deepseek-r1-0528-lkeap", "...": "...", "via": "variant", "inherited_from": "deepseek-r1-0528", "override_from": null, "first_seen": 1790919000.0}
```

New rows (wb_pricing.py:2023-2039) always include `via` (`direct` default),
`inherited_from`, `override_from`; older rows lack them (fixture
`policies_live_before.jsonl`).

**Policy id** (`policy_id`, wb_pricing.py:1664-1674) — must be reproduced
bit-exactly because usage rows reference it:

```
payload = json.dumps({"model": model, "flat": flat, "bands": bands or None,
                      "usd_cny": round(float(usd_cny), 6)},
                     sort_keys=True, ensure_ascii=False)        # separators ", " and ": "
id = sha1(payload.utf8).hexdigest()[:12]
```

Nested dicts are key-sorted too (`sort_keys` is recursive); floats use
Python `repr` (`7.1`, `0.02185`, `1.0`, `272000` int). `via`/`inherited_from`/
`override_from` are **not** in the hash. **[verified]** e.g. payload
`{"bands": null, "flat": {"input_cache_hit": 0.02185, "input_cache_miss": 0.02185, "output": 0.63}, "model": "deepseek-v4.1-flash", "usd_cny": 7.1}`
→ `09bbbab71283`. Test oracle: tests/_test_pricing_tooltip.py:82-125
recomputes every id in the fixtures. A Rust port needs a Python-compatible
float formatter (shortest round-trip repr, `.0` for integral floats, ints stay
ints — OpenRouter-derived values are floats from `round(x*1e6, 6)`,
`min_prompt_tokens`/`start`/`end` are ints).

**`pricing-timeline.jsonl`** — append-only, one line per change of the full
assignment (`load_timeline` sorts by `at`, ignores rows without a
`policies` object, wb_pricing.py:1696-1709):

```json
{"at": 1790908275.292091, "policies": {"deepseek-v4.1-flash": "09bbbab71283", "gpt-6-astra": "7014b582a4ad", "hy4-preview-f": "3f0ad98de26e"}}
```

The last row is "in force now" (`current_assignment`, wb_pricing.py:1712-1720).

**Recording** (`record_policies(doc, at, merge)`, wb_pricing.py:1992-2059),
under one reentrant lock:
1. For each model in `doc.models`: `pid = policy_id(model, flat, bands,
   doc.meta.usd_cny or 7.10)`; append a policy row if `pid` is new
   (`first_seen = at`).
2. Assignment = `{model: pid}` for the doc's models (`merge=True`: start from
   the current assignment and overlay).
3. Append a timeline row only if the assignment differs from the current one.
Returns `(new_policy_count, timeline_changed, assignment)`.

**Pruning** (`prune_policies(protect)`, wb_pricing.py:2082-2116), run after a
refresh that added policies: keep ids in the current assignment, ids
referenced by any usage row's `cost_policy` (string scan of usage.jsonl for
lines containing `"cost_policy"`), the protected batch, and for each model
whose ids are all otherwise unreferenced, its newest (`first_seen`) policy.
Rewrite the file atomically (`.tmp` + replace) in original order.

**`pricing-overrides.json`** — `{"hub-model": "vendor/or-id"}`, pretty JSON
(`indent=2, sort_keys=True`), written by `save_runtime_override`
(wb_pricing.py:1579-1599); blank values ignored on read.

**`pricing-markup.json`** — `{"model": factor}`, hand-edited; non-numeric or
≤ 0 factors ignored; default 1.0 (wb_pricing.py:1530-1576).

### 6.4 Matching hub model → OpenRouter id

`resolve(hub_id, or_models, by_norm, variants)` (wb_pricing.py:1303-1328):

1. `_resolve_once(hub_id)`: runtime overrides file, then the source table
   `OVERRIDES` (wb_pricing.py:1003-1020) — a hit only if the target id exists
   in the fetched catalogue → `(ref, via_override=True)`; else `auto_match`:
   normalize (`lowercase, strip [^a-z0-9]`) the part after the last `/` and
   require **exactly one** OpenRouter id (ids containing `:` excluded) with the
   same normalized leaf.
2. If no hit and variants enabled: for each suffix in `VARIANT_SUFFIXES` with
   strip=True (`-lkeap`, `-taiji`, `-volc`, `-sg`; never `-f`, `-dev`, `-x`)
   that the name ends with, retry `_resolve_once(base)`; hit → `(ref, True)`
   and the entry is marked `via: "variant"`, `inherited_from: base`.

`entry_for` (wb_pricing.py:1341-1368) builds
`{"display","source":"openrouter","currency":"USD","unit":1000000,"or_id","flat"}`
+ `bands` if any + `via/inherited_from` (variant) or `via:"override",
override_from: hub_id` (override).

OpenRouter → rates (`rates_from`, wb_pricing.py:1075-1081):
`input_cache_hit = per_million(input_cache_read)`,
`input_cache_miss = per_million(prompt)`, `output = per_million(completion)`;
`per_million(v) = round(float(v)*1e6, 6)`, unparsable/missing → null.
Bands from OpenRouter `overrides[]` (`bands_from_overrides`,
wb_pricing.py:1084-1124): item with `min_prompt_tokens` →
`{"flat": rates, "min_prompt_tokens": int}`; item with any `utc_*` key →
`{"flat": rates, "days": [mon..sun short names] or null, "start": int(utc_start or 0), "end": int(utc_end) or 2400}`;
other items dropped. `input_cache_write`, audio fields are ignored (no matching
token counts in the log).

Hub ids priced (`priced_model_ids`, wb_pricing.py:1140-1178): static intl
catalogue then static cn catalogue (wb_catalog `STATIC_INTL_MODELS`,
`STATIC_CN_MODELS`), plus live-catalogue ids from both realms that are not in
the static list (`PriceRefresher.live_ids`, wb_pricing.py:2416-2452, via
`curated_live_sources(realm)`, wb_proxy.py:11449).

### 6.5 Band selection

`band_for(entry, at, prompt_tokens)` (wb_pricing.py:847-893):

* Evaluate every band **in order; the last one that applies wins**.
* Band with `min_prompt_tokens`: applies iff `prompt_tokens >= threshold`.
* Otherwise a UTC window: `(weekday, hhmm)` of `at` in **UTC**
  (`gmtime`, `hhmm = hour*100 + minute`); `days` non-empty and weekday not in
  it → no; else applies iff `start <= hhmm < (end or 2400)`.
* A band applies only if its `flat` is a dict. No band → the entry's `flat`,
  band index `null`.

### 6.6 Cost of one row

`cost_for_row(row, details=True, enabled=None)` (wb_pricing.py:2283-2350) is
the single source of truth (per-row view and every aggregate use it).

1. Pricing disabled (or empty row) →
   `{"cny": 0.0, "known": false, "band": null, "source": "builtin", "source_at": null, "backfilled": false, "disabled": true}`
   (+ all detail keys null).
2. Policy lookup: `pid = row.cost_policy`; if absent/null →
   `policy_ref_at(row.at, row.model)` (below), `backfilled` from it.
3. If `pid` resolves in `pricing-policies.jsonl` → price with that policy
   (`entry = policy` fields; `meta.usd_cny = policy.usd_cny or 7.10`);
   `source = pid`, `source_at = policy.first_seen`.
4. Else → the factory snapshot (6.2): `source = "builtin"`,
   `source_at = null`, `backfilled = false`. Model missing there → unpriced
   (`known: false`).

`policy_ref_at(at, model)` (wb_pricing.py:2138-2168), timeline sorted by `at`:
* index = last timeline row with `row.at <= at` (clamped to 0 when `at` is
  before the first row); if that row lists the model → `(pid, false)`;
* else the first row with `row.at >= at` that lists it → `(pid, true)`;
* else scanning backwards from index → `(pid, true)`;
* empty timeline / never listed → `(None, false)`.

Arithmetic (`compute_row`, wb_pricing.py:918-954), all floats:

```
unit     = entry.unit or 1e6
prompt   = row.prompt_tokens
cached   = min(row.cached_tokens, prompt)
uncached = prompt - cached
output   = row.completion_tokens          # reasoning is INSIDE completion; never added again
miss     = rate.input_cache_miss or 0
hit      = rate.input_cache_hit, falling back to input_cache_miss when null
out      = rate.output or 0
factor   = meta.usd_cny if entry.currency == "USD" (case-insens.) else 1.0   (usd_cny 0/missing -> 1.0)
cny      = (uncached*miss + cached*hit + output*out) / unit * factor
cny     *= markup_for(model)  if markup != 1.0
```

There is **no cache-write price** and **no separate reasoning price**.
`credit` plays no role. Missing token fields count as 0.

Return value (details=True) **[verified]**:

```json
{"cny": 0.00545165406, "known": true, "band": 1,
 "rates": {"input_cache_hit": 0.0378, "input_cache_miss": 0.7506, "output": 2.2509},
 "source": "3f0ad98de26e", "source_at": 1790908275.292091, "backfilled": false,
 "unit": 1000000.0, "currency": "USD", "usd_cny": 7.1, "or_id": "tencent/hy4-preview",
 "via": "override", "inherited_from": null, "override_from": "hy4-preview-f",
 "band_note": "每天 16:00–24:00 UTC", "via_derived": true,
 "markup": 1.0, "markup_from": null}
```

```json
{"cny": 30.3525, "known": true, "band": 0,
 "rates": {"input_cache_hit": 2.0, "input_cache_miss": 20.0, "output": 75.0},
 "source": "7014b582a4ad", "...": "...", "via": "direct",
 "band_note": "输入长度 ≥ 272,000 tokens", "via_derived": false, "markup": 1.0, "markup_from": null}
```

(second: prompt 300000, cached 100000, completion 1000 →
`(200000*20 + 100000*2 + 1000*75)/1e6*7.1`.)

Unpriced: `cny 0.0, known false, band null, source "builtin", source_at null,
backfilled false` and every detail key null.

### 6.7 "Cost source" tooltip details

Detail keys, fixed order `rates, unit, currency, usd_cny, or_id, via,
inherited_from, override_from, band_note, via_derived, markup, markup_from`
(wb_pricing.py:2180-2182); all null when unpriced.

* `rates` = copy of the band's (or flat) table actually used.
* `unit` float; `currency` (default "USD"); `usd_cny` float from the
  policy/meta.
* `via`/`inherited_from`/`override_from`/`via_derived` (`match_source`,
  wb_pricing.py:2211-2225): if the entry records `via` in
  (`variant`,`override`,`direct`) use it with `via_derived=false`; else
  (legacy policy, or builtin entry without `via`) if
  `runtime_overrides.get(model) or OVERRIDES.get(model)` equals the entry's
  `or_id` → `("override", null, model, true)`; else `("direct", null, null, false)`.
* `band_note` (wb_pricing.py:2191-2208): length band →
  `"输入长度 ≥ {threshold:,} tokens"`; window band →
  `"每周 周一/周三 HH:MM–HH:MM UTC"` when days given (names from
  `mon..sun → 周一..周日`), else `"每天 HH:MM–HH:MM UTC"` (en dash U+2013;
  `2400 → "24:00"`); null for flat price.
* `markup` = factor; `markup_from = "pricing-markup.json"` if ≠ 1.0 else null.

`usd_cny()` for payloads (wb_pricing.py:979-984) = factory snapshot
`meta.usd_cny` (pricing.json), default 1.0. **FX source:** a constant
`USD_CNY = 7.10` (wb_pricing.py:992) written into every generated snapshot
and every new policy; there is **no live FX fetch**. A policy's own
`usd_cny` is frozen at creation and part of its hash.

### 6.8 Refresh: OpenRouter fetch and schedule

* URL `https://openrouter.ai/api/v1/models`, header
  `User-Agent: workbuddy2api-hub-pricing/1.0`, timeout 45 s; result
  `{m.id: m.pricing}` for `payload.data[]` (wb_pricing.py:1127-1137). No auth.
* `PriceRefresher` thread (wb_pricing.py:2369-2575), created at startup with
  `pricing_refresh_minutes` (wb_proxy.py:22827-22829):
  * pricing off or interval 0 → park until woken (settings change);
  * on first loop iteration: fetch immediately only if the timeline is empty;
    after coming back from "off" → fetch immediately;
  * then wait `interval*60` s, fetch, repeat. `set_interval` / `wake` break
    the wait.
* `run_once` (wb_pricing.py:2454-2499): re-check master switch; fetch (failure
  → `last_error`, log `"抓取失败，继续用上一版价格：..."`, nothing changes);
  `build_snapshot(or_models, previous=factory models, extra_ids=live_ids)`
  (wb_pricing.py:1371-1414); if no model matched → error
  `"抓到了模型列表，但一个都没匹配上"`; else `record_policies(doc, at=now)`,
  remember the catalogue in memory for on-demand pricing, prune if new
  policies were added, log
  `"已取价：N 个模型（新增策略 a，人工覆盖 o，未定价 u[，live 新增 x][，清理无用策略 r]）"`.
  Log lines go to the refresher's own ring (last 40, `"[YYYY-mm-dd HH:MM:SS] msg"`)
  and to the gateway log with prefix `[定价]`, tag `pricing`.
* **On-demand pricing** (`ensure_policy`, wb_pricing.py:1802-1841): when a
  request is recorded for a model with no assignment, the last successful
  in-memory catalogue (never the network) is used to resolve it and
  `record_policies(..., merge=True)` adds it; returns the new pid or null.
  Called from `current_policy_id` (wb_pricing.py:1723-1741), i.e. inside
  `record_usage`. Cold catalogue (no fetch yet this process) → null.
* `tools/_fetch_pricing.py` (offline CLI): fetches, builds the snapshot from
  the **static** catalogue (+ `--extra-ids-file`), writes
  `pricing/pricing.json` (`indent=2`, `ensure_ascii=False`, trailing newline);
  `--dry-run` prints; `--embed` also rewrites the `_JSON = r'''...'''` block
  in wb_pricing.py; on fetch failure reuses the previous file's models or
  exits. Not needed at runtime.

### 6.9 models.dev (context/output limits, not prices)

`wb_modelsdev.py` is **not** part of pricing; it fills `context_length` /
`max_output_tokens` in `/v1/models` (wb_proxy.py:10965-10980). Lookup per
field: upstream value > built-in table `CONTEXT_FALLBACK`
(wb_modelsdev.py:35-69) > `accounts/model.json` cache > default
(context 1,000,000; output omitted). Cache file format:
`{"<bare-lowercase-id>": [context, output]}`. Refresh: async GET
`https://models.dev/api.json` (UA `wb2api-hub`, 5 s timeout, 32 MiB cap) at
most once per 300 s process-wide, triggered by `GET /v1/models`
(wb_proxy.py:18811); index built from `provider.models[id].limit.{context,output}`,
vendor providers (`zai, moonshotai, moonshotai-cn, openai, google, deepseek,
minimax`) win, else most common pair (ties → smaller), values > 1e9 dropped
(wb_modelsdev.py:139-178).

### 6.10 Pricing HTTP endpoints

**`GET /pricing`** (panel; wb_proxy.py:19124-19141, `PriceRefresher.status`
wb_pricing.py:2576-2619). Not redacted. **[verified]** shape:

```json
{"interval_minutes": 5.0, "enabled": true, "master_enabled": true, "running": true,
 "last_run": 1790920226.69, "last_run_label": "2026-10-02 13:50",
 "next_run": 1790920526.69, "next_run_label": "2026-10-02 13:55",
 "last_error": null, "last_models": 48, "last_extra": 2,
 "policies": 52, "models": 48,
 "current_at": 1790920226.6980796, "current_at_label": "2026-10-02 13:50",
 "current": {
   "deepseek-r1-0528": {"id": "1cc7b267520d", "label": "2026-10-02 10:31", "via": "direct", "inherited_from": null},
   "deepseek-r1-0528-lkeap": {"id": "9513f9c1f441", "label": "2026-10-02 13:08", "via": "variant", "inherited_from": "deepseek-r1-0528"}},
 "gaps": [{"model": "default-model", "reason": "alias"},
          {"model": "kimi-k2-instruct-taiji", "reason": "variant_unmatched", "candidates": ["moonshotai/kimi-k2"]},
          {"model": "some-new-model", "reason": "or_missing", "candidates": []}],
 "gap_summary": {"total": 4, "or_missing": 3, "variant_unmatched": 1, "aliases": 5, "variants_enabled": true},
 "variant_inherit": true,
 "overrides_file": "/app/usage/pricing-overrides.json",
 "policies_file": "/app/usage/pricing-policies.jsonl",
 "timeline": "/app/usage/pricing-timeline.jsonl",
 "logs": ["[2026-10-02 13:50:26] 已取价：48 个模型（新增策略 0，人工覆盖 9，未定价 5）"]}
```

* `enabled = master_enabled && interval_minutes > 0`; `running` = thread alive.
* Labels: local `%Y-%m-%d %H:%M`, null when the stamp is null.
* `policies` = rows in the policy table; `models` / `current` = current
  assignment (sorted by model), `label` = the policy's `first_seen`, `via`
  default `"direct"`.
* `gaps` (`gap_items`, wb_pricing.py:1901-1946; capped at 200, cached 30 s):
  for each priced-input id not in the current assignment, not in the factory
  snapshot and not resolvable against the in-memory catalogue: reason
  `alias` (name in `VIRTUAL_ALIAS_MODELS`, no candidates), `variant_unmatched`
  (has a strippable suffix), else `or_missing`. `candidates` = up to 3
  OpenRouter ids (`suggest_matches`, wb_pricing.py:1865-1888: substring of
  normalized leaf → `0.75 + 0.25*min/max len`, else difflib ratio; keep ≥ 0.6;
  sort by `(-score, id)`). `gap_summary.total = or_missing + variant_unmatched`.
* Fallback when the refresher object does not exist (wb_proxy.py:19129-19141):
  `{"interval_minutes": 0.0, "enabled": false, "running": false,
  "master_enabled": <setting>, "policies": 0, "models": 0, "current": {},
  "logs": [], "gaps": [], "gap_summary": {"total": 0, "or_missing": 0,
  "variant_unmatched": 0, "aliases": 0, "variants_enabled": <setting>},
  "variant_inherit": <setting>, "overrides_file", "policies_file",
  "timeline", "msg": "未运行"}`.

**`POST /pricing/refresh`** (panel session required, else 401; body parsed but
ignored; wb_proxy.py:20944-20956, 22083-22096):
* no refresher → `200 {"ok": false, "msg": "价格刷新未运行"}`;
* pricing off → `200 {"ok": false, "msg": "价估算已关闭，请先启用"}`;
* else start `run_once` in a background thread and answer
  `200 {...GET /pricing status..., "ok": true, "msg": "已开始抓取，稍候刷新查看结果"}`.

**`POST /pricing/mapping`** body `{"model": "...", "or_id": "vendor/id" | ""}`
(wb_proxy.py:20974-21012):
* same two `ok:false` replies as above;
* `model` empty → 400 `"model is required"`;
* `or_id` non-empty and not matching `^[A-Za-z0-9._\-]+/[A-Za-z0-9._\-:]+$`
  → 400 `"or_id must look like 'vendor/model'"`;
* writes/removes the entry in `pricing-overrides.json`, invalidates the gap
  cache, starts a refresh thread, logs `[定价] 面板手填映射：...`;
* reply `{"ok": true, "model", "or_id", "known": <or_id in last catalogue>,
  "msg"}` with msg `已删除 X 的手填映射，下次取价起按自动匹配` /
  `已记录 X → Y，正在重新取价` / `已记录 X → Y；本次快照里还没看到该条目，取价后仍可能显示未定价`.

Settings view (`GET /settings`) also exposes `pricing_refresh_minutes`,
`pricing_variant_inherit`, `pricing_enabled` (wb_proxy.py:10151-10158).

### 6.11 Where cost shows up

| Endpoint | Fields | Rows included |
|---|---|---|
| `/usage` | `cost_cny`, `cost_missing`, per-bucket `cost_cny`, `usd_cny` | completed + non-aborted failed (top level); completed only (buckets) |
| `/usage/analytics` | every stat's `cost_cny`, model maps' `cost_cny`, `usd_cny` | all non-aborted rows (stats); completed (model maps) |
| `/usage/recent` | `cost_*` per row, `usd_cny` | each row |
| `/usage/timeseries`, `/usage/perf`, `/usage/by-account` | none | — |

Every aggregate prices each row against the policy in force when the row
happened (its own `cost_policy`, else the timeline), so a later price change
never rewrites history — except rows without `cost_policy` whose model was
first priced later (backfilled) and rows falling back to the factory
snapshot, which move when pricing.json changes.

---

## 7. Activity history — `activity_history.jsonl` and `GET /activity/history`

### 7.1 File

`USAGE_DIR/activity_history.jsonl` (wb_activity.py:22, 74-90). One line per
**real attempt** of a check-in (cn) or daily-chat (intl) task, written by
`record_attempt(account, task, trigger, result)` from wb_accounts.py:2031,
2370, 2466 (`trigger=None` = nested call, not recorded).

```json
{"ts": "2026-10-22T08:00:03+08:00", "uid": "f793acc1-...", "nickname": "13812345678", "realm": "cn", "task": "checkin", "trigger": "scheduler", "ok": true, "message": "签到成功"}
```

| Field | Values |
|---|---|
| `ts` | local time with offset, `isoformat(timespec="seconds")` (wb_activity.py:167-169) |
| `uid`, `nickname` | account (nickname stored unmasked; masked on output by the panel redaction) |
| `realm` | `cn` / `intl` / `""` |
| `task` | `checkin` / `daily_chat` |
| `trigger` | `scheduler`, `manual`, `account_add`, `account_import`, `unknown` (anything else → `unknown`) |
| `ok` | bool |
| `message` | `result.msg or result.error or ("成功"/"失败")`, whitespace-collapsed, secrets redacted, ≤ 200 chars |

`safe_message` (wb_activity.py:114-132) replaces with `[redacted]`:
`(bearer|basic) <8+ token chars>`; `access_token|refresh_token|api_key|apikey|authorization|cookie|set-cookie|password|secret|token` `[:=]` value;
JWT-looking `eyJ...` triples; `sk-` keys of 12+; hex ≥ 32; base64 ≥ 40.

Retention (wb_activity.py:53-62, 341-386): after an append, compact when the
last compaction was ≥ 6 h ago (first write after start always compacts) or
the file > 2,000,000 bytes: drop rows older than 90 days (rows with
unparsable `ts` are kept), then keep the newest 5000; atomic rewrite
(`.tmp` + replace). Write failures never fail the task.

### 7.2 Endpoint

`GET /activity/history` (panel; wb_proxy.py:18889-18910 → `wb_activity.query`,
wb_activity.py:285-330). Query: `range`, `uid`, `task`, `result`, `limit`.

* `range` (default `7d`): `today` = since local midnight; `1d`/`7d`/`30d`/`90d`
  = rolling `now - N*86400`; `all` = no bound. Other values → **400**
  `"range must be one of: today, 1d, 7d, 30d, 90d, all"`.
* `task`: empty or `checkin`/`daily_chat`, else 400 `"task must be one of: checkin, daily_chat"`.
* `result`: empty or `ok|success|succeeded|failed|fail|failure` (case-insens.),
  normalized to `ok`/`failed`, else 400 `"result must be one of: ok, failed"`.
* `limit`: int clamped `[1, 1000]`, default 100.
* `uid`: exact match after trim.

Rows are read newest-first (reverse file order), **projected** to exactly
the allowed fields with type/length normalization (`project`,
wb_activity.py:147-164: `ts[:40]`, `uid[:80]`, `nickname[:120]`, realm/task
whitelisted else `""`, trigger else `unknown`, `ok` via truthy-string parse
(`1,true,yes,ok,success`), message re-sanitized), filtered, and in a bounded
range rows whose `ts` cannot be parsed are excluded.

```json
{"rows": [{"ts": "2026-10-22T08:00:03+08:00", "uid": "f793acc1-...", "nickname": "138****5678",
           "realm": "cn", "task": "checkin", "trigger": "scheduler", "ok": true, "message": "签到成功"}],
 "total": 14, "limit": 100, "range": "7d",
 "filters": {"uid": "", "task": "", "result": ""}}
```

`total` = matches before the limit; `filters.result` is the normalized value.
The 400 uses the OpenAI-style error envelope. The panel calls
`/activity/history?range=today&limit=200` and `/activity/history?<filters>`.

---

## 8. Run logs — memory ring, `logs.sqlite3`, `/logs*`

### 8.1 Entry

`add_log_entry(msg, level, tag)` (wb_proxy.py:10529-10567) — every `log()`
call (also echoed to stderr as `[wb-proxy] HH:MM:SS <masked msg>`):

```json
{"id": 1234, "ts": "2026-10-23 14:02:11", "time": "14:02:11", "level": "INFO", "tag": "chat", "msg": "chat done: model=... acct=f793acc1 key=alice 5337ms tokens=74992 (in=74292 out=700) 220.4t/s"}
```

* `id` monotonically increasing per process; on start with the DB enabled it
  resumes from `MAX(id)` in the DB (wb_proxy.py:10643-10650).
* `level` given, else inferred from lowercase text: any error hint
  (`error, exception, failed, fail:, reject, traceback, errno, fatal, 11-128,
  11101, 11140, 失败, 异常, 错误, 拒绝, 不可用, 无效`) → `ERROR`; any warn hint
  (`warn, warning, retry, timeout, throttl, 重试, 超时, 告警, 警告, 降级, 限流`)
  → `WARN`; else `INFO` (wb_proxy.py:10486-10494).
* `tag` given, else first matching group in `_LOG_TAG_HINTS`
  (wb_proxy.py:10500-10516): chat, scheduler, tasks, accounts, pricing,
  egress, upstream, webtools, catalog, auth, settings, update, usage, agents;
  else `system`.
* Inference runs on the raw text; `msg` stored **phone-masked**.
* Memory ring: last 2000 entries (wb_proxy.py:10377).

### 8.2 `logs.sqlite3` (optional)

`wb_logstore.LogStore(USAGE_DIR/logs.sqlite3)` (wb_logstore.py, wb_proxy.py:22781).
Enabled by setting `log_db_enabled: true` (default **off**: no file is
created). Caps (settings, each blank = unlimited): `log_db_max_rows`,
`log_db_max_days` (**default 7**), `log_db_max_bytes`
(wb_settings.py:213-229). Applied at startup and on every settings save.

Schema (wb_logstore.py:48-59):

```sql
CREATE TABLE IF NOT EXISTS logs (
    id    INTEGER PRIMARY KEY,
    ts    TEXT NOT NULL,   -- "YYYY-mm-dd HH:MM:SS" local
    time  TEXT NOT NULL,   -- "HH:MM:SS"
    level TEXT NOT NULL,
    tag   TEXT NOT NULL,
    msg   TEXT NOT NULL    -- truncated to 64 KiB
);
CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level);
CREATE INDEX IF NOT EXISTS idx_logs_tag   ON logs(tag);
```

Pragmas: `auto_vacuum=INCREMENTAL` (effective only on a new file),
`journal_mode=DELETE`, `synchronous=NORMAL`. Single worker thread, batched
`INSERT OR REPLACE` (≤ 256 per transaction). Retention: every 5 s (also when
idle) `DELETE WHERE ts < now-days` (string compare of local timestamps) and
keep newest `rows`; every 30 s if the file (+`-wal`) exceeds `bytes`, delete
oldest rows down to ~85 % of the cap by average row size and `VACUUM`
(≤ 4 passes) (wb_logstore.py:334-406).

### 8.3 Endpoints

**`GET /logs`** (panel; wb_proxy.py:19192-19217). Query: `limit` (int, default
200; unparsable → 200; no clamp), `level` (exact, upper-cased), `tag`
(case-insensitive exact), `search` (case-insensitive substring of msg or tag),
`since_id` (int, default 0).

Semantics (`get_logs`, wb_proxy.py:10586-10613; SQL twin `LogStore._query`,
wb_logstore.py:293-321) — DB is used when enabled, memory ring otherwise or
on DB error/timeout (8 s):
* filter `id > since_id` (when > 0), level, tag, search;
* `total` = count of matches;
* `since_id == 0` and `limit > 0` → the **last** `limit` matches, oldest
  first; otherwise **all** matches after `since_id` (limit ignored);
* `max_id` = last returned id, else `since_id`.

```json
{"total": 5321, "logs": [{"id": 5320, "ts": "...", "time": "...", "level": "INFO", "tag": "chat", "msg": "..."}],
 "max_id": 5321, "mode": "db", "stored": 5321, "db_bytes": 1843200}
```

`mode` = `"db"` / `"memory"`; `stored` (row count) and `db_bytes` only in db
mode (stats failures → 0).

**`GET /logs/export`** (panel; wb_proxy.py:19219-19238): `text/plain;
charset=utf-8`, `Content-Disposition: attachment; filename="wb-proxy-YYYYmmdd-HHMMSS.log"`,
body = last 5000 entries, one per line
`[<ts>] [<level>] [<tag>] <masked msg>`, joined by `\n` (no trailing newline).

**`POST /logs/clear`** (panel session; wb_proxy.py:20970-20972): clears the
memory ring and, if enabled, `DELETE FROM logs`; reply `{"ok": true}`.

Requests to `/logs*`, `/usage*`, `/accounts*`, `/scheduler*`, `/health`,
`/panel/status`, `/realm` are not access-logged (wb_proxy.py:18080-18086).

---

## 9. Caching machinery deliberately skipped

None of these change any number in any payload (the Python code goes to
great lengths to make each path byte-identical to a plain rescan). "Visible"
notes the only client-observable trace.

| Machinery | Where | Client-visible? |
|---|---|---|
| Per-endpoint TTL caches (`_snap_cache`, `_perf_cache`, `_analytics_cache`, `_series_cache`, `_byacct_cache`, `_remaining_cache`, `_count_cache`) | wb_proxy.py:1035, 1820, 3288-3305, 4654-4742, 6411-6560, 3123-3143 | Only as staleness ≤ 15 s (30 s for recent `total`). Port may be fresher. |
| ETag / 304 on cached GETs | wb_proxy.py:17913-17933, 18118-18168 | **Yes**: `ETag`, `Cache-Control: no-cache`, 304 on `If-None-Match`, no ETag on first request after a rebuild. Optional; dashboard works without it. |
| Incremental byte-offset folds + log identity `(size, dev, ino)` + 64-byte tail signature | wb_proxy.py:3367-3437, 3837-3933 | No (port must still survive external truncation/replacement). |
| All-time snapshot / analytics / by-account / series / count states | wb_proxy.py:4222-4360, 6781-6912, 6439-6507, 8217-8258, 4662-5160 | No |
| Pricing-input fingerprints, "used prices" record (T15), `_object_digest` | wb_proxy.py:3439-3746, 8930 | No |
| Realm-input fingerprints (account→realm map) | wb_proxy.py:3748-3835 | No |
| Day buckets (32 d), hour buckets (48 h), analytics hour layer, closed-window edge ledger (`hits`) | wb_proxy.py:7230-8070 | No (float sum order differs by last ulp from a row scan) |
| Series minute ring (6 h) and 50/100 credit buffer | wb_proxy.py:8073-8183 | No |
| Row memos (`_row_day_memo`, `_row_cost_memo`) | wb_proxy.py:3962-4128, 7384-7415 | No |
| Window pre-filter on `{"at": ` prefix (text + bytes) | wb_proxy.py:4762-4860 | No |
| Recent-rows offset index `usage-recent-index.json` (default **off**: `WB_RECENT_INDEX` must be `1`, despite its docstring) | wb_proxy.py:5167-5765 | No |
| Perf incremental ring samples (`_perf_state`, ≤ 4 states) | wb_proxy.py:1280-1818 | No |
| Aggregate checkpoints `usage-aggregate-cache-<kind>.json` (schema 5, legacy v3 single file; write throttle 1 MiB / 900 s) | wb_proxy.py:8686-10102, usage/README.txt | No; files may be ignored/deleted. |
| `estimate-daily.json` archive | wb_proxy.py:2116-2128, 2618-2775 | Indirect: yesterday/day-before budgets come from it. Port may recompute from retained events/usage (needs ≥ 3 days) or keep the file. |
| `_pair_buffer` cumulative sums for remaining | wb_proxy.py:2192-2347 | No |
| wb_pricing memo/stat caches (`_file_key` 1 s TTL, `_entry_memo`, `_details_memo`, `_timeline_ats_cache`, switch caches) | wb_pricing.py:1186-1268, 1602-1642, 1949-1981, 2119-2135, 2232-2280 | Externally written price files may take ≤ 1 s to be noticed. |
| Env knobs for the above: `WB_STATS_TTL`, `WB_REMAINING_TTL`, `WB_COUNT_TTL`, `WB_USAGE_CACHE*`, `WB_USAGE_DAY_BUCKETS`, `WB_USAGE_DAY_KEEP_DAYS`, `WB_USAGE_HOUR_KEEP_*`, `WB_RECENT_INDEX*`, `WB_ROW_MEMO_MAX`, `WB_ROW_COST_MEMO_MAX` | various | No |

---

## 10. Port checklist / gotchas

1. Write `usage.jsonl` rows with the exact keys of 1.2/1.3 (presence rules
   matter: `key` always, `realm` absent on account-less error rows,
   `cost_policy` null not absent, `usage_missing` only when true).
2. Keep `iso` local-time, offset-less; windows use `at` only.
3. Local-timezone math (midnight, Monday, 1st of month, hour start, day keys)
   must follow the host TZ including DST; band math uses **UTC**.
4. Python `round()` is half-to-even (percentile index, averages, budgets);
   `round(x, 6)` of `cost_cny` in recent, `round(..., 1)`/`(..., 2)` elsewhere.
5. JSON output: `ensure_ascii=False`, floats as Python repr (`0.0`, `7.1`,
   `1e-05` style for tiny values), ints stay ints (`credit` may be int `0`
   from the row or float `0.0` from an accumulator).
6. Outcome counting differs per endpoint (table in 1.4, notes in 3.x) —
   copy each rule, don't unify.
7. `policy_id` must be byte-compatible with Python's `json.dumps(sort_keys=True)`.
8. Panel redaction (2.2) applies to every usage endpoint except `/pricing`.
9. `/usage/timeseries` should accept `bucket_seconds` as an alias of `bucket`
   (the dashboard sends it) — or keep the quirk.
10. Rows with invalid UTF-8 stop Python scans; decide whether to keep.
