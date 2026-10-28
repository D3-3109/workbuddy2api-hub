# 07 — Background jobs and non-chat upstream interactions

Baseline: tag `py-baseline-1.7.32`. Every claim cites `file:line` in that tree.
Scope: every background thread / periodic job, every upstream call that is not
a proxied client chat request, the startup/shutdown sequence, and the OAuth
"add account" flow. Target is a Linux VPS (Docker or systemd).

Conventions used below:

- **chat headers** = `Account.headers("chat")` (identity-dependent; built by
  `wb_identity.build_identity_headers`, `wb_accounts.py:1722-1768`). Their exact
  shape belongs to the identity spec; this document only says which purpose a
  call uses.
- **billing headers** = `Account.headers("billing")` (`wb_accounts.py:1688-1720`):
  `Content-Type: application/json`, `Accept: application/json, text/plain, */*`,
  `X-Requested-With: XMLHttpRequest`, `User-Agent: <realm billing_ua>`,
  `Origin/Referer: <realm origin>`, `Authorization: Bearer <accessToken>`,
  `X-User-Id`, `X-Domain`, `X-CodeBuddy-Request: 1`, `Accept-Language`
  (`en-US` intl / `zh-CN` cn), `X-Request-ID`, per-account derived
  `X-Machine-ID` and `X-Session-ID`; plus `X-API-Key` for static keys,
  `X-Enterprise-Id`+`X-Tenant-Id` or `X-No-Enterprise-Id: 1`, `X-Product: SaaS`
  on cn, optional `X-Device-Token` (not for CLI identity).
- **realm config** (`wb_accounts.py:267-298`):

| realm | chat_upstream | billing_upstream | origin | chat_ua | billing_ua |
|---|---|---|---|---|---|
| intl | `https://www.workbuddy.ai` | `https://www.workbuddy.ai` | same | `WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0` | `WorkBuddy/5.8.4` |
| cn | `CN_CHAT_BASE` (default `https://copilot.tencent.com`, env `WB_CN_CHAT_UPSTREAM`) | `https://www.codebuddy.cn` | `https://www.codebuddy.cn` | `WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0` | `WorkBuddy/5.8.3` |

  UA values: `wb_identity.py:70-81`; CN base: `wb_identity.py:265-289`.
- **http_json** (`wb_accounts.py:236-265`): JSON request helper with
  `retries=3` default, sleep `backoff*attempt` (1 s, 2 s) between attempts,
  retrying only `_retryable` errors (`wb_accounts.py:135-147`: TLS resets,
  `URLError`, timeouts, `OSError`, HTTP >= 500; never certificate-verify
  failures, which raise `TLSVerifyError` with a hint).
- **urlopen** (`wb_accounts.py:210-233`): per-account outbound proxy and a
  keep-alive connection pool (`wb_upstream_pool`, on unless
  `WB_UPSTREAM_KEEPALIVE=0`; idle TTL `WB_UPSTREAM_POOL_IDLE` default 90 s,
  `wb_upstream_pool.py:79-87`; idle sweeping happens inline on get/put,
  `wb_upstream_pool.py:400-420`, so there is no pool janitor thread).
- **Time zone**: every schedule uses `time.localtime()` (host local time). The
  Docker image sets `TZ=Asia/Shanghai` (`Dockerfile:5,10`,
  `docker-compose.yml:23`) and the defaults are documented as CST
  (`wb_scheduler.py:86`, `wb_settings.py:1095`). Rust: resolve the zone from
  `TZ` (default the host zone; ship `Asia/Shanghai` in the container) and use
  it for all wall-clock schedules and all "today" checks.

---

## 1. Inventory

| # | Job | Kind | Started by | Section |
|---|---|---|---|---|
| J1 | Scheduler main loop | daemon thread, 30 s tick | `_bootstrap_runtime` `wb_proxy.py:22812-22814` | §3 |
| J2 | Token keepalive | step of J1 at 22:00 | `wb_scheduler.py:399-412` | §3.4 |
| J3 | CN check-in + activity report | step of J1 at 09:00/21:00 | `wb_scheduler.py:416-435` | §4.1-4.2 |
| J4 | CN cat travel + streak bonus | step of J1 at 09:00/21:00 | `wb_scheduler.py:437-450` | §4.3-4.4 |
| J5 | CN night task (black_cat) | step of J1 at 01:00 | `wb_scheduler.py:452-458` | §4.5 |
| J6 | Intl daily activity (desktop leg + web session) | step of J1 (checkin hours or own HH:MM slots) | `wb_scheduler.py:461-466` | §5 |
| J7 | Window probe | thread spawned by J1 each tick | `wb_scheduler.py:195-211` | §6 |
| J8 | Quota scan | thread spawned by J1 each tick | `wb_scheduler.py:213-229` | §7 |
| J9 | Credits refresher | daemon thread `credits-refresher`, 1800 s tick | `wb_proxy.py:22833-22835` | §8.1 |
| J10 | Catalog daily fallback | daemon thread `catalog-daily`, hourly | `wb_proxy.py:22817`, `11382-11395` | §9.2 |
| J11 | Price refresher (OpenRouter) | daemon thread `price-refresher` | `wb_proxy.py:22827-22829` | §9.4 |
| J12 | models.dev index refresh | one-shot thread per `/v1/models`, 300 s throttle | `wb_proxy.py:18808-18813` | §9.3 |
| J13 | Log store writer | daemon thread `logstore` (only when enabled) | `wb_proxy.py:22781-22782` | §10.1 |
| J14 | SSE heartbeat | per-streaming-request daemon thread | `wb_proxy.py:15354-15409` | §10.4 |
| J15 | ACP SSE reader | per-web-session daemon thread | `wb_webagent.py:132-133` | §5.4 |
| J16 | Update checker (GitHub) | daemon thread | `wb_proxy.py:22840-22845` | dropped, §15 |

Manual-only (no timer): full CN growth tasks (`/tasks/run`), task claim,
credits sweep, catalog refresh, account test, profile sync, proxy exit probe,
output-clamp probe script. All are in §8-§11.

Note: the full growth-task automation (`wb_tasks.run_growth_tasks`, accept ->
report events -> claim) is **not** scheduled. The scheduler only runs check-in,
activity report, cat travel, streak bonus and the night task
(`wb_scheduler.py:415-458`); `run_growth_tasks` is reachable only from
`POST /tasks/run` (`wb_proxy.py:20725-20763`).

---

## 2. Startup sequence

`main()` (`wb_proxy.py:22651-22667`):

1. `_parse_cli_args` (`22669-22697`): `--host` (env `HOST`, default
   `127.0.0.1`), `--port` (env `PORT`, default 8788), `--lan`, `--api-key`
   (env `API_KEY`/`WB_PROXY_KEY`), `--usage-dir`, `--accounts-dir`
   (env `ACCOUNTS_DIR`), `--panel-password`, `--probe-only`, plus
   `--info`/`--import-desktop` (desktop import, dropped).
2. `_apply_cli_overrides` (`22699-22723`): `--lan` forces host `0.0.0.0`;
   `--usage-dir` re-points `USAGE_LOG`, `limit-events.jsonl`,
   `estimate-daily.json`, `usage-recent-index.json`; then
   `wb_activity.set_data_dir(USAGE_DIR)` **before any background thread
   exists**, so the scheduler's first cycle writes history into the right dir.
3. `--probe-only` exits 0 (instance running) / 3 (port free) (`22654-22660`).
4. `_probe_running_instance` (`22725-22768`): `GET http://<host|127.0.0.1>:<port>/health`
   timeout 2 s. A JSON body containing `accounts` and no `service` key means
   "our gateway is already up" -> print and return without starting. Any other
   JSON answer means a foreign service owns the port -> exit 1.
5. `_bootstrap_runtime` (`22770-22846`), in this exact order:
   1. `LOG_STORE = LogStore(USAGE_DIR/logs.sqlite3)`; `apply_log_db_settings()`
      (`10625-10651`) starts thread J13 if `log_db_enabled`, and lifts the log
      id counter to `MAX(id)` in the DB.
   2. API key resolution: `--lan` without key -> `ensure_launcher_key` (generated
      and persisted in `settings.json`); a panel-saved key overrides unless
      `--api-key` was given (`22786-22795`).
   3. `--panel-password` persists, else warn if still default `admin`.
   4. `POOL = AccountPool(ACCOUNTS_DIR)`; `POOL.load()` (`wb_accounts.py:3708-3728`):
      reads every `*.json` except `NON_ACCOUNT_FILES`
      (`settings.json, active_realm.json, model_windows.json, quota_scan.json`,
      `wb_accounts.py:3676`), skips files without `uid`, applies reserve
      credits, expiring window, and re-arms persisted 6004 model caps.
   5. `apply_proxy_slots()`, `apply_reserve_credits()`,
      `resize_chat_slots(POOL.count_ready())`, `apply_daily_guards()`,
      `load_persisted_realm()` (`accounts/active_realm.json`, `6018-6031`).
   6. `SCHEDULER = Scheduler(POOL); SCHEDULER.start()` -> J1.
   7. `start_catalog_daily()` -> J10.
   8. `wb_pricing.set_data_dir(USAGE_DIR)`, `set_settings_dir(ACCOUNTS_DIR)`,
      `PRICING = PriceRefresher(pricing_refresh_minutes)`; `PRICING.start()` -> J11.
   9. `CREDITS_REFRESHER = CreditsRefresher(POOL)`; `.start()` -> J9.
   10. `UPDATES = UpdateChecker(...)`; `.start()` -> J16 (dropped).
6. `_report_first_run` (`22848-22877`): with zero accounts it scans for desktop
   credentials (dropped on Linux) and still continues so the dashboard can add
   accounts via OAuth.
7. `_log_startup_summary` (`22879-22952`): log-only.
8. `_serve_forever` (`22965-23005`): binds `GatewayServer` (a
   `ThreadingHTTPServer` with `request_queue_size = 128`, `22954-22963`) only
   after all threads are started. Bind failure -> exit 1.

Import-time side effect: `wb_identity.configure_versions(wb_settings.identity_versions(ACCOUNTS_DIR))`
runs when `wb_proxy` is imported (`wb_proxy.py:6000-6005`), i.e. before `main`
and before `--accounts-dir` is applied (it reads the default dir).

**Prewarm**: there is none. No catalog, credits or models.dev fetch happens
synchronously at startup. First upstream contacts are:

| Delay after start | Who | What |
|---|---|---|
| 0 s | J11 | OpenRouter fetch only if no pricing timeline exists yet (`wb_pricing.py:2560-2568`); that fetch also calls `curated_live_sources` for both realms -> `/v3/config` |
| ~0 s | J7 | window probes for any persisted window already due (`wb_scheduler.py:159`, after the first cycle) |
| 10 s | J1 | first cycle "启动初次初始化巡检" with `force=False` (`wb_scheduler.py:152-156`) |
| 60 s | J10 | first catalog daily pass (`wb_proxy.py:11384`) |
| 1800 s | J9 | first credits refresh (the loop waits the interval **before** its first refresh, `wb_accounts.py:3644-3654`) |
| lazy | J12 / catalog | first `GET /v1/models` triggers catalog fetch (300 s cache) and models.dev refresh |

Important: the startup cycle runs with `force=False`, so it only executes the
steps whose configured hour equals the **current hour** (`wb_scheduler.py:359-370`).
Started at 14:20 with defaults, it logs "开始执行任务" and "巡检完成：0 ..." and
does nothing upstream. Started inside 09:xx it runs the 09:00 work. Port this
behavior as is (it is what bounds restart storms), but document it.

Rust port: same order. Load settings and accounts, create the shared state,
spawn tasks (scheduler, credits, catalog-daily, pricing, logstore), then bind.
A `tokio::sync::watch`/`CancellationToken` shared by all jobs replaces the
per-thread stop events.

---

## 3. Scheduler (J1) — `wb_scheduler.py`

### 3.1 State

`Scheduler.__init__` (`wb_scheduler.py:84-115`):

- hour grids (defaults, CST): `checkin_hours=[9,21]`, `travel_hours=[9,21]`,
  `keepalive_hours=[22]`, `cat_hours=[1]`; `daily_chat_times=[]`
  (`88-93`). Overridden every tick from `settings.json` (§3.6).
- `all_hours` = sorted union of the four grids (`97`, `302-303`).
- `enabled=True` — **in memory only**; `/scheduler/toggle` flips it and a
  restart resets it to `True` (`wb_proxy.py:20937-20942`).
- `_daily_chat_done: {date: set(HH:MM)}` — same-day dedup for minute slots,
  lost on restart (`94-96`).
- `logs`: ring of 60 lines (`117-123`), mirrored into the gateway log with
  tag `tasks` when the line contains 签到/打卡/活跃/猫猫/连登/夜猫/抽奖/兑换,
  else tag `scheduler`, prefixed `[调度器]` (`124-136`).
- `_run_lock` (non-blocking) prevents overlapping cycles (`106-108`, `335-343`).
- `wb_tasks.set_logger(self.log)` so task-level failures appear in the same log
  (`115`).

### 3.2 Main loop

`_run_loop` (`wb_scheduler.py:150-193`):

```
sleep 10 s
_execute_cycle("启动初次初始化巡检")            # force=False
_run_window_probes()                             # J7 thread
loop until stop_event:
    _reload_schedule(); _calc_next_fire()
    now = localtime()
    if enabled:
        slot = _due_daily_chat_slot(now)         # minute-level HH:MM slot
        if slot: _execute_cycle("网页活跃定时命中 (HH:MM)", daily_chat_only=True); sleep 35 s
        if now.min == 0 and now.hour in all_hours:
            _execute_cycle("整点排程命中 (H:00)"); sleep 65 s
    _run_window_probes()                         # not gated by enabled
    _run_quota_scans()                           # not gated by enabled
    stop_event.wait(_next_wait())                # <= 30 s
```

- **Tick**: `_next_wait()` = `min(30, max(1, seconds_until_due(window)))`
  (`231-246`): wakes early when a persisted 429 window is about to reset.
- **Hour match**: fires when `tm_min == 0` and the hour is in `all_hours`; the
  65 s sleep after a cycle prevents a second fire in the same minute (`180-186`).
  Because the tick is <= 30 s, every :00 minute is observed at least once.
  There is no catch-up: if the process is down (or a previous cycle is still
  holding `_run_lock`) during HH:00, that slot is skipped ("跳过本次巡检 ...：上一轮仍在执行", `337-339`).
- **Minute slots** (`_due_daily_chat_slot`, `248-271`): `HH:MM` strings from
  `settings.schedule.daily_chat`; each slot fires at most once per local day
  (in-memory set); the 35 s sleep after it avoids double fire.
- **Jitter**: none on the trigger time itself. Per-account staggering inside a
  cycle: `random.uniform(2.0, 5.0)` s between accounts (`372-375`, "issue #394"),
  plus fixed 1.0 s after each CN sub-step.
- `enabled=False` suppresses J2-J6 but **not** J7/J8 (`187-192`).
- `next_run_time` (`308-325`): next hour in `all_hours` (today, or the first one
  tomorrow), as `YYYY-MM-DD HH:00:00`; display only.

### 3.3 Cycle body `_run_cycle` (`wb_scheduler.py:345-468`)

Flags for this cycle (`359-370`), with `h = localtime().tm_hour` at cycle start:

| step | runs when |
|---|---|
| keepalive | `not daily_chat_only and (force or h in keepalive_hours)` |
| checkin + activity report | `not daily_chat_only and (force or h in checkin_hours)` |
| travel + streak bonus | `not daily_chat_only and (force or h in travel_hours)` |
| night (cat) | `not daily_chat_only and (force or h in cat_hours)` |
| intl daily chat | `force or daily_chat_only or (daily_chat_times == [] and run_checkin)` |

`force=True` only from `trigger_now()` (manual, §3.7).

Per account, in pool order (`372-463`), after the 2-5 s stagger:

1. **Skip restricted**: `acc.restricted_until > now` (persisted 403/11140
   cooldown, 2 h default, `wb_accounts.py:3152-3176`) -> log and `continue`
   (skips keepalive too).
2. **Skip realm hold**: `wb_proxy.restriction_hold_remaining(acc.realm) > 0`
   -> `continue` (`wb_proxy.py:14108-14113`; hold is set when
   `WB_RESTRICTION_WAVE_MIN` (2) distinct accounts are restricted within
   `WB_RESTRICTION_WAVE_WINDOW` (900 s), lasting `WB_RESTRICTION_HOLD_SECONDS`
   (1800 s), in memory, `wb_proxy.py:14083-14140`).
3. `task_active = acc.tasks_enabled` (`wb_accounts.py:1878-1883`); persisted
   `tasksEnabled`, defaulting to `enabled` for legacy files (`wb_accounts.py:816-819`).
   Keepalive ignores it; all reward tasks require it.
4. keepalive (§3.4) — any realm, any `task_active`.
5. if `realm == "cn" and task_active`: checkin (if `can_checkin()`), sleep 1;
   activity report (if `can_report_activity()`), sleep 1 — both only in
   checkin hours; travel then streak bonus, sleep 1 each — travel hours;
   night growth, sleep 1 — cat hours.
6. if `realm == "intl" and task_active and run_daily_chat and can_daily_chat()`:
   collected into `daily_chat_targets` (run after the loop, §5.1).

Note: `enabled` (API dispatch switch) is not checked anywhere in the cycle;
disabled accounts still run tasks if `tasksEnabled` (issue #521, comment
`392-395`).

Final log: `巡检完成：Token保活 N 个，国内签到 N 个，活跃上报 N 个，猫猫日常 N 个，国际活跃 N 个` (`468`).

### 3.4 Token keepalive (J2)

- Due check `keepalive_due(expires_at)` (`wb_scheduler.py:63-80`): due when
  `expires_at <= 0` (unknown) or `expires_at - now < lead`.
- Lead: env `WB_KEEPALIVE_LEAD_HOURS`, default 48 h, valid range [1, 720] h,
  invalid -> default (`44-60`). Must exceed the 24 h keepalive interval.
- Action `Account.refresh()` (`wb_accounts.py:1814-1876`), serialized by a
  per-account lock (refresh tokens rotate):
  - static API key -> return False, no state change.
  - no refresh token -> `token_expired=True`, error "no refresh token; sign in again".
  - `POST <chat_upstream>/v2/plugin/auth/token/refresh`, body `{}`, timeout 30,
    via `http_json` (3 attempts). Headers: `Content-Type`, `Accept`,
    `X-Requested-With`, `User-Agent: billing_ua`, `Origin`, `Referer`,
    `X-Refresh-Token: <refreshToken>`, `X-Auth-Refresh-Source: workbuddy` (cn) /
    `plugin` (intl), `X-User-Id`, `X-Domain`, `X-CodeBuddy-Request: 1`,
    `Accept-Language`, optional `X-Enterprise-Id`. No `Authorization` header.
  - Response: `data.data.accessToken` or `data.accessToken`; `refreshToken`
    replaces the old one when present; `expiresAt = jwt.exp`.
  - Failure: exception text containing `401`/`Authorization`/`Unauthorized`
    -> `token_expired=True`, "登录已过期 (HTTP 401)，请重新登录"; other
    exceptions -> "refresh failed: ..."; missing token -> `token_expired=True`.
    No scheduled retry until the next 22:00.
  - Success clears `last_error` and `cooldown_until` and saves the account file.
- Other places that call `refresh()`: window probe / quota scan when the token
  expires in < 120 s (`wb_window_probe.py:289-294`, `wb_quota_scan.py:292-297`);
  credits fetch on a 401 (`wb_accounts.py:2642-2657`, `2802-2816`);
  `POST /accounts/refresh` (§11).

### 3.5 Daily gates and persisted markers

Stored in `accounts/<uid>.json` (camelCase keys, `wb_accounts.py:1003-1050`),
written atomically via `<path>.<pid>.<tid>.tmp` + `os.replace`, with
`lastError`/`cooldownUntil` stripped (`wb_accounts.py:1206-1236`, `4835`):

| marker | format | set when | gate |
|---|---|---|---|
| `lastCheckin` | `YYYY-MM-DD HH:MM:SS` local | checkin state `checked`/`already` | `can_checkin`: cn, not enterprise/api-key, date prefix != today (`1885-1894`) |
| `lastActivityReport` | same | `/v2/report` returned code 0 | `can_report_activity` (`1904-1913`) |
| `lastDailyChat` | same | desktop leg OK **and** (web leg disabled or web leg OK) | `can_daily_chat`: intl, date != today (`1896-1902`) |
| `restrictedUntil` | epoch float | 403/11140 | scheduler skip |

"today" = `time.strftime("%Y-%m-%d")`, local zone. The Rust port must use the
same zone for writing and comparing.

### 3.6 Schedule settings (`wb_settings.py:1095-1240`)

`accounts/settings.json` key `schedule`:

```json
{"schedule": {"checkin": [9,21], "travel": [9,21], "keepalive": [22], "cat": [1],
              "daily_chat": ["08:30", "20:15"]}}
```

- Hours: list or comma string, ints 0-23, deduped and sorted; missing, empty or
  invalid per key -> default for that key (`1126-1140`).
- `daily_chat`: list/comma/whitespace string of `HH:MM` (full-width `：`
  accepted), max 24 entries; `[]`/unset = follow checkin hours (`1176-1215`).
- `settings.json` reads are cached on `(path, mtime, size)`
  (`wb_settings.py:254-282`), so the scheduler re-reads every tick cheaply and
  edits take effect without restart.
- Written via `POST /settings/save` `{"schedule": {...}}`
  (`wb_proxy.py:19669-19688`); invalid non-empty input -> 400. The payload
  also accepts `restart_scheduler: true` -> `stop(); start()`
  (`wb_proxy.py:20108-20112`).

### 3.7 Manual control and inspection

| Endpoint | Auth | Behavior |
|---|---|---|
| `GET /scheduler` | panel token (route is panel-only, `wb_proxy.py:18561-18591`) | `Scheduler.status()` (`wb_scheduler.py:534-576`): `enabled, mode, mode_cn, mode_intl, last_run_time, next_run_time, windows` (first 10 of window snapshot), `quota_scans` (6), `logs` (last 20) |
| `POST /scheduler/trigger` | panel | `trigger_now()`: if `_run_lock` held -> `{"ok":false,"msg":"已有巡检正在执行，请稍候再试"}`; else thread `_execute_cycle("手动立即触发", force=True)` -> all steps for all accounts (`327-333`, `wb_proxy.py:20909-20912`) |
| `POST /scheduler/toggle` | panel | flips in-memory `enabled`; returns status (`wb_proxy.py:20937-20942`) |

All `/accounts*`, `/tasks*`, `/scheduler*`, `/activity*`, `/logs*`,
`/settings*`, `/pricing*` routes need the panel session (`X-Panel-Token`
header) (`wb_proxy.py:18561-18591`, `22145-22146`).

---

## 4. CN reward tasks — `wb_tasks.py`, `wb_accounts.py`

Bases: `CHAT_BASE = wb_identity.CN_CHAT_BASE` (default `https://copilot.tencent.com`),
`BILL_BASE = https://www.codebuddy.cn`, `WEB_BASE = https://www.workbuddy.cn`
(`wb_tasks.py:18-25`). All task requests use `account.proxy`. Enterprise
accounts are skipped everywhere (`is_enterprise()` = `enterpriseId` set or JWT
carries an enterprise id, `wb_accounts.py:2878-2885`).

Response envelope everywhere: `{"code": 0, "msg": "...", "data": {...}}`; only
`code == 0` is success.

### 4.1 Daily check-in

`Account.checkin(trigger)` (`wb_accounts.py:2453-2506`):

- Rejects non-cn, API key, enterprise.
- `POST https://www.codebuddy.cn/v2/billing/meter/daily-checkin`, body `{}`,
  billing headers, timeout 15, `http_json` (3 attempts).
- Classification `checkin_state(code, msg)` (`wb_accounts.py:358-384`):
  - msg contains 处理中 / 请勿重复操作 / 系统繁忙 / 稍后再试 -> `pending` (fail)
  - `code == 0`: msg contains 失败 -> `failed`, else `checked`
  - `code == 10001`: msg contains 已签到 -> `already`, else `failed`
  - anything else -> `failed`
- `checked`/`already` -> write `lastCheckin`, save. An HTTP error body is also
  parsed: a 400 with 已签到 counts as `already`.
- Every call appends one activity-history row (§10.2) with the trigger tag.
- Failure leaves `lastCheckin` untouched, so the 21:00 run retries.
- Also auto-run after an OAuth add for cn (`trigger="account_add"`,
  `wb_accounts.py:4596-4598`).

### 4.2 CN activity report (streak)

`Account.report_activity()` (`wb_accounts.py:2508-2537`):

- `wb_tasks.build_event(account, "chat")` (`wb_tasks.py:387-395`):

```json
[{"eventCode":"chat_request_send","timestamp":<ms>,"reportDelay":0,"mode":"craft",
  "conversationId":"wb-task-<ms>-0","requestId":"wb-task-<ms>-0-req","inputLength":12,
  "requestModelId":"deepseek-v4-flash","requestModelName":"DeepSeek V4 Flash",
  "isPlan":false,"agentName":"default","agentType":"conversation","userId":"<uid>"}]
```

- `POST <CHAT_BASE>/v2/report`, body = JSON array of events, chat headers,
  timeout 15; success iff `code == 0` (`wb_tasks.py:399-417`). No retry.
- Then `GET <CHAT_BASE>/activity/growth/streak` to read `data.streak.days`
  (`wb_tasks.py:198-212`), message only.

### 4.3 Cat travel ("猫猫旅行") — `do_cat_travel` (`wb_tasks.py:472-531`)

1. `GET <CHAT_BASE>/activity/growth/buddy/travel/status` (chat headers, 10 s)
   -> `data.state`.
2. `arrived`: `POST .../buddy/travel/claim` body `{}` -> `data.reward_credit`;
   then `fetch_credits()`. Result action `claim`.
3. `idle`: if `data.daily_limit_reached` -> action `idle` ("明日 00:00 刷新").
   Else `GET .../buddy/travel/config` -> `data.locations[0].id` (fallback 1);
   `POST .../buddy/travel/depart` body `{"location_id": <id>}`
   (empty body is rejected 400). If the error contains `no active buddy`:
   `ensure_buddy()` (complete and claim the `first_buddy` auto task,
   `wb_tasks.py:428-443`) then retry depart once. Action `depart`.
4. `traveling` -> action `traveling`; other states echoed.

Scheduler counts `claim`/`depart` only. The default 09:00 / 21:00 grid is
"depart in the morning, claim in the evening".

### 4.4 Streak bonus ("连登管家") — `run_streak_bonus` (`wb_tasks.py:791-899`)

Runs in travel hours after cat travel. All chat headers unless noted,
timeouts 10 s, errors swallowed and logged via `_log`:

1. Makeup: `GET /activity/growth/heatmap` -> `data.cells[]`; if yesterday's
   cell (`date == local yesterday`) has `score == 0`, `GET /activity/growth/streak`
   -> `data.makeup_cards.balance`; if > 0 `POST /activity/growth/makeup-cards/use`
   `{"target_date": "YYYY-MM-DD"}`.
2. Gifts (billing headers, `BILL_BASE`): `POST /billing/meter/claim-gift` and
   `POST /billing/meter/claim-compensation`, body `{}`; `code 0` -> log
   `data.credit`.
3. Tier redeem: `GET /activity/growth/streak` -> `data.redemption_status`
   (`tiers[]`, `tier_7d_status`, `tier_14d_status`, `tier_28d_status`); for every
   tier whose status is not `locked`/`claimed`: `POST /activity/growth/redeem`
   `{"tier": "7d", "client_token": "<uuid4>"}`.
4. Lottery: `GET /activity/growth/lottery/summary` -> `data.chances`; for each
   chance `POST /activity/growth/lottery/draw` `{"client_token": "<uuid4>"}`,
   sleep 1.0 s between draws, stop on the first exception.

### 4.5 Night task (black_cat) — `run_night_growth` (`wb_tasks.py:732-789`)

- Only counts when local hour >= 23 or < 8 (`in_night_window`, `88-90`); default
  run at 01:00.
- `GET <CHAT_BASE>/v2/activity/growth/tasks` (`fetch_growth_tasks`,
  `wb_tasks.py:116-147`) -> find `task_code == "black_cat"` (target 3).
- `claimed` -> done. `not_accepted` -> `POST /v2/activity/growth/tasks/accept`
  `{"task_codes":["black_cat"]}`; failure -> skip tonight.
- `current >= target` -> claim (§4.6). Else report one `chat_request_send` event
  with `mode:"night"`, model `glm-5.2`/`GLM-5.2` (`build_event(..., "cat")`), then
  poll the task list 4 times with 4 s sleeps until `current` increases; claim if
  it reached the target. `fetch_credits()` at the end.

### 4.6 Growth task automation (manual only) — `run_growth_tasks` (`wb_tasks.py:651-729`)

Triggered by `POST /tasks/run {uid?: "<uid>"|"all", claim?: bool}`
(`wb_proxy.py:20725-20763`): sequential over `POOL.task_accounts("cn")`, 1.5 s
between accounts, `gap=1.0`.

1. `GET /v2/activity/growth/tasks` -> `data.tasks[]` with `task_code, title,
   description, jump_url, accept_status, task_type, progress{current,target},
   reward_credit, reward_energy`.
2. Auto-type tasks (e.g. `first_buddy`) first via `_progress_one`.
3. Accept every `not_accepted`, non-auto, forgeable task:
   `POST /v2/activity/growth/tasks/accept` `{"task_codes":[...]}` in chunks of 20;
   per-item `data.results[].status` in `accepted|already_accepted` is success
   (`wb_tasks.py:214-275`). Re-fetch, retry still-unaccepted once.
4. `_progress_one` per task (`wb_tasks.py:534-648`):
   - unknown/unforgeable spec -> skip (`TASK_SPECS`, `wb_tasks.py:41-70`;
     `wb_wechat_oa_subscribe_task`, `Expert_Philanthropy` unforgeable).
   - `claimed` -> 0. `completed` or `current >= target` -> claim (or leave for
     manual when `claim=false`).
   - Desktop-only tasks (`RichMeow_Chat, Library_read, Buddy_App, Buddy_App_QQ`,
     `wb_tasks.py:78-83`) -> skip with the deep link.
   - Night task outside 23-08 -> skip.
   - Not accepted (and not auto) -> skip.
   - Report `need = target - current` events, one per `POST /v2/report`, `gap`
     seconds apart; expert/team events rotate through distinct ids
     (`EXPERT_ID_POOL`, `TEAM_ID_POOL`, `wb_tasks.py:95-113`) because upstream
     dedups on `(eventCode, id)`. Event shapes per kind: `build_event`
     (`wb_tasks.py:330-396`) — canvas, template, expert/team/lighthouse
     (`expert_actual_use`), skill (`skill_info`), automation
     (`automated_task_create_suc`), playbook (`playbook_prompt_send`), skin
     (`appearance_skin_apply`), chat/glmchat/cat (`chat_request_send`).
   - Sleep 1.5 s, then poll the task list up to 6 times (1.5 s sleeps for the
     first 2, 2.5 s while progress grows) until target reached; else "领奖顺延".
   - Claim.
5. `do_cat_travel`, then `fetch_credits()`.

Claim `claim_task` (`wb_tasks.py:278-327`):
`POST <CHAT_BASE>/activity/growth/tasks/<code>/claim` body `{}` -> `data.credit`,
`data.energy`. On HTTP 400 fall back to
`POST https://www.workbuddy.cn/activity/growth/tasks/<code>/claim` with web
headers: `Authorization: Bearer`, `Accept`, `Content-Type`,
`Origin: https://www.workbuddy.cn`, `Referer: .../profile/growth-center`,
`x-client-platform: web`, `User-Agent: Chrome/152 UA` (`wb_tasks.py:25`),
`X-User-Id`, `X-Domain: https://www.workbuddy.cn`.

Other manual task endpoints (all panel-auth, cn accounts only):

| Endpoint | Body | Calls | Pacing |
|---|---|---|---|
| `GET /tasks?uid=` | — | `growth_snapshot`: `fetch_growth_tasks` + `fetch_growth_summary` concurrently (2 workers) (`wb_proxy.py:6281-6308`, `19096-19117`) | per-uid cache TTL `WB_TASKS_CACHE_TTL` (20 s; 0 disables) |
| `POST /tasks/claim` | `{uid, code}` | `claim_task` | — |
| `POST /tasks/travel` | `{uid?}` | `do_cat_travel` per account | 1.0 s between |
| `POST /tasks/streak-bonus` | `{uid?}` | `run_streak_bonus` per account | 1.0 s between |
| `POST /accounts/checkin` | `{uid?}` | `checkin(trigger="manual")`; without uid: cn, task_active, not enterprise | none |

`fetch_growth_summary` (`wb_tasks.py:150-195`): `GET /v2/activity/growth/energy`
(`data.balance`), `GET /activity/growth/streak`, `GET /activity/growth/lottery/summary`,
`GET /activity/growth/buddy/travel/status`, each 10 s, errors logged. Any
state-changing route calls `invalidate_tasks_cache()` (`wb_proxy.py:6276-6279`).

---

## 5. Intl daily activity (J6) — "real web session" chat

Purpose: the intl "每日活跃" reward (30/50 credits) only counts a **completed
web-console agent session**. A desktop-identity `chat/completions` call does not
count (comments `wb_accounts.py:25-41`, issues #75/#59/#90).

### 5.1 Batch orchestration (`wb_scheduler.py:470-532`)

- Concurrency: `settings.daily_chat_web_concurrency`, int 1-10, default 1
  (`wb_settings.py:2481-2498`).
- `concurrency <= 1` or one target: sequential, `sleep 1.5 s` after each account.
- Otherwise `ThreadPoolExecutor(max_workers=concurrency)`; worker `i` sleeps
  `min(i * 0.3, 2.0)` s before starting.
- Each worker calls `acc.daily_chat(trigger="scheduler")`; exceptions are logged
  and counted as failure.

### 5.2 `Account.daily_chat` (`wb_accounts.py:2351-2451`)

1. Intl only. If `can_daily_chat()` is false, return `{"ok":true,"skipped":true}`.
2. **Desktop leg**: `POST <chat_base_url>/v2/chat/completions` with
   `account.headers("chat", model=model)`. Body before `build_upstream_body`:
   `{"model": <daily model>, "messages":[{"role":"user","content":"Hi"}],
   "stream": true, "max_tokens": 10, "reasoning_effort": "none"}`. For the CLI
   identity: drop `CLI_BODY_DROP_FIELDS`, add `cache_retention`, gzip the body and
   set `Content-Encoding`. Header names are sent verbatim
   (`req.wb_exact_headers`). Timeout 20 s. The body is read and discarded, then
   `fetch_credits()` runs (errors ignored).
3. **Web leg** runs when `web` is true (default `settings.daily_chat_web`,
   default **true**, `wb_settings.py:2151-2162`): `daily_chat_web(trigger=None)` (§5.3).
4. With the web leg enabled, `ok` follows the web leg. `lastDailyChat` is written
   only when the web leg succeeded or is disabled (`2436-2442`).
5. An HTTP error on the desktop leg returns
   `{"ok":false,"error":"HTTP <code>: <first 100 chars>"}`; no web leg, no marker.
6. One activity-history row per call (`task=daily_chat`).

Daily model (`daily_chat_model`, `wb_accounts.py:1927-1953`): candidates
`[settings.daily_chat_model?, "deepseek-v4.1-flash", "hy3", "hy4-preview-f",
"hy4-preview", "glm-5.3-flash" (intl only)]`. The first candidate without an
active model cooldown or persisted 6004 cap wins, else the first candidate.

### 5.3 Web leg retry and fallback policy (`wb_accounts.py:2009-2128`)

Settings (`wb_settings.py:2200-2500`; env defaults `wb_accounts.py:55-67`):

| key | type | default | range |
|---|---|---|---|
| `daily_chat_web` | bool | true | |
| `daily_chat_web_retry` | bool | true | off = 1 attempt |
| `daily_chat_web_attempts` | int | 3 (`WEB_TURN_ATTEMPTS`) | 1-5 |
| `daily_chat_web_timeout` | int s | env `WB_WEB_TURN_TIMEOUT` or 120 | 1-3600 |
| `daily_chat_web_retry_backoff` | int s | env `WB_WEB_RETRY_BACKOFF` or 5 | 0-60 |
| env `WB_WEB_RETRY_BACKOFF_MAX` | float | 30 | |
| `daily_chat_web_fallback` | bool | false | |
| `daily_chat_web_fallback_models` | list | [] | <= 20 ids, <= 128 chars |
| `daily_chat_web_random_prompt` | bool | false | |
| `daily_chat_web_prompts` | list | [] (= builtin pool) | <= 500 lines, <= 200 chars |
| `daily_chat_web_concurrency` | int | 1 | 1-10 |
| `daily_chat_model` | str | "" | |

`_daily_chat_web_upstream`:

- `base, total = (timeout, attempts if retry else 1)`.
- **Fallback mode** (`daily_chat_web_fallback` true): `models =
  web_fallback_models()` (the configured list, or `[daily model] + realm free
  list`; deduped, sorted by the bundled catalogue multiplier with unknown ids
  last and the preferred model first within a tie, `wb_accounts.py:707-728`,
  `1955-1987`). `attempts = max(total, len(models))`; attempt `i` uses
  `models[i % len]` and waits `base * 2**i`.
- **Normal mode**: attempt `i` waits `base * (i+1)` (120/240/360 s by default).
- Stops on success or on a non-retryable result.
- Between attempts, `_web_retry_pause` sleeps only when the failed phase was
  `create` or `session` (no wait budget spent): `min(b * 2**i, max(cap, b))`
  with `b = retry_backoff`, `cap = 30` (`wb_accounts.py:70-81`, `2084-2098`).

Prompt (`daily_chat_web_prompt`, `1989-2008`): `"Hi"` unless random prompts are
on; then `random.choice(settings.daily_chat_web_prompts or builtin pool)`. The
builtin pool is 300 short Chinese questions (`wb_webprompts.py:10-316`); port
the list verbatim.

### 5.4 One web attempt (`_daily_chat_web_once`, `wb_accounts.py:2130-2212`)

Web headers (`web_headers`, `1915-1925`): `Authorization: Bearer <accessToken>`,
`X-User-Id`, `Content-Type: application/json`,
`Accept: application/json, text/plain, */*`, `Origin: https://www.workbuddy.ai`,
`Referer: https://www.workbuddy.ai/app`, `User-Agent:` the Edge 140 string
(`46-47`). No desktop fingerprint headers.

1. **Create**: `POST https://www.workbuddy.ai/console/as/conversations/`,
   timeout 30, body
   `{"prompt": <prompt>, "model": <model>, "conversationOrigin": "workbuddy-app",
   "plugins": [{"name":"weixinpay","marketplace":"codebuddy-builtin"}]}` returns
   `data.id`. Failure classes (`2318-2349`): HTTP error is retryable iff status
   >= 500 or 408/429; `limit` iff 429 and the body contains `10105` or "maximum
   number of active conversations"; network error is retryable; a `code` other
   than 0/None is not retryable.
2. **Limit recovery**: on `limit`, run `web_conversation_prune()` and create once
   more. If it still fails, return a non-retryable error that names the
   10-session cap. Prune (`2290-2316`): `GET .../conversations/?page=1&pageSize=50`
   returns `data.conversations[]`; active means status `working|creating|pending`.
   With 10 or more active, sort by `createdAt` and `POST .../<id>/stop` the oldest
   whose `name` is in the prompt pool (builtin + custom + "Hi") until 8 remain.
   User conversations with other names are never touched.
3. **Session**: `GET .../conversations/<id>/session` (30 s) returns `data.link`
   (or `endpoint`), `data.token`, `data.sessionId` (or `session_id`, else the
   conversation id) and `data.cwd` (default `/workspace`). Missing link/token or
   any error is retryable, phase `session`.
4. **Turn**: `wb_webagent.run_turn` (`wb_webagent.py:221-282`), ACP over
   streamable HTTP. Every request carries `Authorization: Bearer <session token>`
   and the web UA. With a per-account proxy the connection is an HTTP CONNECT
   tunnel with `Proxy-Authorization: Basic` built from the proxy URL userinfo;
   both `http://u:p@h:port` and scheme-less `u:p@h:port` are accepted, proxy port
   default 8080 (`35-66`, `99-111`).
   - `GET <link>` with `Accept: text/event-stream` must answer 200 with header
     `Acp-Connection-Id`. A reader thread parses `data:` lines as JSON-RPC and
     counts `session/update` notifications and `agent_message_chunk` updates
     (`113-164`).
   - JSON-RPC POSTs go to the same `<link>` with `Content-Type: application/json`,
     `Accept: application/json, text/event-stream` and `Acp-Connection-Id`; status
     must be 200 or 202 (`166-189`):
     - id 1 `initialize` `{"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}`
     - id 2 `session/load` `{"sessionId", "cwd", "mcpServers": []}`
     - id 3 `session/set_model` `{"sessionId", "modelId": <model>}`; failure is
       only logged. This call decides billing: without it the session runs on
       the account default ("Auto"), which costs credits (`201-218`).
     - id 4 `session/prompt` `{"sessionId", "prompt":[{"type":"text","text": <prompt>}]}`
   - Poll `GET .../conversations/<id>` every 3 s until `data.status` is
     `completed` (ok) or `failed|error`, for at most `wait_seconds`.
   - Result `{ok, status, events, chunks, elapsed_ms, error}`. A failed turn is
     always retryable, phase `turn`.
5. **Release** (always, in `finally`): `POST .../<id>/stop` body `{}`; for a
   failed attempt also `POST .../<id>/delete`. Errors are swallowed
   (`2235-2266`). A successful conversation stays in the web history. Skipping
   the release leaks one of the 10 active slots per run.

Rust: run the SSE GET as its own task with a streaming body, send the RPCs as
plain POSTs, and honor the per-account proxy including CONNECT auth.

### 5.5 Manual triggers

| Endpoint | Body | Effect |
|---|---|---|
| `POST /accounts/daily-chat` | `{uid}` / `{uids:[...]}` / `{}` | `daily_chat(trigger="manual")`, sequential, no stagger. `uids` selects exactly those intl, task-active accounts (empty list = none); `{}` = all intl task accounts (`wb_proxy.py:21049-21077`) |
| `POST /accounts/daily-chat-web` | `{uid?}` | web leg only via `daily_chat_web(trigger="manual")`; it does not write `lastDailyChat`, and still skips accounts already done today (`21079-21099`) |

---

## 6. Window probe (J7) — `wb_window_probe.py`

Purpose: a free-model quota window starts at the first request after its reset.
The probe sends one minimal request at the reset instant so the new window
starts then instead of at the next real request.

- **Registration** `schedule(dir, uid, model, reset_at)` (`146-178`). Called from
  the chat path on HTTP 429 when the body yields a reset time
  (`wb_proxy.py:14297-14315`, `14674`) and from the quota scan. Ignored when the
  `window_probe` setting is off (default **on**, `wb_settings.py:2671-2681`),
  when `reset_at <= now`, or when the same reset is already stored (|diff| < 1 s).
- `parse_rate_limit_reset` (`wb_proxy.py:14200-14240`): regex
  `(?:reset at|将在)\s*(YYYY-MM-DD[ T]HH:MM:SS)`, fallback `(stamp)\s*UTC[+-]N`.
  The offset comes from `UTC([+-]\d{1,2})(?::?(\d{2}))?`; the stamp is converted
  with `timegm` minus that offset, never with the host zone.
- **State file** `accounts/model_windows.json` (atomic tmp + replace; read as
  `utf-8-sig`; a corrupt file reads as empty):

```json
{"version": 1, "saved_at": "YYYY-MM-DD HH:MM:SS",
 "windows": [{"uid": "...", "model": "hy3", "reset_at": 1760000000.0,
              "attempts": 0, "last_try": 0.0, "last_result": ""}]}
```

  Load prunes rows missing uid/model, with `reset_at <= 0`, or older than 7 days.
  The in-process cache is keyed by directory and reloads only when the directory
  changes, so hand edits need a restart.
- **run_due** (`234-280`): skipped when the setting is off or a run is already in
  progress (non-blocking lock). Due rows are `reset_at <= now`, ascending, at most
  8 per run, 0.4 s apart. A row whose account left the pool is dropped; a
  disabled account or one without a token keeps its row.
- **_fire** (`283-326`): refresh first if the token expires in < 120 s. Probe:
  `POST <chat_base_url>/v2/chat/completions`, chat headers (no `model` argument),
  body `{"model", "messages":[{"role":"user","content":"Hi"}], "stream":true,
  "max_tokens":16, "reasoning_effort":"none"}` through `build_upstream_body`,
  timeout 60 s (`329-366`). Unlike the daily chat, this path applies no CLI
  gzip or `cache_retention` adjustment.
  - success: `clear_model_cooldown(model)` and drop the row.
  - 429 with reset > now + 30 s: store the new reset, attempts 0.
  - otherwise attempts += 1; at 20 drop the row, else `reset_at = now + 15 s`.
- Not gated by the scheduler `enabled` switch.
- Inspection: rows appear in `GET /scheduler` under `windows` (10 rows: `uid, uid8,
  model, reset_at, reset_in, attempts, last_result`). No manual trigger endpoint.

## 7. Quota scan (J8) — `wb_quota_scan.py`

Purpose: detect exhausted (account x model) quota lines for watched models
before a real request hits them.

- Setting `quota_scan_models`: list of model ids (comma/semicolon/newline string
  accepted), default `[]` = feature off (`wb_settings.py:2771-2809`). Saved via
  `/settings/save`, at most 200 ids (`wb_proxy.py:20078-20096`).
- Constants (`36-54`): interval 6 h; retry 15 min; at most 6 consecutive
  failures; first delay 5 s plus 15 s stagger per new pair (cap 3600 s); batch 6;
  gap 0.4 s; rows untouched for 30 days pruned.
- **State file** `accounts/quota_scan.json`:

```json
{"version": 1, "saved_at": "...", "interval_hours": 6.0,
 "scans": [{"uid": "...", "model": "...", "last_scan": 0.0, "next_at": 0.0,
            "seen_at": 0.0, "attempts": 0, "last_try": 0.0,
            "last_result": "ok|throttled|error", "last_detail": ""}]}
```

- **due_targets** (`198-250`): pairs = watched models x enabled accounts with a
  token; new pairs get staggered `next_at`. Excluded: model not watched, account
  missing, a window already registered (`has_window`), `throttle_wait(model) > 0`,
  or not yet due (`max(last_scan + 6h, next_at)`) unless `force`. Sorted by due
  time, first 6.
- **_scan** (`286-354`): refresh if the token expires in < 120 s, then the same
  probe request as §6.
  - ok: clear the model cooldown; record `last_scan=now`, `result=ok`.
  - 429 with reset > now + 5 s: `note_error("HTTP 429 (model throttled)", model,
    until=reset)` and `wb_window_probe.schedule(...)`; record `throttled`.
  - otherwise attempts += 1; at 6 set `last_scan=now` (wait a full cycle), else
    `next_at = now + 900`.
- Manual: `POST /quota-scan/run` (panel) runs `run_due(force=True)` and returns
  `{"ok":true,"sent":N,"msg":...}` (`wb_proxy.py:20914-20935`). Inspection: 6
  rows in `GET /scheduler` under `quota_scans` (`uid, uid8, model, watched,
  pending, last_scan, last_result, last_detail, due_in`).

---
## 8. Credits

### 8.1 Background refresher (J9) — `CreditsRefresher` (`wb_accounts.py:3523-3664`)

- Thread `credits-refresher`, `interval_seconds = max(60, 1800)`; the loop waits
  first, then refreshes (`3644-3654`). `wake()` cuts the wait short; it is called
  when `credits_refresh_hours` is saved (`wb_proxy.py:19641-19642`).
- TTL: `settings.credits_refresh_hours`, default 12 h, max 720, negative or
  non-numeric -> default, **0 disables** (`wb_settings.py:33-42`, `1026-1043`).
- Each tick refreshes **one** account: the enabled account with a token, not
  parked, whose `credits.updated_at` is the oldest and older than the TTL; never
  fetched = infinitely old (`3582-3608`). This caps billing traffic at 48 calls a
  day regardless of pool size.
- Failure (`ok` false or exception) parks the uid for 6 h in memory
  (`_failed_cooldown`; the log text says "暂缓一小时", which is wrong) (`3623-3630`).
- Status (`3656-3664`): `ttl_hours, interval_seconds, last_run, last_uid,
  last_error, logs[-10:]`. It is not exposed by any HTTP route in this tree
  (`CREDITS_REFRESHER` is only started and woken, `wb_proxy.py:5913`,
  `19641`, `22833-22835`).

### 8.2 `Account.fetch_credits()` (`wb_accounts.py:3064-3122`)

Serialized per account (`_credits_lock`). On success it also calls
`revive_balance_cooldown()` and refreshes the paid/free tier at most once per 24 h
(`_refresh_tier_if_due`, `3053-3062`; `TIER_TTL_SECONDS`, `352`).

Path selection (`_fetch_credits_raw`, `3085-3122`):

| account | calls |
|---|---|
| static API key | enterprise usage (`/v2/...`) |
| cn | detailed path; if ok but `size == 0` and enterprise -> enterprise usage; if detailed fails with an auth error -> stop; enterprise -> enterprise usage; else fallback |
| intl | fallback |

All requests: billing headers, `http_json`, account proxy.

- **CN detailed** (`2630-2783`), headers + `X-Client-Platform: web`, base
  `https://www.codebuddy.cn`:
  1. `POST /billing/meter/get-user-resource-summary` `{}` (15 s) -> `data.Packages[].PackageCode`,
     `IsPaidUser`, tier fields. A 401-like error triggers one `refresh()` + retry;
     failing again sets `token_expired` and returns `need_relogin`.
  2. `POST /billing/meter/get-user-resource-free-packages`
     `{"PackageCodes":[...],"PageNumber":1,"PageSize":200,"Status":[0]}` -> `data.Accounts`.
  3. `POST /billing/meter/get-user-resource-paid-packages`
     `{"PackageCodes":[...],"PageNumber":1,"PageSize":200,"Status":[0,3],"NeedRenewInfo":true}`.
  4. `POST <chat_upstream>/v2/billing/meter/checkin-activity-status` `{}` (10 s) ->
     `data.today_checked_in, streak_days, daily_credit, today_credit, active`.
- **Fallback / intl** (`2785-2869`): `POST <billing_upstream>/v2/billing/meter/get-user-resource`
  `{"PageNumber":1,"PageSize":100,"ProductCode":"p_tcaca","Status":[0,3],
  "PackageEndTimeRangeBegin":"<now local>","PackageEndTimeRangeEnd":"2036-01-01 00:00:00"}`
  (30 s) -> `data.Response.Data.Accounts`; same 401 -> refresh -> retry rule.
- **Tier summary for intl** (`3019-3051`): `POST <billing>/billing/meter/get-user-resource-summary`
  with `X-Client-Platform: web`; skipped for enterprise/API key and while
  restricted.
- **Enterprise** (`2896-2975`): `POST <billing>/billing/meter/get-enterprise-user-usage`
  (`/v2/...` for API keys) `{}` -> `data.credit` (used), `limitNum`,
  `cycleStartTime`, `cycleEndTime`; synthesizes one `enterprise` package with
  `no_expiry: true`.

Package parsing (`_parse_package_account`, `2539-2628`) produces
`name, package_code, product_name, sub_product_name, grant_reason`
(`AccountAttributes[Key=grantReason]`), `resource_id, deal_name, create_time`
(ms -> local `YYYY-MM-DD HH:MM:SS`), `remain/used/size` (from the `*Precise`
fields first), `unit, in_usage, auto_renew, cycle_start_time, cycle_end_time,
expire_time` (deduction deadline preferred), `no_expiry` (> 730 days out,
`EXPIRY_SENTINEL_DAYS` `505`), `days_left, is_expired, status`.

Stored on the account as `credits` and saved to `accounts/<uid>.json`:
`remain, used, size, used_percent, remain_percent, is_paid_user, checkin,
earliest_expiring, packages[], updated_at (epoch), updated_iso` (`2768-2780`).

Other callers of `fetch_credits`: cat travel claim, growth/night tasks,
daily chat desktop leg, add-API-key (`wb_proxy.py:21417`). Between fetches the
chat path decrements the cached balance per request (`consume_credit`,
`wb_accounts.py:1263-1306`).

### 8.3 Manual sweep with guards (`wb_proxy.py:5915-5995`)

- Single-flight CAS flag `_CREDITS_SWEEP_RUNNING` (`5932-5967`): a second
  multi-account sweep returns immediately with `busy: true` and the current
  snapshot (HTTP 200).
- Pacing between accounts: uniform 0.8-1.2 s; env `WB_CREDITS_SWEEP_GAP_MS`
  (`"min-max"` or one number, ms; 0 disables; bad value -> default) (`5936-5951`).
- Single-account refresh skips both guards.
- A per-account exception is logged and recorded; the sweep continues.

| Endpoint | Behavior |
|---|---|
| `GET /accounts/credits` | full-pool sweep (guarded); returns `{"accounts": [...], "busy": bool}` (`wb_proxy.py:18912-18925`) |
| `POST /accounts/credits` or `/accounts/credits/fetch` | body `{uid}` (single, unguarded) / `{realm}` / `{}` (guarded if > 1 target); returns `{"results":[{uid, ok, credits, error}], "accounts":[...]}` (`20671-20692`) |
| `GET /accounts/credits/detail?uid=&refresh=1` | fetches when `refresh` or no packages cached; falls back to the first account when uid is unknown (`18938-18970`) |
| `POST /accounts/credits/detail` | `{uid, refresh=true}`; unknown uid -> error (`20694-20723`) |
| `GET /v1/balance-summary` | API-key route; snapshot only, never fetches (`6078-6118`) |

### 8.4 Credit grants history (`wb_proxy.py:6120-6263`)

`GET /accounts/credits/grants` (`18927-18936`) -> `credit_grants()`: **no
upstream calls**. One row per cached package across all accounts:
`uid, nickname, realm, name, product, grant_reason, size, remain, used, unit,
create_at, create_iso, expire_at, expire_iso, no_expiry, days_left, status,
action`.

- `status` (`6141-6155`): `expired` (flag, or `expire_time <= now`) >
  `used_up` (remain <= 0) > `active` (`in_usage`) > `available`.
- `action`: the latest local checkin/daily_chat attempt for that uid from the
  activity history whose time is within [-2 h, +10 min] of the package
  `create_time`: `{task, ts, ok, delta_seconds}` (`6158-6202`).
- Sorted newest first, then nickname, then name. Top-level
  `generated_at/iso`, `fetched_at/iso` (max `credits.updated_at`), `summary
  {count, size, remain, used, accounts}`.

---

## 9. Catalogue, models.dev, pricing

### 9.1 Catalogue fetch pipeline (`wb_proxy.py:10861-10902`, `11060-11520`)

- `fetch_models(realm)`: in-memory cache per realm with a 300 s TTL; misses are
  serialized by `_catalog_lock` (one upstream walk per realm).
- `curated_live_sources(realm)` (`11449-11482`) tries in order:
  1. **remote** `fetch_remote_product_config` (`11234-11276`): representative
     account of that realm (first with a token, `wb_accounts.py:4479-4484`);
     `GET <chat_upstream>/v3/config`, timeout 10, up to two attempts with
     `User-Agent = chat_ua` then `billing_ua`. Headers (`11075-11096`): `Accept`,
     `User-Agent`, `Origin`, `Referer`, `Authorization: Bearer`, `X-User-Id`, plus
     `X-API-Key` for static keys (without it the list is 2 short). Parse
     `agents[].models` (cli agent preferred, longest list) and a `models[]`
     metadata walk (`11099-11176`); empty list -> None. On success:
     `wb_tiers.ingest(realm, payload)` (PRO-only map, memory only) and
     `persist_remote_catalog` -> `catalog/product-config-<realm>.json`
     (dir `WB_PROXY_CATALOG_DIR` or `<script dir>/catalog`; raw response body;
     write-only archive, never read back; `11405-11425`, `catalog/README.txt`).
  2. **cache**: desktop cache file `~/.workbuddy-ai|.workbuddy/cache/acc-product-config-v3.json`
     (`11398-11402`). Absent on a VPS; keep the reader for parity, but it will
     normally miss.
  3. **legacy**: same file, lenient parser.
  4. intl only: `GET https://www.workbuddy.ai/v2/enterprises/personal/models`
     (chat headers of `POOL.pick("intl")`, 30 s) -> `data.agents[].models`
     (`11484-11505`).
  5. Bundled snapshot `wb_catalog.STATIC_INTL_MODELS` / `STATIC_CN_MODELS`
     (`wb_catalog.py:716-1731`) merged in every case by `merge_catalog`.
- `curate_remote_catalog` (`11196-11231`) drops non-chat ids, `-sg`/`-x` variants,
  and the paid sibling when a free (`x0.00`) `-f` sibling exists.
- A remote/endpoint success calls `note_remote_catalog(realm)`; `_catalog_meta`
  records `source` (`remote|endpoint|cache|legacy|bundled`), `fetched_at`,
  `file_mtime` for the panel.

### 9.2 Catalogue daily fallback (J10) (`wb_proxy.py:11340-11395`)

- Thread `catalog-daily`: sleep 60 s, then every 3600 s run `catalog_daily_pass`.
- A realm is due when `now - last remote success >= 24 h` (never = due). Realms
  without a representative account are skipped silently. Due realms call
  `refresh_catalog(realm)`.
- `refresh_catalog` (`11287-11338`): per-realm 30 s cooldown recorded before the
  fetch (failures also enter cooldown); only the remote source (no desktop
  file); failure keeps the old cache. Success replaces `_models_cache[realm]`.
- Manual: `POST /catalog/refresh {"realm":"intl|cn"}` (panel) ->
  `{ok, realm, source, count, cooldown?, retry_after?, msg}` (`wb_proxy.py:20958-20968`, `22097-22107`).

### 9.3 models.dev (J12) — `wb_modelsdev.py`

- Lookup order per field (`lookup`, `214-245`): upstream value > built-in table
  `CONTEXT_FALLBACK` (`35-70`) > local index `accounts/model.json` > default
  (context 1,000,000; output omitted).
- `refresh_async(dir)` (`182-212`) is called on every `GET /v1/models`
  (`wb_proxy.py:18808-18813`); process-wide throttle 300 s and single-flight.
  Worker: `GET https://models.dev/api.json`, `User-Agent: wb2api-hub`, timeout
  5 s, body capped at 32 MiB, no proxy. Builds `bare id -> (context, output)`:
  vendor providers (`zai, moonshotai, moonshotai-cn, openai, google, deepseek,
  minimax`) win, else the most common pair (ties: smaller); values > 1e9
  dropped (`146-179`). Saves `accounts/model.json` as `{"<id>": [ctx, out]}`
  (atomic). Failure logged, silently ignored.
- `_negatives` / `MODELSDEV_NEGATIVE_TTL` are declared but unused (`24`, `76`).

### 9.4 Price refresher (J11) — `wb_pricing.PriceRefresher` (`wb_pricing.py:2359-2600`)

- Thread `price-refresher`. Interval `settings.pricing_refresh_minutes`, default 5,
  max 43200, 0 = off; legacy `pricing_refresh_hours` migrated x60
  (`wb_settings.py:25-32`, `1413-1440`). Master switch `pricing_enabled`,
  default true (`1481-1490`).
- Loop (`2530-2573`): switch off or interval 0 -> block on `_wake` (no timer).
  On the first pass, fetch immediately only when no timeline exists, or right
  after the switch was turned back on; then `wait(interval)` and `run_once`.
- `run_once` (`2454-2496`), serialized: `GET https://openrouter.ai/api/v1/models`,
  `User-Agent: workbuddy2api-hub-pricing/1.0`, timeout 45 s, no proxy
  (`991`, `1128-1138`); plus `live_ids()` which calls
  `curated_live_sources` for intl and cn (a `/v3/config` fetch per realm,
  `2411-2452`). Writes the policy/timeline files under `USAGE_DIR`
  (`pricing-policies.jsonl`, `pricing-timeline.jsonl`; overrides in
  `pricing-overrides.json`, markup `pricing-markup.json`; `wb_pricing.py:1465-1538`).
  Failure keeps the previous prices. File formats belong to the pricing spec.
- Manual: `POST /pricing/refresh` (panel; spawns `run_once` in a thread and
  returns status immediately, `wb_proxy.py:20944-20956`); `POST /pricing/mapping`
  `{model, or_id}` saves an override and refreshes (`20974-21010`); toggling
  `pricing_variant_inherit` also spawns a refresh (`19698-19714`).
  `GET /pricing` -> `PRICING.status()`: `interval_minutes, enabled,
  master_enabled, running, last_run(_label), next_run(_label), last_error,
  last_models, last_extra, policies, models, ...` (`wb_pricing.py:2575-2600`).

---

## 10. Logs, activity history, heartbeats

### 10.1 Log ring and SQLite store (J13) — `wb_logstore.py`

- In-memory ring `LOG_BUFFER = deque(maxlen=2000)` (`wb_proxy.py:10377`); each
  entry `{id, ts "YYYY-MM-DD HH:MM:SS", time "HH:MM:SS", level, tag, msg}`;
  level/tag inferred from keywords when not given, then phone numbers masked
  (`10529-10560`).
- Optional DB `USAGE_DIR/logs.sqlite3`, off by default (`log_db_enabled`,
  `wb_settings.py:2015-2023`). Caps: `log_db_max_rows` (unset = no limit),
  `log_db_max_days` (default 7), `log_db_max_bytes` (unset)
  (`wb_settings.py:210-229`).
- Schema (`wb_logstore.py:48-59`): table `logs(id INTEGER PRIMARY KEY, ts, time,
  level, tag, msg)` + indexes on level, tag. `PRAGMA auto_vacuum=INCREMENTAL`,
  `journal_mode=DELETE`, `synchronous=NORMAL`.
- Single worker thread owns the only connection (`198-282`); `append` is a
  queue put (`INSERT OR REPLACE`), batches up to 256 per commit; reads (`query`,
  `stats`, `clear`) are queued jobs waited up to 8 s. Idle tick 5 s.
- Retention: rows/days every 5 s (`DELETE ... WHERE ts < cutoff`; keep newest N
  ids); bytes every 30 s: delete oldest by estimated row size toward 85% of the
  cap, then `VACUUM`, at most 4 passes (`334-406`). Messages truncated at 64 KiB.
- `configure()` on startup and on every settings save starts/stops the worker
  (`88-110`); turning it off leaves the file.
- Endpoints: `GET /logs?limit=200&level=&tag=&search=&since_id=` (DB first,
  memory fallback; adds `mode: db|memory`, `stored`, `db_bytes`,
  `wb_proxy.py:19192-19217`, `10586-10613`); `GET /logs/export`;
  `POST /logs/clear` (ring + DB, `10615-10622`).
- Rust: one `tokio` task owning a `rusqlite` connection behind an mpsc channel
  is the direct equivalent.

### 10.2 Activity history — `wb_activity.py`

- File `USAGE_DIR/activity_history.jsonl` (`22`, `74-91`), one JSON line per
  real attempt of `checkin` or `daily_chat` (not per cycle):
  `{"ts": "<local ISO 8601 with offset, seconds>", "uid", "nickname",
  "realm": "cn|intl", "task": "checkin|daily_chat", "trigger":
  "scheduler|manual|account_add|account_import|unknown", "ok": bool,
  "message": "<<=200 chars, redacted>"}` (`179-206`).
- `message` redaction patterns: bearer/basic tokens, `key=value` for
  token/api key/cookie/password/secret, JWTs, `sk-...`, 32+ hex, 40+ base64
  (`110-132`). Reads project rows onto the same whitelist (`147-164`).
- Writes never raise. Compaction keeps rows <= 90 days and the newest 5000;
  runs when the file exceeds 2,000,000 bytes or 6 h after the last compaction
  (process-local clock), atomically (`55-62`, `333-386`).
- `GET /activity/history?range=today|1d|7d|30d|90d|all&uid=&task=&result=ok|failed&limit=`
  (default 7d / 100, max 1000; bad filter -> 400) (`wb_proxy.py:18889-18910`,
  `wb_activity.py:285-331`).

### 10.3 Scheduler and refresher log rings

Scheduler 60 lines (§3.1), credits refresher 40, price refresher 40. Each is
also pushed into the gateway log with a tag.

### 10.4 SSE heartbeat (J14) (`wb_proxy.py:15351-15409`)

Per streaming response: a daemon thread writes `: keepalive\n\n` when nothing
was written for `SSE_HEARTBEAT_SECONDS = 15`, checking every
`min(0.1, interval/4)` s, under the same lock as data writes. Stops on close or
on a write error. Rust: a `tokio::select!` on the upstream stream with an idle
timer inside the response body stream; no extra thread.

---
## 11. Other on-demand upstream interactions

| Trigger | Upstream | Notes |
|---|---|---|
| `POST /accounts/test {uid, model?}` | `POST <chat_base_url>/v2/chat/completions` body `{"model": m (default deepseek-v4.1-flash), "messages":[{"role":"user","content":"hi"}], "stream": false}` via `build_upstream_body`; CLI identity drops fields and adds `cache_retention`; CLI and VSCode gzip; `headers("chat", model=m)`; timeout 30 (`wb_proxy.py:15110-15270`) | success -> `note_success(model)` (clears streak/breaker/degrade); 403 + account-level restriction -> `note_restricted` + persist + `note_account_restriction`; local-network errors do not penalize the account; other HTTP -> 60 s cooldown |
| `POST /accounts/test {realm?, model?}` (no uid) | same probe for every enabled account of the realm, `ThreadPoolExecutor(min(4, n))` (`PROBE_CONCURRENCY`, `15113`), plus `probe_local_network(realm)`: DNS + TCP connect (5 s) to the realm `chat_upstream` host:443, rejecting non-public addresses (`15124-15167`, `21256-21287`) | |
| `POST /accounts/refresh {uid?}` | `refresh()` for one or all accounts, then save (`21184-21194`) | sequential, no pacing |
| `POST /accounts/sync-profile {uid?|realm?}` | `GET <web origin>/console/account` (`https://www.workbuddy.cn` cn / `https://www.workbuddy.ai` intl) with `Authorization`, `Accept`, `x-client-platform: web`, `Origin/Referer: https://www.workbuddy.cn/...`, web UA; parses only `uid` and `nickname`; uid mismatch raises (`wb_accounts.py:155-183`, `1777-1791`; `wb_proxy.py:21196-21227`) | manual only, never scheduled |
| `POST /accounts/add-api-key` | `POST https://www.codebuddy.cn/v2/billing/meter/get-enterprise-user-usage` `{}` with `Authorization: Bearer <key>`, `X-API-Key`, `User-Agent: cn chat_ua` (`wb_accounts.py:1174-1204`); then `POOL.add` and `fetch_credits()` (`wb_proxy.py:21366-21420`) | key must pass the probe before it is stored |
| `POST /proxy/slots/test`, `POST /proxy/discover` | `GET https://api.ipify.org` through the proxy (12 s), then `GET http://ip-api.com/json/<ip>?lang=zh-CN&fields=...` direct (8 s) (`wb_proxy.py:6324-6390`, `wb_ipintel.py:23-27`). Discover scans `http://<WB_PROXY_DISCOVER_HOST or cli-proxy-mihomo>:<WB_PROXY_DISCOVER_PORTS or 17901-17910>` (`6311-6322`, `6372-6373`) | panel only |
| `scripts/probe_max_tokens.py` (CLI, not a thread) | `POST <gateway>/v1/chat/completions` against **this gateway** with `max_tokens` 1,000,000 and a 200,000-integer prompt, `stream:false`, timeout 900; if `finish_reason == "length"`, `completion_tokens` is the clamp (`wb_probes.py:87-120`) | writes `accounts/output_probes.json` `{"<model>": {model, requested_max_tokens, observed_completion_tokens, finish_reason, clamped, at}}`; read with a 60 s cache by `/v1/models` to add `output_clamp` / `max_output_tokens_clamped` (`wb_proxy.py:10983-10986`) |

The output-probe flow has no HTTP trigger and no background job. Port the
reader (`clamp_for`) and keep the script as an optional CLI subcommand.

---

## 12. OAuth "add account" flow (dashboard)

Code: `AccountPool.start_login` / `poll_login` / `cancel_login`
(`wb_accounts.py:4516-4603`); routes `wb_proxy.py:21101-21117`, `19028-19032`;
dashboard JS `dashboard.html:6090-6155`. All three routes require the panel
session.

```
Browser (panel)          Gateway                                   Upstream (realm chat_upstream)
  | POST /accounts/login/start {platform:"CLI", realm}
  |----------------------->| POST /v2/plugin/auth/state?platform=CLI  body {}
  |                        |------------------------------------------>|
  |                        |<-- {code:0, data:{state, authUrl}} -------|
  |<-- {state, authUrl, realm, platform}
  | user opens authUrl in a new tab and signs in
  | every 2.5 s: GET /accounts/login/poll?state=...
  |----------------------->| GET /v2/plugin/auth/token?state=...
  |                        |------------------------------------------>|
  |                        |<-- code 11217 (pending) | code 0 + token -|
  |                        | on token: GET /v2/plugin/login/account?state=...  (Bearer token)
  |                        | build Account, POOL.add -> accounts/<uid>.json
  |                        | cn: checkin(trigger="account_add")
  |<-- {status:"ok", account:{public view}}
```

Details:

- **start** (`4516-4536`): `POST <chat_upstream>/v2/plugin/auth/state?platform=<urlquoted platform>`,
  body `{}`, `http_json` (timeout 30, 3 tries). Headers: `Content-Type: application/json`,
  `Accept`, `X-Requested-With: XMLHttpRequest`, `User-Agent: billing_ua`,
  `Origin`, `Referer: <origin>/`. Missing `data.state` or `data.authUrl` -> error.
  Stores `logins[state] = {created, platform, realm}` **in memory** (lost on
  restart). Route errors: `TLSVerifyError` -> 502 with the hint; others ->
  502 "could not start login: ...". Dashboard sends `platform: "CLI"` and the
  realm currently viewed; route default realm = `CURRENT_REALM`.
- **poll** (`4538-4600`):
  - unknown state -> `{"status":"unknown", ...}`; older than 600 s
    (`LOGIN_TTL_SECONDS`, `355`) -> removed, `{"status":"expired"}`.
  - `GET <chat_upstream>/v2/plugin/auth/token?state=<state>`, same headers,
    timeout 30, 2 tries. Network error -> `{"status":"pending","message":"poll error: ..."}`.
  - `code == 11217` (`LOGIN_PENDING`, `354`) -> pending; other non-zero -> `{"status":"error"}`;
    `code 0` without `data.accessToken` -> pending.
  - uid = JWT `sub` (`wb_accounts.py:423-438`, unverified decode).
  - Nickname: `GET <chat_upstream>/v2/plugin/login/account?state=<state>` with the
    same headers + `Authorization: Bearer <token>`, timeout 15, via plain
    `urllib.request.urlopen` (**no account proxy, no keep-alive pool**);
    `data.nickname`; any error ignored.
  - Account record: `uid`, `nickname` (or uid[:8]), `domain` (`data.domain` or
    realm domain), `realm`, `platform`, `accessToken`, `refreshToken`,
    `expiresAt` (`normalize_epoch(data.expiresAt)` ms->s, else JWT `exp`),
    `source: "oauth"`, `enabled: true`.
  - `POOL.add` (`3755-3785`): on an existing uid it keeps `addedAt`, `path`,
    cached `credits`, `tier`, `lastCheckin`, `lastDailyChat`, proxy slot and
    legacy proxy, clears `token_expired` and errors, replaces the entry; then
    saves `accounts/<safe uid>.json` (non `[A-Za-z0-9_-]` -> `_`), and
    re-applies proxy slots, reserve credits and the expiring window.
  - cn realm: immediate `checkin(trigger="account_add")` (errors ignored).
  - Returns `{"status":"ok","account": account.public()}` and drops the state.
- **cancel**: `POST /accounts/login/cancel {state}` -> `{"cancelled": bool}`;
  the dashboard also calls it when the dialog closes (`dashboard.html:6064`, `6092`).
- The dashboard stops polling on `ok|error|expired|unknown` and keeps polling on
  `pending` (`dashboard.html:6137-6155`).

Rust notes: keep the login map in memory with the 600 s TTL and sweep expired
entries on access. The nickname call should go through the normal upstream
client; the Python behavior (direct, no proxy) is an accident of using bare
`urlopen`. Treat that as a known deviation and decide explicitly.

---

## 13. Shutdown

- There is **no** signal handler in the Python gateway (`grep signal` finds only
  `wb_update_portable.py` and tests). On SIGTERM CPython's default action kills
  the process immediately; on SIGINT `KeyboardInterrupt` ends `serve_forever`,
  logs `bye`, `server_close()` (`wb_proxy.py:22996-23005`).
- All workers are daemon threads; nothing is flushed or joined on exit. The log
  store is never `close()`d by the gateway (pending queued rows are lost; the DB
  stays consistent because of the rollback journal).
- Durability relies on every state write being atomic tmp + `os.replace`
  (accounts, settings, model_windows, quota_scan, output_probes, model.json,
  catalog archive, activity compaction). Activity rows are plain appends.
- `POST /shutdown` (panel): replies `{"ok":true,"shutting_down":true}`, then a
  thread sleeps 0.4 s and calls `os._exit(0)` (`wb_proxy.py:18758-18770`).
- Windows console-close handler (`install_console_close_handler`, `243-275`) is
  Windows-only; drop it.
- Docker: exec-form `CMD` keeps python as PID 1 so `docker stop` delivers
  SIGTERM (`Dockerfile:30-36`); compose uses `restart: unless-stopped`
  (`docker-compose.yml:9`).

Rust target (an improvement, not parity): handle SIGTERM and SIGINT with
`tokio::signal`; stop accepting connections; cancel the job token so loops
exit at their next await point; let in-flight HTTP responses finish with a
grace period (for example 10 s, below Docker's default 10 s stop timeout or
with `stop_grace_period` raised); flush the log-store channel and close
SQLite; do not interrupt a cycle in the middle of an atomic file write.
In-flight scheduler cycles may simply be abandoned: every daily marker is
written only after the upstream confirmed, so the next run retries.

---

## 14. At-rest encryption (`wb_atrest.py`) — Linux relevance

What it is: a **decryptor** for the desktop client's at-rest envelopes, not
encryption of the gateway's own files.

- The desktop client (since 2026-09-24) stores `accessToken`, `refreshToken`
  and on cn also `nickname` / `phoneNumber` in its `.info` file as
  `{"$wbEncrypted": suite, "envelope": base64(JSON{suite,keyId,nonce,authTag,ciphertext})}`
  (`wb_atrest.py:1-37`, `111-139`).
- Crypto: AES-256-GCM; `key = sha256(atRestSecretKey base64 string)`;
  `keyId = sha256(key).hex[:16]`; AAD =
  `"WB-AAD\0" 0x01 LP(magic) LP("sym-v1") u32be(suite) LP(keyId) code 0x00 0x00`
  with `LP = u32be length + utf8`, magic/code per framing
  `file WBEF1/1, field WBEV1/2, record WBER1/3, stream WBES1/4`
  (`50-104`). Pure-Python AES/GHASH implementation (`166-293`).
- Key storage: the key is compiled into the desktop app. It is recovered from
  the running WorkBuddy process memory (Windows only, `hunt_key`) or pasted by
  the operator (`adopt_key`: 64-hex protector key or 44-char base64 secret),
  and is verified against `~/.workbuddy-ai|.workbuddy/keyblob`'s `static-v1`
  slot (`324-399`, `796-836`). It lives **only in process memory**
  (`_KEY_CACHE`, `778-790`); never written to disk.
- Gateway files written by this project are **not** encrypted: `accounts/<uid>.json`
  holds plaintext `accessToken` and `refreshToken` (`wb_accounts.py:1018-1019`,
  `1206-1236`). `seal()` exists only for tests (`wb_atrest.py:295`;
  `tests/_test_desktop_atrest.py`).
- Endpoints: `POST /accounts/import/desktop` with `{recoverKey}`, `{keyText}`,
  `{forgetKey}`, `{path}`, `{all}`, `{}` (`wb_proxy.py:21119-21182`).

Linux verdict: **drop**. A VPS has no desktop client, no keyblob and no process
to scan, and desktop credential import is out of scope. Accounts arrive via
OAuth (§12), JSON import or API key. If a future requirement wants to accept a
pasted desktop `.info`, the decrypt path (AAD + AES-GCM with the `aes-gcm`
crate) is small and well specified above. Separately, consider `0600`
permissions on `accounts/*.json` and `settings.json` in the Rust port: the
Python code relies on directory permissions only.

---

## 15. Dropped (out of scope)

- `wb_atrest.py` hunt/adopt + `import_desktop_credential` / `scan_desktop_credentials`
  (`wb_accounts.py:4605-4834`): reads the Windows/macOS desktop client's
  encrypted `.info` credentials, recovering the AES key from process memory via
  ctypes `ReadProcessMemory`.
- `wb_updates.py` (`UpdateChecker`, J16): polls the GitHub releases API
  (`https://api.github.com/repos/<repo>/releases`, optional `GITHUB_TOKEN`,
  30 s first delay, 300 s poll, daily window) to report a newer stable release;
  routes `/updates`, `/updates/check`.
- `wb_update_request.py`: writes a validated install/rollback request into a
  root-owned spool for the privileged systemd updater and reads its status;
  routes `/updates/install`, `/updates/rollback`, `/updates/backups`.
- `wb_update_portable.py`, `wb_update_apply.py`: portable-zip self-updater
  (download, SHA256SUMS verify, stop gateway via SIGTERM, replace files,
  restart, roll back).
- `wb_update_external.py`: one-shot reference executor for non-systemd
  deployments that consumes `request.json` and writes `status.json`.
- `console_tail.py`: standalone terminal viewer that polls `/logs` incrementally.
- `ops/aliyun/*`: systemd path/service units and a root-owned Python updater
  that applies queued releases on an Aliyun host.
- `install_console_close_handler` (`wb_proxy.py:243-275`): Windows
  `SetConsoleCtrlHandler` to release the port when the console closes.
- Desktop product-config cache reader (`~/.workbuddy*/cache/acc-product-config-v3.json`,
  §9.1 step 2-3): keep as optional; always misses on a VPS.

---

## 16. Rust port checklist

1. One shared `AppState` (pool, settings cache, log ring, stores) and one
   `CancellationToken`.
2. Jobs as tokio tasks:
   - `scheduler`: 30 s tick, early wake from the window file, hour and HH:MM
     matching in the configured zone, `try_lock` single cycle, sequential
     per-account loop with `rand(2..5 s)` stagger, intl batch with
     `Semaphore(concurrency)` and `min(i*0.3, 2) s` start offsets.
   - `window_probe` and `quota_scan`: spawned each tick, each guarded by its own
     `try_lock`; independent of `enabled`.
   - `credits_refresher`: `select!(sleep(1800), notify)`; one stalest account per
     tick; 6 h in-memory park on failure.
   - `catalog_daily`: sleep 60 s then hourly; 24 h per-realm success stamp;
     30 s per-realm manual cooldown shared with `/catalog/refresh`.
   - `price_refresher`: `Notify`-driven park when off, interval in minutes.
   - `logstore`: mpsc-fed SQLite writer, 5 s idle trim, 30 s byte trim.
   - models.dev: single-flight with a 300 s throttle, triggered from `/v1/models`.
3. Persisted state (all atomic temp + rename, JSON with 2-space indent,
   UTF-8, read tolerant of a BOM): `accounts/<uid>.json`, `accounts/settings.json`,
   `accounts/active_realm.json`, `accounts/model_windows.json`,
   `accounts/quota_scan.json`, `accounts/output_probes.json`,
   `accounts/model.json`, `catalog/product-config-<realm>.json`,
   `usage/activity_history.jsonl`, `usage/logs.sqlite3`, pricing files in `usage/`.
4. In-memory only (lost on restart, keep that way): scheduler `enabled`,
   minute-slot dedup, credits park map, OAuth login states, restriction-wave
   hold, catalogue cache and tier map, task snapshot cache.
5. Upstream call budget guards to keep: 2-5 s account stagger; 1 s between CN
   sub-steps; 0.8-1.2 s manual sweep gap with single-flight; 0.4 s probe/scan
   gap with batch caps 8/6; 30 s catalogue cooldown; 300 s catalogue cache;
   skip restricted accounts and realm holds in every scheduled path.
6. Timezone: one configured zone (default `Asia/Shanghai` in containers) for
   schedules, "today" markers, `HH:MM:SS` stamps and activity ISO offsets.
   429 reset parsing uses the offset in the body, never the host zone.
