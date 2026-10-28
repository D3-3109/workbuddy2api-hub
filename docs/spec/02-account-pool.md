# 02 — Account pool: selection, routing and protection rules

Source baseline: tag `py-baseline-1.7.32`. All citations are `file:line` in that tree.
Scope: chat dispatch for `/v1/chat/completions`, `/v1/responses`, `/v1/messages`
(and their un-prefixed aliases). Windows desktop import / portable update are out of scope.

Notation: `now` = `time.time()` (epoch seconds, float). "Local" = process timezone
(`time.localtime`); the shipped Docker image sets `TZ=Asia/Shanghai`
(`Dockerfile:5`, `docker-compose.yml:23`). The Rust port must use the host/`TZ`
local zone for every rule marked "local".

---

## 0. Key constants (single table)

| Constant | Value | Source |
|---|---|---|
| `SOFT_RATE_BASE` / `SOFT_RATE_MAX` | 600 s / 7200 s (credential-scoped 429 ladder) | wb_accounts.py:640-641 |
| `SOFT_RATE_UNVERIFIED_BASE` / `_MAX` | 60 s / 600 s (unscoped 429 ladder, per model) | wb_accounts.py:648-649 |
| `BREAKER_THRESHOLD` | 3 consecutive hard failures | wb_accounts.py:650 |
| `BREAKER_COOLDOWN` / `_MAX` | 1800 s / 21600 s | wb_accounts.py:651-652 |
| `FAIL_WINDOW` | 600 s, env `WB_FAIL_WINDOW` (0 = no decay; negative/garbage → 600, clamp ≥0) | wb_accounts.py:659-662 |
| `DEGRADE_THRESHOLD` | 5 consecutive unknown failures | wb_accounts.py:663 |
| `DEGRADE_COOLDOWN` / `_MAX` | 600 s / 7200 s | wb_accounts.py:664-665 |
| `RESTRICTED_ACCOUNT_COOLDOWN` | 7200 s (403/11140, persisted) | wb_accounts.py:671 |
| backoff step cap | exponent clamped to 20 steps | wb_accounts.py:674-677 |
| token near-expiry threshold | 120 s → synchronous refresh inside `ready()` | wb_accounts.py:1634-1646 |
| `SessionAffinity` | ttl 7200 s, max_entries 5000, history 3 | wb_accounts.py:3435 |
| `AFFINITY_BY_PREFIX` | env `WB_AFFINITY_BY_PREFIX`, default on | wb_proxy.py:10271 |
| `AFFINITY_MAX_MSGS` | env `WB_AFFINITY_MAX_MSGS`, default 400 (0 = no cap) | wb_proxy.py:10292 |
| `AFFINITY_PAGE_SIZE` | env `WB_AFFINITY_PAGE_SIZE`, default 3 | wb_proxy.py:10305 |
| prefix hash | sha256 hex, first 16 chars, key prefix `pfx-` | wb_proxy.py:10336-10338 |
| `MAX_PRODUCT_SWITCHES` | 4 per (uid, model) per 60 s round | wb_proxy.py:13601, 13713-13721 |
| `EGRESS_SLEEP_MAX` | 5.0 s max sleep per egress backoff in one request | wb_proxy.py:13700 |
| `ACCOUNT_SLOT_WAIT_MAX` | 2.0 s (poll every 0.05 s) for pinned / refresh-retry accounts | wb_proxy.py:13705, 14527-14549 |
| `EGRESS_BACKOFF_BASE` / `_MAX` | 1 s / 15 s, doubling, jitter ×U(0.75, 1.25) | wb_egress.py:22-28 |
| `EGRESS_STREAK_RESET` | 60 s of quiet resets an egress streak | wb_egress.py:25 |
| `RESTRICTED_403_ROTATE_MAX` | env `WB_RESTRICTED_403_ROTATE_MAX`, default 0 | wb_proxy.py:14091 |
| `RESTRICTION_WAVE_MIN` | env `WB_RESTRICTION_WAVE_MIN`, default 2 (min 1) | wb_proxy.py:14092 |
| `RESTRICTION_WAVE_WINDOW` | env, default 900 s | wb_proxy.py:14093 |
| `RESTRICTION_HOLD_SECONDS` | env, default 1800 s (0 = disable pause) | wb_proxy.py:14094 |
| `CAP_LIMIT_CODE` | 6004 | wb_proxy.py:14032 |
| `SOFT_LIMIT_CODES` | (14003,) | wb_proxy.py:13966 |
| `MODEL_SCOPED_ERROR_CODES` | {11102, 11129, 11134, 11150, 11158} | wb_proxy.py:13853 |
| `MAX_CONCURRENT_CHAT` | env `WB_MAX_CONCURRENT_CHAT`, default 32; `auto`/`pool`/`dynamic` → max(32, ready accounts at startup) | wb_proxy.py:52-58, 389-418, 22808 |
| `CHAT_SLOT_WAIT_SECONDS` | env `WB_CHAT_SLOT_WAIT`, default 30 s | wb_proxy.py:59 |
| `account_max_concurrent` | settings, 0..10, default 0 (= unlimited) | wb_settings.py:143-160 |
| upstream timeouts | header 120 s, idle 300 s (settings `upstream.*`) | wb_proxy.py:14243-14266, wb_settings.py:1522-1524 |
| daily stats TTL `_STATS_TTL` | env `WB_STATS_TTL`, default 15 s | wb_proxy.py:1820 |
| free-model cache TTL | 60 s | wb_proxy.py:3228 |
| remaining payload TTL | env `WB_REMAINING_TTL`, default 15 s | wb_proxy.py:2166 |
| remaining-priority bands | ratio ≤0.05→4, ≤0.15→3, ≤0.30→2, else 1 | wb_proxy.py:3170 |
| `expiring_window_days` default | 7 (all other limits default 0 = off) | wb_settings.py:768 |
| `EXPIRY_SENTINEL_DAYS` | 730 (packages ending later count as never expiring; applied at package build time) | wb_accounts.py:505 |
| Credits refresher | tick 1800 s, TTL `credits_refresh_hours` default 12 h, failed account parked 6 h | wb_accounts.py:3543-3555, wb_settings.py:40 |
| Prompt degrade window | until next 00:00 **UTC+8 fixed** (not local) | wb_prompt.py:106-132 |
| 402 park | until next **local** 04:00 | wb_accounts.py:619-632, 3178-3191 |

---

## 1. Lifecycle of one gateway request

### 1.1 HTTP handler stage (`do_POST`, wb_proxy.py:22031-22229)

Order matters; each step can terminate the request.

1. **Pinned route** `/u/<ref>/...` (wb_proxy.py:18593-18630). `_strip_pinned_prefix_only`
   resets the thread-local pin, strips `/u/<ref>`; empty ref or no trailing `/` → **404**
   `pinned account route must be /u/<ref>/v1/...`. Pool missing → **503**.
   `POOL.find_unique(ref)` (wb_accounts.py:4486-4514): exact uid → unique nickname →
   unique uid prefix; `is ambiguous` → **409**, `was not found` → **404**.
   The resolved account is stored thread-locally (`_PINNED.account`, wb_proxy.py:5902-5910).
2. Route allow-list; unknown path → 404 (wb_proxy.py:22158-22163).
3. **Auth** (`_key_ok`/`_authorized`, wb_proxy.py:18441-18549). Panel session counts as
   authorized (no key entry). `identify_key` (wb_proxy.py:553-560) matches panel keys
   (`hmac.compare_digest`, enabled and not deleted, wb_settings.py:709-738); the
   launcher `API_KEY` is only accepted when no panel key exists. `auth_required()` is false
   when `auth_disabled` or no enabled key and no launcher key (wb_proxy.py:546-552).
   Failure → **401**.
4. Anthropic route disabled by default → **400** (wb_proxy.py:22172-22180).
5. Parse JSON body.
6. **Model aliases** (wb_proxy.py:22186-22191):
   - `_apply_model_aliases` (wb_proxy.py:337-354): table `settings.model_aliases`
     normalised to `{lowercase alias: target}` (wb_settings.py:1299-1329); lookup key is
     `model.strip().lower()`; target replaces `payload.model`.
   - `_apply_background_model_alias` (wb_proxy.py:316-334): if lowercase model ∈
     `WB_BACKGROUND_MODEL_NAMES` (default `gpt-5.6-terra,gpt-5.6-luna,gpt-5.3-codex`)
     **and** `background_request_reason(payload)` is non-empty, rewrite to
     `WB_BACKGROUND_MODEL_TARGET` (default `deepseek-v4.1-flash`). Independent of
     `BLOCK_BACKGROUND_REQUESTS`.
7. **Per-key concurrency** `_acquire_key_slot` (wb_proxy.py:22193-22202, 22603-22638).
   Non-blocking; over limit → **429** `rate_limit_error`
   (`Rate limit reached: API key '<name>' reached its concurrency limit (N in flight)`),
   an error row is recorded. Released in `finally` after the handler returns
   (wb_proxy.py:22228-22229).
8. **Global chat semaphore** `_chat_slots.acquire(timeout=CHAT_SLOT_WAIT_SECONDS)`
   (wb_proxy.py:22208-22227). Blocking wait up to 30 s; timeout → **503**
   `gateway is at its concurrent chat limit (N in flight); retry shortly`
   (Anthropic route: `overloaded_error`). Held for the whole handler including the
   stream body. `count_tokens` does not take a slot.
9. `_dispatch_chat_post` (wb_proxy.py:22231-22347):
   - `BLOCK_BACKGROUND_REQUESTS` (default **False**, wb_proxy.py:12917): when true and
     `background_request_reason` non-empty → **400** (wb_proxy.py:22234-22243).
     `background_request_reason` (wb_proxy.py:12954-13019): reads only
     `client_metadata` (`request_kind`/`turn_trigger`/`thread_source`, either as plain
     keys or inside a JSON-string value, also `kind`/`trigger`/`source` inside JSON);
     compaction requests whose `thread_source` is not a background source are exempt;
     otherwise any `BACKGROUND_TRIGGER_KEYWORDS` substring of the joined lowercase values
     matches.
   - Responses routes branch to `_handle_responses` (same downstream logic, below).
   - `build_upstream_body(payload)` once (`forwarded`), reused as `prebuilt_body`.
   - `session_key = extract_session_key(headers, payload)` (wb_proxy.py:15266-15278):
     first non-empty of headers `X-Conversation-Id`, `Conversation-Id`,
     `X-Session-Id`, `Session-Id`, body `conversation_id`, `session_id`,
     `metadata.conversation_id`; stripped string.
   - **Realm**: `explicit_realm = _request_realm()` (wb_proxy.py:18515-18533): key-bound
     realm → `X-Realm` header → `?realm=` query. `req_realm = explicit_realm or
     CURRENT_REALM`. Not validated: an unknown header value becomes a realm with no
     accounts (→ "no usable account").
   - `_cross_realm_error` (wb_proxy.py:18464-18480): `exclusive_realm(model)`
     (wb_proxy.py:219-240) differs from `req_realm` → **400**.
   - `_banned_model_error` (wb_proxy.py:12879-12900): defaults empty → never bans.
   - `_key_model_error` (wb_settings.py:428-444): key `models` list non-empty → fnmatch
     (case-sensitive on lowercased name and patterns) must match; empty requested model
     is refused → **400**.
   - `open_upstream_with_fallback(payload, session_key, target_realm=req_realm,
     allow_realm_fallback = not explicit_realm, prebuilt_body, defer_egress_success=True,
     allowed_accounts=key.accounts or omitted)` (wb_proxy.py:22280-22284, 13082-13090).
   - Exception → status mapping: §1.4.
   - Success: stream or non-stream response; stream gets a `reopen` closure that calls
     `open_upstream(..., pin_account=account)` (wb_proxy.py:22334-22347).

Note: because `target_realm` is always non-empty in the three chat handlers,
`detect_model_realm` (wb_proxy.py:192-211) is effectively unused for routing there;
model→realm inference only shows up via `_cross_realm_error`.

`CURRENT_REALM`: env `WB_PROXY_DEFAULT_REALM` default `intl` (wb_proxy.py:191),
overridden at startup from `accounts/active_realm.json` `{"realm": "intl"|"cn"}`
(wb_proxy.py:6018-6031) and changed by POST `/realm` with a panel session
(wb_proxy.py:21014-21025, persisted immediately, 6032-6041).

### 1.2 Realm fallback wrapper (`open_upstream_with_fallback`, wb_proxy.py:15036-15080)

```
realm = target_realm or detect_model_realm(model) or CURRENT_REALM
try: return open_upstream(...)
except RateLimited as e: backup = backup_or_raise(e)
except RuntimeError as e:
    if not str(e).startswith("no usable account"): raise
    backup = backup_or_raise(e)
return open_upstream(..., target_realm=backup)   # second failure propagates as-is

backup_or_raise(e):
    if not allow_realm_fallback or not settings.realm_fallback_enabled: raise e
    if realm != "intl": raise e                       # one-way intl -> cn only
    if exclusive_realm(model): raise e                # model only on one side
    if POOL.count_ready(realm="cn", model=model) <= 0: raise e
    return "cn"
```
`realm_fallback_enabled` default false (wb_settings.py:1981-2002). Not triggered by
ContentRejected (incl. 11140), PinnedAccountError, HTTPError, LocalNetworkError,
TLSVerifyError. `count_ready` for cn ignores the key's `allowed_accounts` (quirk; the
second `open_upstream` still honours it).

### 1.3 `open_upstream` (wb_proxy.py:14330-14993) — pseudocode

```
apply_daily_guards()                                     # 14340; §6
POOL.apply_remaining_weights(remaining_schedule_weights() if remaining_priority_enabled else None)  # 14345-14347
realm = target_realm or detect_model_realm(model) or CURRENT_REALM
model = str(payload.model or "")
pinned = pin_account if given else thread-local pinned_account()
if pinned:
    if allowed_accounts is not None and pinned.uid not in allowed: raise PinnedAccountError(403)
    if pinned.realm != realm: raise PinnedAccountError(409)
    if not pinned.ready(model): raise PinnedAccountError(503, unavailable_reason)
hold = restriction_hold_remaining(realm)
if hold > 0: raise RateLimited(wait=int(hold)+1, message=restriction_hold_message)   # 14366-14370
upstream_body = prebuilt_body or build_upstream_body(payload)
page = None
if not session_key: session_key, page = affinity_route(upstream_body.messages)       # §3.5
turn_request_id = new_turn_request_id()
ready_total = POOL.count_ready(realm, model, allowed?)                              # 14395-14401
total = 1 if pinned else max(1, ready_total)
tried, charged, refreshed, busy = {}, {}, {}, {}
auto_switch = settings.auto_switch_product if allow_product_switch and not pinned else False
egress_backoff = settings.egress_backoff_enabled
max_attempts = 1 if pinned else max(2, total) + 1 + (4 if auto_switch else 0)        # 14473-14474
attempt = 0
loop:
    attempt += 1; if attempt > max_attempts: break
    release slot_held if any
    account = (pinned if pinned.uid not in tried)       # + slot wait ≤2 s
           or (pool.get(refresh_uid) once)              # + slot wait ≤2 s
           or pick_account()                            # pick_for_session excluding tried|busy, acquire slot
    if account is None and not pinned and (transient_hits or local_net_hits):
        tried.clear(); sleep(min(1.5 * (transient_hits or local_net_hits), 3.0)); account = pick_account()
    if account is None: break
    if account.realm != realm: demote; release slot; continue
    tried.add(uid); slot_held = account
    build per-attempt body/headers (identity-specific; spec 03)
    try:
        resp = urlopen(chat_url, timeout=header_timeout, proxy=account.proxy)
        set socket idle timeout
        account.note_success(model)                     # at header time, 14620
        if egress_backoff and not defer_egress_success: egress.note_success(key)
        reset_switch_counter(account, model)
        bind slot release to resp.close(); return (resp, account, effort)
    except HTTPError / Exception: per-class handling (§4), then continue or break
after loop: release slot_held
free-model credit fallback (§4.12)
if last_error: raise mapped error (§1.4)
if realm_model_throttled: raise RateLimited(wait=min model cooldown)
enabled = realm accounts with enabled && access_token
if enabled and all daily_limit_blocked: raise RateLimited(wait=seconds_until_local_midnight)
if enabled and model and all credit_limit_blocked(model): RateLimited(midnight)
if enabled and model and all model_token_limit_blocked(model): RateLimited(midnight)
if busy and last_error is None: raise RateLimited(wait=1, "every usable account is already serving ...")
raise RuntimeError("no usable account for realm '<r>': all are disabled, cooling down, expired or parked by the daily token limit" + pool_unavailable_detail)
```

`pick_account` (wb_proxy.py:14489-14509): loop `POOL.pick_for_session(realm,
session_key, exclude=tried|busy, model, page, allowed?)`; `None` → return None; if
`account_slot_acquire` succeeds return it; else `busy.add(uid)` and if
`len(busy) >= max(1, total)` return None, otherwise re-pick.

`pool_unavailable_detail` (wb_proxy.py:15082-15108) appends `；各账号：<uid8> <reason>，…`
using `Account.unavailable_reason(model)` (wb_accounts.py:1536-1589), or `；该区域没有任何账号`.

### 1.4 Final error mapping

Inside `open_upstream`, after the loop, when `last_error` is set (wb_proxy.py:14929-14957),
the first matching rule wins; `account_uid = last_uid` is attached:

| Condition | Raised |
|---|---|
| `is_tls_verify_error(last_error)` | `TLSVerifyError(tls_verify_hint)` |
| `looks_like_local_network(last_error)` | `LocalNetworkError(exc_detail)` |
| `last_429` set | `RateLimited(last_429, last_429_detail, wait=retry_after_seconds(model, realm))` |
| `last_403_detail` set | `ContentRejected(last_error, last_403_detail)` (also for 11140!) |
| otherwise | re-raise `last_error` (HTTPError or other exception) |

Note: a 400 is re-raised immediately inside the loop (wb_proxy.py:14798-14804), and
non-handled 1xx-4xx statuses (e.g. 404, 413, 422) hit the bare `raise` at
wb_proxy.py:14844 — both escape the loop without releasing `slot_held` explicitly (the
slot is never bound to a response; see §5.2 ambiguity).

`retry_after_seconds(model, realm)` (wb_proxy.py:13770-13777): min over the realm's
`enabled && access_token` accounts of `throttle_wait(model)` that are > 0, as int;
else 60.

`realm_model_throttled` (wb_proxy.py:13780-13799): true when the realm has ≥1
`enabled && access_token` account and **every** one has a model cooldown for `model` in
the future; wait = int(min). Only model cooldowns count, not account cooldowns.

Handler mapping (identical in chat wb_proxy.py:22285-22333, responses 21545-21592,
messages 21891-~21950 with Anthropic envelopes):

| Exception | HTTP | Body |
|---|---|---|
| `PinnedAccountError` | `exc.status` (403/409/503) | `invalid_request_error`, message |
| `ContentRejected` | 403 | `upstream 403: <detail>` (`permission_error` on Anthropic) |
| `LocalNetworkError` | 503 | `本机网络不可用：<detail> - 请检查本机网络/代理后重试（本次不计入账号熔断）` |
| `TLSVerifyError` | 502 | hint text |
| `RateLimited` | 429 | `_rate_limited` (wb_proxy.py:18325-18362): `Retry-After: wait`, body `{"error":{"message", "type":"rate_limit_error","code":429,"retry_after":wait}}`; message = `exc.message` if set, else `upstream rate limit reached for this model; retry in Ns` + ` - <detail[:200]>` |
| `HTTPError` | upstream code | `upstream <code>: <detail>` |
| `RuntimeError("no usable account…")` | 503 | message + ` - add or enable one at the dashboard (/)` |
| any other | 502 | `upstream unreachable: <exc>` |

Every error path calls `record_error(...)` (one JSONL row in `usage/usage.jsonl`,
wb_proxy.py:945-999). Every `_error` adds `gateway_hint` when `gateway_hint()` classifies
the message (wb_proxy.py:18308-18318, 13520-13582).

---

## 2. Candidate filtering

A candidate must pass, in this order:

### 2.1 Pool snapshot (realm, key allow-list, page)

`AccountPool.pick` (wb_accounts.py:4224-4259):
1. `snapshot = [a for a in accounts if not realm or a.realm == realm]` (insertion order
   = sorted filename order at load, then append order for adds; wb_accounts.py:3708-3722,
   3755-3785).
2. If `allowed is not None`: keep `a.uid in allowed`. `allowed` = set of exact uids from the
   key's `accounts` list; empty list ⇒ `None` (unrestricted) (wb_settings.py:456-499). Max
   200 refs, each ≤128 bytes.
3. If `page` set: `_page_slice` (§3.6).
4. Empty → None.

`pick_for_session` additionally enforces `account.realm == realm` and `allowed` on the
affinity path (wb_accounts.py:4198-4209). `open_upstream` re-checks realm after pick
(wb_proxy.py:14562-14565).

Account realm itself: static API key accounts (`ck_`/`pt_` prefixes) are always `cn`;
otherwise stored `realm` or detected from JWT issuer then domain, default `intl`
(wb_accounts.py:319, 776-781, 531-551).

### 2.2 `Account.ready(model)` (wb_accounts.py:1602-1646)

Evaluated in order; first failure returns False:

1. `enabled` false, `access_token` empty, or `token_expired` true.
2. `model_disabled(model)`: `model.strip().lower() in disabled_models` (exact match;
   list normalised to lowercase, deduped, wb_accounts.py:470-490, 1591-1600).
3. PRO gate: `model and not is_paid_user and wb_tiers.requires_pro(model, realm)`.
   `requires_pro` (wb_tiers.py:116-129): realm map known → `map[model] == "standard"`;
   realm map missing → any realm map says standard; empty/unknown → False. Map comes from
   `/v3/config` `modelTiers` (in-memory only, wb_tiers.py:65-102): skip `enabled: false`,
   skip priority-only badges (`订阅优先`/`priority`/`subscription priority` without
   `accessMode: exclusive`), `requiredUserType`/`tier` in
   {free, personal_free_user, personal-free-user} → free, other non-empty → standard.
   `is_paid_user` = `tier.paid` if known else `credits.is_paid_user` else False
   (wb_accounts.py:905-915).
4. `throttle_wait(model) > 0` (wb_accounts.py:3316-3327): max of
   `cooldown_until, balance_until, breaker_until, degrade_until, restricted_until` minus now,
   and `model_cooldowns[model]` minus now.
5. `reserve_blocked()` (wb_accounts.py:1242-1261): `reserve_credits > 0` and
   `credits` is a dict with numeric `remain` and `int(remain) <= reserve`. Unknown balance
   never blocks.
6. `daily_limit_blocked()` (wb_accounts.py:1364-1382): limit > 0 and
   `daily_tokens_today` not None and `>= limit`.
7. `credit_limit_blocked(model)` (wb_accounts.py:1409-1419): model non-empty, not in
   `free_models` (realm's free set), and `daily_credits_today >= daily_credit_limit > 0`.
   Unknown model counts as paid.
8. `model_token_limit_blocked(model)` (wb_accounts.py:1497-1518): limit > 0 and
   `model_daily_tokens[model] >= limit`. Key is the exact model string as logged.
9. Token expiry: `exp = expires_at or jwt_exp(token)`; `exp == 0` → True (API keys);
   `exp - now > 120` → True; else **synchronous** `self.refresh()` and its boolean is the
   result (network call, under per-account `_refresh_lock`). Refresh failure with 401
   text / missing refresh token / no token returned sets `token_expired = True`
   (wb_accounts.py:1821-1876). API-key accounts' refresh returns False without setting
   `token_expired`.

Side effects of `ready()` matter: it may hit the network and rewrite the credential file.
The pool deliberately calls `ready()` outside its lock (wb_accounts.py:4379-4383).

`count_ready(realm, model, allowed)` (wb_accounts.py:4171-4176) counts
`enabled && access_token && ready(model)` (also triggers refreshes).

### 2.3 Realm-wide pause (11140 wave)

Checked before any pick: `restriction_hold_remaining(realm) > 0` → `RateLimited`
(wb_proxy.py:14366-14370). See §4.7.

### 2.4 Per-account in-flight cap

Not a `ready()` filter: a candidate that fails `account_slot_acquire` is put in `busy` and
excluded from re-picks for this request (wb_proxy.py:14489-14509). See §5.

### 2.5 Things that are NOT filters in this tree

- `weight`, `note`, `tier` (other than PRO gate), `tasks_enabled`, `proxy` do not exclude.
- `is_model_banned` lists are empty by default (wb_proxy.py:12865-12876).
- Per-model 6004 caps act only through `model_cooldowns` (§4.4); caps are restored into
  `model_cooldowns` at load (wb_accounts.py:3725-3727, 1473-1495).
- There is no "realm active" filter beyond `req_realm`; accounts of the other realm are
  never candidates except through §1.2.

### 2.6 `unavailable_reason(model)` (diagnostic, wb_accounts.py:1536-1589)

Same order as `ready()` except penalties are reported as the single latest-expiring of
[`上游风控限制` restricted, `熔断` breaker, `降权` degrade, `余额保护` balance,
`软限流冷却` cooldown, `模型 <m> 冷却` model] with `剩余 <human_left>` and a streak suffix,
and the near-expiry branch reports `凭证已过期` without refreshing. Strings returned:
`已停用`, `无凭证`, `凭证已过期`, `该模型已被人工禁用`, `该模型仅限 PRO 账号`,
`余额低于保留线`, `今日 Token 额度用尽`, `今日积分额度用尽`, `该模型今日额度用尽`.
`_human_left`: `<60` → `Ns`; `<3600` → `Mm` or `MmSSs`; `<86400` → `Hh` or `HhMMm`;
else `%.1f days` (wb_accounts.py:3398-3415).

---

## 3. Ordering

### 3.1 Overall precedence

```
pick_for_session(realm, session_key, exclude, model, page, allowed):     # wb_accounts.py:4178-4222
    if session_key and page is None:
        for uid in affinity.recent(session_key):          # MRU order, unexpired only
            skip if uid in exclude or (allowed and uid not in allowed)
            a = get(uid); if a and a.realm == realm and a.ready(model):
                affinity.bind(session_key, uid); return a
    a = pick(realm, exclude, model, page, allowed)
    if a is None and page is not None:
        a = pick(realm, exclude, model, page=None, allowed)   # leave the page
    if a and session_key and page is None: affinity.bind(session_key, a.uid)
    return a

pick(...):                                                # wb_accounts.py:4224-4259
    snapshot = filter(§2.1)
    return (_pick_weighted_first(snapshot, exclude, model)    # manual weight > 1
         or _pick_expiring_first(snapshot, exclude, model)    # expiring credits
         or _pick_remaining_first(snapshot, exclude, model)   # remaining usage (opt-in)
         or _rotate_pick(snapshot, exclude, model, cursor_key=page))
```

Each tier only considers accounts that pass `ready(model)` and are not excluded. A tier
whose eligible set is empty returns None and the next tier runs. There is no randomness
in account selection; the only randomness in dispatch is the egress jitter (§4.9).

### 3.2 Manual weight (tier 1)

`weight` is an int 1..100 per account, persisted in the credential file; garbage → 1
(wb_accounts.py:823-826, 3896-3906). Only accounts with `weight > 1` enter this tier
(wb_accounts.py:4261-4282); they share a smooth weighted round-robin keyed by uid with
state `_manual_weights`, pruned to the current entries each pick.

### 3.3 Expiring credits (tier 2)

`in_expiring_window()` (wb_accounts.py:1348-1362): `expiring_window_days > 0` and
`soonest_expiring_days() <= window`. `soonest_expiring_days()` (wb_accounts.py:1308-1346):
min `days_left` over `credits.packages[]` that are dicts, not `no_expiry`, not
`package_code == "enterprise"`, not `is_expired`, with numeric `days_left >= 0` and
numeric `remain > 0`; None otherwise. Weight (wb_accounts.py:4304-4318):
`max(1, round(window - days) + 1)`, or 1 when days is None. Smooth WRR keyed by uid,
state `_expiry_weights`, pruned to current entries (wb_accounts.py:4284-4302, 4352-4356).
Window per realm from settings `limits.expiring_window_days` (default 7).

### 3.4 Remaining-usage priority (tier 3, opt-in)

Enabled by `settings.remaining_priority_enabled is True` (default off,
wb_settings.py:1933-1944). On each `open_upstream`, the proxy pushes
`{(uid, model): weight}` built from `remaining_usage()` rows with
`_remaining_schedule_weight(remaining, budget)` (wb_proxy.py:3170-3224): no weight when
either value is missing/non-numeric, `remaining <= 0`, or `budget <= 0`; else
ratio = remaining/budget mapped by bands ≤0.05→4, ≤0.15→3, ≤0.30→2, else 1; only
weights > 1 are kept (also re-filtered in `apply_remaining_weights`,
wb_accounts.py:4002-4022). Disabled → push `None` → table cleared. Any exception → `{}`.
`_pick_remaining_first` (wb_accounts.py:4320-4350): requires `model`; entries are
accounts with `table[(uid, model)] > 1` and `ready`; smooth WRR keyed by `(uid, model)`,
state `_remaining_pick_state`, pruned only to keys whose uid is still in the snapshot
(so other models' rotations survive). The remaining-usage estimator itself (budget
inference from 6004 events, segments, 6/3/1 day weighting, `estimate-daily.json`,
`limit-events.jsonl`) is documented in the usage spec; this spec only depends on the
row fields `uid`, `model`, `remaining`, `budget` (wb_proxy.py:2070-2166).

### 3.5 Smooth weighted round-robin (shared algorithm, wb_accounts.py:4358-4405)

nginx algorithm under the pool lock:
```
state = stored state pruned (see per tier)
total = 0; chosen = None
for (account, key, weight) in entries (snapshot order):
    state[key] += weight; total += weight
    if chosen is None or state[key] > state[chosen.key]: chosen = (account, key)   # strict >, first wins ties
state[chosen.key] -= total
store state; return chosen.account
```

### 3.6 Plain rotation (tier 4, wb_accounts.py:4452-4477)

`start = page_cursors.get(page, 0)` if a page is given, else the pool-wide `_cursor`.
Scan `offset in 0..len(group)`: `index = (start + offset) % len`; skip excluded; first
`ready(model)` wins and the cursor becomes `(index + 1) % len`. The cursor is an index
into the realm-filtered snapshot, and one `_cursor` is shared by both realms and all
models (so it moves irregularly when realms interleave; preserve this, it is harmless).

### 3.7 Session affinity

**Key derivation** (`affinity_route`, wb_proxy.py:10306-10355). Only used when the client
sent no session key (`extract_session_key` empty), computed from the upstream body
after `build_upstream_body` (so an injected system prompt is included,
wb_proxy.py:14377-14386):
```
if not AFFINITY_BY_PREFIX or not messages: return (None, None)
head = messages[:2]
if settings.affinity_stable_key_enabled and len(messages) > 2: head = messages[:3]
blob = json.dumps(head, ensure_ascii=False, sort_keys=True).encode("utf-8")
digest = sha256(blob).hexdigest()[:16]
key = "pfx-" + digest
if AFFINITY_MAX_MSGS > 0 and len(messages) > AFFINITY_MAX_MSGS:
    if AFFINITY_PAGE_SIZE > 1: return (key, (int(digest[:8], 16) % SIZE, SIZE))
    if AFFINITY_PAGE_SIZE == 1: return (key, None)        # cap disabled, pinned
    return (None, None)                                   # SIZE == 0: no affinity
return (key, None)
any exception -> (None, None)
```
Byte-exactness: Python `json.dumps` default separators are `", "` and `": "`,
`ensure_ascii=False`, keys sorted recursively; Rust must reproduce this serialisation
exactly to keep keys stable across the port (otherwise every live conversation
re-binds once after upgrade; acceptable but should be a conscious choice). Note
`affinity_stable_key_enabled` adds only the 3rd message although the settings comment
says "first assistant + the user after it" (wb_settings.py:178-187 vs wb_proxy.py:10331-10335).

Client-supplied session keys are used verbatim as affinity keys, never paged.

**Store** (`SessionAffinity`, wb_accounts.py:3417-3501), memory-only, one per pool:
- `bindings: key -> [(uid, expires_at)]`, MRU first, at most `history = 3` entries.
- `recent(key)`: drop expired entries, return uids in order (does not slide TTL).
- `get(key)`: front uid and slides its expiry to `now + 7200`.
- `bind(key, uid)`: if `len(bindings) >= 5000`, first rebuild the dict keeping only keys
  whose **front** entry is unexpired; then remove `uid` from the list, insert
  `(uid, now + 7200)` at front, truncate to 3.
- `demote(key, uid)`: remove that uid (or the front when uid None); delete key when empty.
- `unbind(key)`: delete.

Demote is called on every failure branch for the failing account: realm mismatch,
429 (all variants, including after a product switch), 403 (11140 and content review
pass-through; not the degrade retry), 402, 401 (before refresh), 5xx (egress and account
paths), transport/local-network/other exceptions (wb_proxy.py:14563, 14650-14651,
14702-14708, 14730-14731, 14757-14758, 14770-14771, 14775-14776, 14831-14832, 14840-14841,
14851-14852). Not called for 400, model-scoped 5xx, or TLS verify errors.

**Pages** (`_page_slice`, wb_accounts.py:4407-4450): ranks = index of uid in
`sorted(uid for a in realm-filtered snapshot)` (after the `allowed` filter, cached by the
uid tuple). If `size <= 1` or `size >= len(ranks)` the page is the whole snapshot;
otherwise members are accounts with `rank % size == index % size`. Page conversations are
never bound and rotate with a per-page cursor keyed `(index, size)`. If nobody on the page
is usable, `pick_for_session` falls back to a whole-pool pick (not bound either because
`page is not None`).

### 3.8 Breaker fairness

"Fairness" in this tree means the anti-accumulation rules, not an ordering term:
- `FAIL_WINDOW` decay: a failure more than 600 s after the previous one resets `fails`
  and `degrade_count` to 0 before counting (wb_accounts.py:3264-3273); soft streaks decay
  the same way on `last_soft_at` (wb_accounts.py:3224-3236). `WB_FAIL_WINDOW=0` restores
  cumulative counting.
- One charge per account per request (`charged` set, wb_proxy.py:14404-14436).
- Local-network failures never charge (§4.10); with egress backoff, transport errors and
  502/503/504 charge the egress instead (§4.9).
- Panel "test" (`probe_account`, wb_proxy.py:15169-15264) success calls
  `note_success(model)` and clears all account penalties. Failures: 11140 → restricted
  path (§4.7); local network or non-HTTP exceptions → `note_error(..., cooldown=0)`;
  other HTTP errors → `note_error(..., cooldown=60)` (account-level 60 s cooldown).
  No `fails` charge from tests.
CHANGELOG lines 326-331 and 435 document this intent.

---

## 4. Failure handling per error class

### 4.1 Account penalty primitives (all under `_throttle_lock`)

Backoff formula (wb_accounts.py:674-704):
`backoff(count, base, cap, offset) = min(base * 2^min(max(0, count - offset), 20), cap)`.

| Primitive | Effect | Source |
|---|---|---|
| `note_error(msg, cooldown=60, single_account=False, model=None, until=None, detail=None)` | `last_error = msg[:200]`, `last_error_detail = detail[:2000]`; if `_auth_failure_text(msg)` → `token_expired = True`. With `model`: `model_cooldowns[model] = now + wait` where `wait = max(1, until - now)` if `until` else (3 if single_account else cooldown); `model_cooldowns_at[model] = now`; return. Without model: `cooldown_until = now + (3 if single_account else cooldown)` (overwrites, not max). | wb_accounts.py:3130-3150 |
| `_auth_failure_text(t)` | lowercase; contains `<html` → False; else any of `401`, `unauthorized`, `authorization`, `token expired`, `invalid_token`. | wb_accounts.py:3383-3395 |
| `note_soft_rate(msg)` | decay soft streaks; `soft_streak += 1`; `cooldown_until = max(cooldown_until, now + soft_backoff(streak))` with base 600, cap 7200, offset 1 (600, 1200, 2400, 4800, 7200…). Returns wait. | wb_accounts.py:3214-3222 |
| `note_unscoped_rate(model)` | decay soft streaks; `unscoped_streak += 1`; returns `unverified_soft_backoff` base 60, cap 600, offset 1 (60, 120, 240, 480, 600). Does not itself set a cooldown. | wb_accounts.py:3238-3251 |
| `_decay_soft_streak` | if `FAIL_WINDOW` and `last_soft_at` and `now - last_soft_at > FAIL_WINDOW`: both streaks = 0; then `last_soft_at = now`. | wb_accounts.py:3224-3236 |
| `note_failure(msg)` (hard) | `last_error`; decay strikes; `fails += 1`; if `fails >= 3`: `breaker_until = max(breaker_until, now + breaker_backoff(fails))` base 1800, cap 21600, offset 3 (1800, 3600, 7200, 14400, 21600). | wb_accounts.py:3253-3262 |
| `note_unknown_failure(msg)` | `last_error`; decay strikes; `degrade_count += 1`; `fails += 1`; if `degrade_count >= 5`: `degrade_until = max(.., now + degrade_backoff)` base 600, cap 7200, offset 5; if `fails >= 3`: breaker as above. | wb_accounts.py:3275-3289 |
| `_decay_strikes` | if `FAIL_WINDOW` and `last_fail_at` and gap > window: `fails = degrade_count = 0`; `last_fail_at = now`. | wb_accounts.py:3264-3273 |
| `note_restricted(detail, cooldown=None)` | `last_error = "HTTP 403 (account restricted)"`; if `restricted_until <= now`: `restricted_until = now + max(1, cooldown or 7200)` (first hit only, repeated hits do not extend). | wb_accounts.py:3152-3176 |
| `note_balance_cooled(msg)` | `last_error`; `balance_until = max(balance_until, next_local_4am())`. | wb_accounts.py:3178-3191 |
| `revive_balance_cooldown()` | called after a successful `fetch_credits`; if `balance_until` set and `int(credits.remain) > 0`: clear `balance_until`, `last_error`, detail. | wb_accounts.py:3193-3212, 3064-3083 |
| `note_success(model)` | pop `model_cooldowns[model]` / `_at`; zero both streaks, `fails`, `degrade_count`, `last_fail_at`, `last_soft_at`; clear `breaker_until`, `degrade_until`, **`restricted_until`**, `last_error(_detail)`, `cooldown_until`. Leaves `balance_until` and other models' cooldowns. | wb_accounts.py:3291-3314 |
| `clear_error(model=None, keep_caps=False)` | model given: pop its cooldown unless `keep_caps` and a live cap exists. No model + keep_caps: pop every cooldown without a live cap. No model, no keep_caps: clear all model cooldowns. Then clear `last_error`, detail, `cooldown_until`, `balance_until`. Never touches `restricted_until`, breaker, degrade, streaks. | wb_accounts.py:3329-3359 |
| `note_model_cap(model, reset_at)` | only if `is_free_cap_model(realm, model)` (intl: hy4-preview-f, hy3, deepseek-v4.1-flash; cn: hy4-preview-f, hy3 — wb_accounts.py:464-467). Same `reset` as stored → only re-assert `model_cooldowns[model] >= reset`, return False. Else store `{"at": now, "reset": reset}`, `model_cooldowns[model] = max(.., reset)`, return True. | wb_accounts.py:1421-1453 |

`note_success` runs at **response-header time** (wb_proxy.py:14620), before the body
is read. A stream that dies later has already cleared the account's penalties.

### 4.2 Request-local bookkeeping in `open_upstream`

- `tried`: accounts attempted this request; excluded from picks. Cleared (with a sleep of
  `min(1.5 * hits, 3.0)` s) when the pool is exhausted and there were transient or
  local-network hits (wb_proxy.py:14552-14559), which allows another round over the same
  accounts until `max_attempts` is reached.
- `charged`: `charge_account` calls `note_failure`/`note_unknown_failure` at most once per
  account per request (wb_proxy.py:14427-14436). Does not limit `note_error` cooldowns.
- `refreshed`: 401 refresh at most once per account per request.
- `egress_wait`: one `wb_egress.note_transient` per egress key per request; later hits
  reuse the same wait (wb_proxy.py:14465-14471).
- `max_attempts = 1` (pinned) or `max(2, total) + 1 + (4 if auto_switch)`, where `total`
  is the ready count at request start. Every loop iteration counts, including 401
  refresh retries, degrade retries and realm-mismatch skips.

### 4.3 429, credential-scoped (wb_proxy.py:14639-14655)

`detail = body[:600]`, `reset_at = parse_rate_limit_reset(detail)`.
`rate_limit_is_account_level(detail, reset_at)` = `reset_at is None and
credential_scope_phrase(detail)` (wb_proxy.py:14048-14066, 13975-14003). Phrase list at
wb_proxy.py:13975-13985; ASCII phrases are matched with `\b<phrase>\b` on the lowercased
body, CJK phrases as substrings.
→ `note_soft_rate("HTTP 429 (account soft rate)")`, demote, `last_429 = exc`, continue
(rotate). No model cooldown, no product switch.

### 4.4 429, model-scoped (wb_proxy.py:14656-14712)

```
wait = max(1, reset_at - now) if reset_at else 60
if reset_at is None and rate_limit_is_soft_shape(detail):      # markers or code 14003
    wait = account.note_unscoped_rate(model)                   # 60..600 ladder
account.note_error("HTTP 429 (model throttled)", model=model, until=reset_at, cooldown=wait, detail=detail)
schedule_window_probe(account, model, reset_at)                # best effort, wb_proxy.py:14297-14315
if reset_at:
    note_limit_event(account, model, reset_at)                 # usage/limit-events.jsonl
    if rate_limit_is_cap(detail):                              # int(json.code) == 6004
        if account.note_model_cap(model, reset_at): account.save()   # persist modelCaps
if auto_switch and _try_switch_product(account, model):
    tried.discard(uid); account.clear_error(model=model, keep_caps=True); demote; continue
demote; last_429 = exc; continue
```
Notes:
- Unrecognised 429 bodies without reset clock: flat 60 s on the model, no streak.
- `note_error(model=...)` sets the cooldown to `now + wait` unconditionally (it can shorten
  a longer existing window).
- `parse_rate_limit_reset` (wb_proxy.py:14200-14240): un-escape `\uXXXX` when present;
  regex `(?:reset at|将在)\s*(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})`, fallback
  `(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})\s*UTC[+-]\d{1,2}`; offset from first
  `UTC([+-]\d{1,2})(?::?(\d{2}))?` (default 0); epoch = timegm(stamp) − offset. A 429 with
  any parsable stamp is model-scoped, even if it is not 6004.
- The 429 path never passes `single_account`, so the model window is always
  `until - now` (min 1 s) or `wait`, never the 3 s single-account shortcut.

**Product auto-switch** (wb_proxy.py:13708-13767), only when settings
`auto_switch_product is True` (default off, wb_settings.py:1790-1798) and not pinned:
sequence `workbuddy → vscode → cli` (cyclic) starting after the current product; first
candidate in `auto_switch_products` (default all three) wins. Limit: 4 switches per
`(uid, model)` where a round expires 60 s after the last switch; the log table is cleared
entirely when it exceeds 500 entries. `set_product` also calls
`clear_error(keep_caps=True)` (wb_accounts.py:1793-1808). The new product is written to
the credential file on the next `save()` of that account. `reset_switch_counter` on
success.

### 4.5 403 generic / content review (wb_proxy.py:14713-14761)

If not account-level (§4.6):
- First time in this request and `prompt_config.mode in ("passthrough", "append")`:
  `PROMPT_DEGRADE.trigger()` (process-wide, until next 00:00 UTC+8, not extended while
  active; wb_prompt.py:106-132), rebuild `upstream_body`, `tried.discard(uid)`, continue
  (same account normally picked again via affinity/rotation; no demote).
- Otherwise: demote, `last_error`, `last_403_detail = detail`, **break** → client gets
  403 `ContentRejected`. No account counters change.

### 4.6 403 code 11140, account restriction (wb_proxy.py:14723-14743)

Detection `restriction_is_account_level(detail)` (wb_proxy.py:14162-14182): regex
`"code"\s*:\s*"?11140"?` or lowercase body contains `unusual activity`,
`账号存在异常行为`, `已被限制`. Checked before the content-review branch.
```
account.note_restricted(detail, cooldown=7200)
_persist_restriction(account)              # account.save() -> restrictedUntil on disk
demote; last_error = exc; last_403_detail = detail
note_account_restriction(realm, uid)       # §4.7
if restricted_rotations >= RESTRICTED_403_ROTATE_MAX (default 0): break
restricted_rotations += 1; continue
```
Client sees **403** (`ContentRejected` mapping, §1.4) with the upstream body; the
`gateway_hint` explains 11140. Realm fallback never retries it (§1.2).

`restricted_until` is the only throttle window persisted (field `restrictedUntil`) and is
cleared only by `note_success` (a real success, e.g. panel test). Import ignores it
(wb_accounts.py:1035-1038). Scheduler tasks skip restricted accounts and paused realms
(wb_scheduler.py:377-391); tier fetch skips restricted accounts (wb_accounts.py:3036-3038).

### 4.7 Restriction wave → realm pause (wb_proxy.py:14096-14149)

Memory-only state: `_RESTRICTION_EVENTS[realm] = {uid: at}`, `_RESTRICTION_HOLD[realm]`.
On each event: drop entries older than `RESTRICTION_WAVE_WINDOW` (900 s), set
`events[uid or "-"] = now`; if `len(events) >= RESTRICTION_WAVE_MIN` (2) and
`RESTRICTION_HOLD_SECONDS > 0` (1800): `hold = max(old_hold, now + 1800)`. Dedup by uid.
While held, `open_upstream` raises `RateLimited(wait=int(hold)+1,
message=restriction_hold_message)` before any upstream call; the client gets **429** with
`Retry-After`. Note: `RateLimited` triggers realm fallback (§1.2) when enabled, so an intl
pause can still route to cn (cn has its own hold). Panel tests also record events
(wb_proxy.py:15233-15242). `reset_restriction_state()` clears both maps (no HTTP route
calls it in this tree).

### 4.8 402 out of credits (wb_proxy.py:14762-14773)

`note_balance_cooled(detail or "HTTP 402 (insufficient credits)")` → park until next local
04:00 (if now ≥ 04:00 today, tomorrow 04:00); demote; continue. `last_error = exc` (so a
pool of all-402 accounts ends by re-raising the HTTPError 402 to the client as
`upstream 402: …`). Memory-only; lifted early by a successful credits fetch with
`remain > 0`, or by `clear_error()` (enable/disable toggle, product switch).

### 4.9 5xx (wb_proxy.py:14805-14843)

```
detail = body[:600]; code = int(json(detail).code) or None
if code in MODEL_SCOPED_ERROR_CODES and env WB_MODEL_SCOPED_ERRORS_PENALIZE not truthy:
    last_error = exc; break                     # no charge, no demote; client gets upstream 5xx body
transient_hits += 1
if egress_backoff and status in (502, 503, 504):
    wait = charge_egress(chat_url, proxy)       # once per egress key per request
    demote; last_error = exc; sleep(min(wait, 5.0)); continue
charge_account(account, "HTTP <status>", unknown=False)   # note_failure -> breaker
demote; last_error = exc; continue
```
All statuses ≥ 500 (including non-standard 550) rotate. No per-account `cooldown_until`
is set; only the breaker after 3 strikes within the fail window. Final error: the last
HTTPError is re-raised and the client sees `upstream <code>: <body>`.

Egress backoff (wb_egress.py:35-80): per `egress_key = (scheme, host, port, proxy_raw)`
(proxy folded to direct when bypassed; wb_upstream_pool.py:751-765). `streak` resets if
more than 60 s since the last hit; `wait = min(1 * 2^(streak-1), 15) * U(0.75, 1.25)`.
The streak is a memory-only, process-global map. Success clears it (`note_success`): at
header time for non-deferred callers, or at end of body read for chat
(`defer_egress_success=True`) — see §4.13. Settings `egress_backoff_enabled` default off.
Note the backoff is advisory: nothing blocks other requests from using that egress; the
sleep happens only in the request that charged it (other concurrent requests sharing
the egress also sleep when they hit their own failure).

### 4.10 Transport / other exceptions (wb_proxy.py:14845-14899)

Order:
1. `is_tls_verify_error(exc)` → `last_error`, **break** (no demote, no charge). Final:
   `TLSVerifyError` → 502.
2. Demote.
3. `looks_like_local_network(exc)` (wb_proxy.py:13904-13927): `socket.gaierror`, or OS
   codes {ENETDOWN, ENETUNREACH, EHOSTUNREACH, -2, -3, -5, 100, 101, 113, 10050, 10051,
   10065, 11001}, or text markers (`gaierror`, `getaddrinfo failed`, `name or service not
   known`, `nodename nor servname`, `temporary failure in name resolution`, `network is
   unreachable`, `network is down`, `no route to host`) → `local_net_hits += 1`, no
   charge, `sleep(min(0.3 * hits, 1.5))`, continue. Final: `LocalNetworkError` → 503.
4. `is_transient(exc)` (wb_proxy.py:13820-13846): not TLS-verify, and OS codes {EPIPE,
   ECONNABORTED, ECONNRESET, ETIMEDOUT, 10053, 10054, 10060} or text markers (`ssl`,
   `unexpected_eof`, `eof occurred`, `remote end closed`, `connection reset`,
   `connection aborted`, `connectionreseterror`, `connectionabortederror`, `timed out`,
   `timeout`, `temporarily unavailable`, `bad gateway`, `502`, `503`, `504`,
   `incompleteread`). ECONNREFUSED is deliberately not transient.
   `transient_hits += 1`; egress backoff on → charge egress, `sleep(min(wait, 5))`,
   continue; off → `charge_account(unknown=True)` (degrade + breaker counters),
   `sleep(min(0.6 * hits, 2.0))`, continue. No cooldown is set.
5. Anything else (incl. ECONNREFUSED, proxy errors): `charge_account(unknown=True)`,
   `note_error(str(exc)[:120], cooldown=60, single_account=(total <= 1))` → account
   cooldown 60 s (3 s when the realm had ≤1 ready account), continue. Final: re-raise →
   502 `upstream unreachable: …`.

Text matching for `_network_error_details` concatenates `type(e).__name__ + str(e)` of the
exception and its `.reason`, lowercased; numeric codes come from `errno`/`winerror`
attributes (not for HTTPError) and `errno N`/`winerror N` regex in the text
(wb_proxy.py:13802-13817). Windows codes can be dropped on Linux but are harmless.

### 4.11 401 (wb_proxy.py:14774-14797)

```
demote
if uid not in refreshed:
    refreshed.add(uid)
    ok = account.refresh()            # exceptions -> False
    if ok: tried.discard(uid); refresh_uid = uid; last_error = exc; continue   # retry same account next iteration
account.note_error("HTTP 401", cooldown=60, single_account=(total <= 1))
last_error = exc; continue
```
`note_error("HTTP 401")` matches `_auth_failure_text` (contains `401`) and therefore sets
`token_expired = True`: the account leaves rotation (`ready()` returns False before it
would try a refresh) until a successful `refresh()` (scheduler keepalive when within the
48 h lead, wb_scheduler.py:398-411) or a re-login/re-add (`pool.add` clears it,
wb_accounts.py:3774). Also `cooldown_until = now + 60` (3 s for a single-account realm).
Static API-key accounts: `refresh()` returns False, so the first 401 marks them expired.
Final error if all fail: HTTPError 401 re-raised → client 401 `upstream 401: …`.

### 4.12 Free-model credit fallback (wb_proxy.py:14318-14328, 14905-14928)

After the loop and before error mapping: if `allow_credit_fallback`, the `(realm, model)`
pair is in `{("intl", "deepseek-v4.1-flash"): "deepseek-v4.1-flash-sg"}`, the feature is on
(settings `free_model_credit_fallback` or env `WB_ENABLE_FREE_MODEL_CREDIT_FALLBACK`
truthy; default off), and every realm account with `enabled && access_token` (and in
`allowed`) has `cap_until(model) > now` → recursive `open_upstream` with the paid model,
`allow_credit_fallback=False`, `allow_product_switch=False`; the response is tagged
`_wb_billed_model` so usage is recorded under the paid model. Runs even when
`last_error` is set.

### 4.13 Streaming: before first byte, mid-stream, client disconnect

Chat completions (`_chat_stream_response`, wb_proxy.py:22349-22572). Headers (200,
`text/event-stream`) are sent **before** the first upstream body read, so an upstream
failure after `open_upstream` returned can never change the HTTP status.
- **Upstream read error** (any non-client exception while iterating): with egress backoff
  on, `wb_egress.note_transient(egress_key)` (no per-request dedup here). If nothing was
  emitted to the client yet and a `reopen` closure exists: close upstream, call
  `open_upstream(..., pin_account=account)` once (pinned: `max_attempts = 1`, same account,
  no rotation; pinned readiness rules and the 2 s slot wait apply); on success recurse
  with `headers_sent=True, retried=True, reopen=None`. Reopen failure or second failure:
  `record_error(502, "stream aborted: …", outcome="upstream_aborted")`, flush buffered
  frames, write `chat_stream_error_frame` (`{"error":{"message","type":"upstream_error",
  "param":null,"code":"upstream_stream_error"}}`) and **no** `[DONE]`
  (wb_proxy.py:12815-12825). The account is **not** charged for a mid-stream failure
  (its penalties were already cleared at header time).
- `emitted` becomes true on the first frame buffered; after that no retry.
- `read1` returning `b""` with `length > 0` remaining raises `IncompleteRead`
  (wb_proxy.py:15336-15340), so truncated Content-Length bodies count as aborts.
- **Client disconnect** (`BrokenPipeError`, `ConnectionResetError`,
  `ConnectionAbortedError`): `record_upstream_usage(..., outcome="client_aborted")` with
  whatever usage was seen; no account or egress change. Such rows are excluded from daily
  counters (§6.1) but `consume_credit` still applies (§6.3).
- Success: egress `note_success` only when something was emitted and the stream was not a
  retry. A stream whose held finish reason was `error` is recorded as `failed`
  (`record_error(502, …)`), without account penalties.
- Empty stream: emits `{"error":{"message":"empty upstream stream","type":"server_error"}}`
  then `[DONE]`.
Non-stream chat (wb_proxy.py:22574-22598): aggregate failure → egress `note_transient`,
`record_error(502)`, client **502** `upstream stream error: …`; no retry, no account charge.
Responses and Anthropic stream handlers have no `reopen` retry (wb_proxy.py:21593-21606,
21978-22029); they record `upstream_aborted` / `client_aborted` the same way. Responses
calls `open_upstream_with_fallback` without `defer_egress_success`, so its egress success
is recorded at header time.

### 4.14 Other HTTP statuses

| Status | Behaviour |
|---|---|
| 400 | re-raised immediately (no demote/charge); client gets `upstream 400: …` |
| other 4xx (404, 409, 413, 422, …) | bare `raise` (wb_proxy.py:14844): immediate, no demote/charge |
| ≥500 | §4.9 |

### 4.15 Summary matrix

| Class | Counters / state changed | Cooldown | Rotate? | Persisted |
|---|---|---|---|---|
| Transport (transient), egress off | `degrade_count+1`, `fails+1` (once/request) | breaker ≥3 fails (1800→21600 s), degrade ≥5 (600→7200 s) | yes, sleep min(0.6·n, 2 s) | no |
| Transport (transient), egress on | egress streak | egress 1→15 s ±25 %, sleep ≤5 s | yes | no |
| Local network | none | none | yes (same pool, sleep min(0.3·n, 1.5 s)) | no |
| TLS verify | none | none | no, stop | no |
| Other exception (e.g. ECONNREFUSED) | `degrade+1`, `fails+1`; `cooldown_until` | 60 s (3 s if ≤1 ready) | yes | no |
| 5xx model-scoped code | none | none | no, stop | no |
| 502/503/504, egress on | egress streak | egress | yes | no |
| other 5xx (or egress off) | `fails+1` | breaker ≥3 | yes | no |
| 401 | refresh once; then `token_expired=True`, `cooldown_until` | 60 s (3 s) + out until refresh | yes (same account once after refresh) | token on refresh success |
| 402 | `balance_until` | next local 04:00 | yes | no |
| 403 content | `PROMPT_DEGRADE` once if passthrough/append | none | retry once (degraded), then stop | no |
| 403 11140 | `restricted_until`, wave event | 7200 s first hit; realm hold 1800 s at ≥2 uids/900 s | no (default `ROTATE_MAX=0`) | `restrictedUntil` |
| 429 credential phrase, no clock | `soft_streak+1`, `cooldown_until` | 600→7200 s | yes | no |
| 429 with reset clock | `model_cooldowns[m]`, limit event, window probe | until reset | yes (or product switch) | `modelCaps` if 6004 and free-cap model |
| 429 soft shape, no clock | `unscoped_streak+1`, `model_cooldowns[m]` | 60→600 s | yes | no |
| 429 other | `model_cooldowns[m]` | 60 s | yes | no |
| 400 / other 4xx | none | none | no | no |
| Stream abort after headers | egress streak (if on) | none | one pinned reopen if nothing emitted | no |
| Client disconnect | none | none | n/a | usage row `client_aborted` |

---

## 5. Concurrency

### 5.1 Layers, in acquisition order

| Layer | Scope | Default | Wait? | Over-limit result | Source |
|---|---|---|---|---|---|
| Per-key | `key_entry.concurrency_limit` (`concurrency_limit` or legacy `max_concurrency`, int ≥0; 0 = unlimited) | 0 | no | HTTP **429** `rate_limit_error`, no `Retry-After` header | wb_proxy.py:22192-22202, 22600-22638; wb_settings.py:519-531 |
| Global chat | `BoundedSemaphore(MAX_CONCURRENT_CHAT)` | 32 | yes, up to 30 s | HTTP **503** `gateway is at its concurrent chat limit (N in flight); retry shortly` | wb_proxy.py:22204-22227 |
| Per-account | `_ACCOUNT_INFLIGHT[uid] < account_max_concurrent` | 0 (off) | no for pool picks; ≤2 s poll for pinned/refresh retry | rotate to next account; all busy and no other error → `RateLimited(wait=1)` → **429** | wb_proxy.py:13604-13694, 14489-14549, 14982-14990 |

The key slot is keyed by key id and only tracked when the limit > 0 (both acquire and
release re-read the entry's limit; a key edited between acquire and release can leak or
under-count — see §8). Panel sessions and unkeyed requests are never limited. The key
and global slots are held for the whole handler, including the full stream body.
`count_tokens` takes a key slot but not a global slot.

`WB_MAX_CONCURRENT_CHAT=auto|pool|dynamic`: at startup `resize_chat_slots(POOL.count_ready())`
sets the ceiling to `max(32, ready)`; it only ever grows and is only called at startup
(wb_proxy.py:389-418, 22805-22808). Non-numeric garbage → 32.

### 5.2 Per-account slot lifecycle

- `account_slot_acquire(account, limit=None)`: limit read from settings each call; ≤0 →
  always True, no bookkeeping.
- Acquired in `pick_account` (pool picks) or the pinned/refresh branches.
- Released at the top of the next loop iteration (failure paths), after the loop
  (`break` paths), or bound to `resp.close()` on success (one-shot guard;
  wb_proxy.py:13650-13688). The slot is held until the response body is closed, so a
  stream occupies its account for the whole stream.
- `busy` accounts are excluded only for the current request, are not charged, and are not
  demoted from affinity.
- Paths that `raise` out of the loop (400, other 4xx, `raise` inside `except
  HTTPError`) skip the release, leaking one slot per such failure while
  `account_max_concurrent > 0` (Python bug; Rust should release on every exit — see §8).

### 5.3 Locks worth preserving semantically

- Pool lock (`RLock`) around snapshot copy, cursor and WRR state; `ready()` runs outside
  it (wb_accounts.py:3685, 4379-4405).
- Per-account `_throttle_lock` for penalty fields; `_refresh_lock` serialises token
  refresh (refresh tokens rotate, so concurrent refresh would burn one);
  `_save_lock` + unique temp name + `os.replace` for credential writes
  (wb_accounts.py:981-1001, 1206-1236); `_balance_lock` for `consume_credit`.

---

## 6. Daily counters, limits and credit deduction

### 6.1 Daily counters (wb_proxy.py:1823-1955)

Source: `usage/usage.jsonl` (append-only, one JSON row per finished request,
wb_proxy.py:929-943). `daily_usage_stats(ttl=15)` folds rows with
`at >= local midnight` into three maps: `tokens[uid] += total_tokens`,
`credits[uid] += credit` (when non-zero), `models[uid][model] += total_tokens`. Rows
with `outcome == "client_aborted"` and rows without `account` are skipped
(wb_proxy.py:1884-1890). Error rows (`record_error`) with usage are counted (they carry
`total_tokens` from a broken stream). Folding is incremental by byte offset; a line
without trailing `\n` is left for the next scan; if the file shrank (`offset > size`) the
state resets. On local date change the offset is kept but totals restart (the midnight
filter drops older unread rows). Read failure → returns None and resets offset to 0;
None means "unknown" and never parks.

`model` in the row is the billed model (`_wb_billed_model` or requested model,
wb_proxy.py:923-926), so the per-model guard counts under the upstream id.

Midnight: `_local_midnight()` = `mktime(local Y, M, D, 0, 0, 0)` (wb_proxy.py:708-715).
Day key `time.strftime("%Y-%m-%d")` (local). Retry-After for daily-limit 429s:
`seconds_until_local_midnight()` = max(60, next local midnight − now)
(wb_proxy.py:1958-1962).

### 6.2 Pushing guards to the pool (wb_proxy.py:2008-2067; wb_accounts.py:4024-4132)

`apply_daily_guards(refresh=False)` runs at the start of every `open_upstream`, at
startup, and with `refresh=True` on settings save (wb_proxy.py:14340, 22809, 19586).
- Limits come from settings `limits.{daily_token_limit, daily_credit_limit,
  model_daily_token_limit}` resolved per realm (`{"global": g, "intl": o or g, "cn": o or
  g}`, override `None` inherits, explicit 0 = off for that realm;
  wb_settings.py:755-918). Flat legacy top-level keys are migrated on first read.
- Counters are folded only if any guard is > 0 for any realm; otherwise `None` is
  passed and accounts keep their last counts.
- A guard is pushed when it is on, or always when `refresh=True` (so turning a guard off
  takes effect immediately). Each account takes the value of its own realm
  (`_realm_limit`, wb_accounts.py:3503-3520).
- Credit guard also pushes `free_models_by_realm()` (wb_proxy.py:3227-3271): ids whose
  catalogue `credits` string is one of `x0.00`, `x0`, `0`, `0.00`, from the bundled
  static catalogue plus the cached remote catalogue file; cached 60 s.
- Per-account fields are memory-only (`daily_*_limit`, `daily_tokens_today`,
  `daily_credits_today`, `model_daily_tokens`, `free_models`).

`reserve_credits` and `expiring_window_days` are pushed at load, on account add, at
startup, and on settings save (wb_accounts.py:3723-3724, 3783-3784; wb_proxy.py:19581,
19588, 22804), not per request.

### 6.3 Credit deduction after a request (wb_accounts.py:1263-1306; wb_proxy.py:879-887)

`record_usage` (success and `client_aborted` rows; **not** `record_error`): if the row's
account exists and `usage.credit` is truthy → `account.consume_credit(credit)`:
`remain = max(0, remain − amount)`, `used += amount`, both rounded to 2 dp; when
`size > 0`, `remain_percent` / `used_percent` strings `"%.1f%%"` with used capped at size.
No-op when `credits` is not a dict or `remain` is not numeric. Memory only; not saved
to the credential file by this call (it reaches disk only on the next unrelated `save()`
since `credits` is a persisted field). The next `fetch_credits` overwrites it.

### 6.4 Credits refresher (wb_accounts.py:3523-3667)

Background thread, tick `interval_seconds = 1800` (min 60). Each tick refreshes the one
`enabled && access_token` account with the oldest `credits.updated_at` (missing = ∞)
whose age ≥ TTL (`credits_refresh_hours`, default 12 h; 0 disables). Failure parks that
uid in the refresher for 6 h (memory). Success may lift the 402 park
(`revive_balance_cooldown`) and refresh `tier` (24 h TTL, wb_accounts.py:352, 3053-3062).

---

## 7. Persisted vs memory-only state

### 7.1 Credential file `accounts/<safe_uid>.json` (wb_accounts.py:1003-1049, 1206-1236)

`safe_uid = re.sub(r"[^A-Za-z0-9_-]", "_", uid).strip("_ ")` (random hex if empty).
Written atomically (`<path>.<pid>.<tid>.tmp` → `os.replace`), JSON indent 2,
`ensure_ascii=False`. `to_dict()` minus `RUNTIME_ONLY_FIELDS = ("lastError",
"cooldownUntil")` (wb_accounts.py:4835).

Persisted fields relevant to dispatch: `uid, realm, domain, product, accessToken,
refreshToken, expiresAt, enabled, tasksEnabled, weight, note, disabledModels, proxySlot,
proxy, restrictedUntil, credits, tier, modelCaps{model: {at, reset}}` (plus identity /
task bookkeeping: `nickname, platform, enterpriseId, addedAt, source, apiKey,
lastCheckin, lastDailyChat, lastActivityReport`).

Load-time rules: `restrictedUntil` float ≥0; `modelCaps` entries dropped unless
`is_free_cap_model(realm, model)` and both `at`, `reset` > 0; `weight` clamped 1..100;
`disabledModels` normalised; `tasksEnabled` defaults to `enabled`; files named
`settings.json`, `active_realm.json`, `model_windows.json`, `quota_scan.json` are not
accounts (wb_accounts.py:3676); unreadable files are logged and skipped; accounts without
uid are skipped.

Writes that happen on the dispatch path: 11140 (`_persist_restriction`), new 6004 cap
(`_save_account_caps`), token refresh success, and admin setters (`set_enabled`,
`set_weight`, `set_note`, `set_disabled_models`, `set_proxy`, `set_proxy_slot`,
`set_all_enabled`, `add`).

### 7.2 Other persisted files

| File | Content | Source |
|---|---|---|
| `accounts/settings.json` | all settings keys referenced here (`api_keys[]` with `realm/models/accounts/concurrency_limit`, `limits`, `model_aliases`, `account_max_concurrent`, `auto_switch_product(s)`, `egress_backoff_enabled`, `realm_fallback_enabled`, `remaining_priority_enabled`, `affinity_stable_key_enabled`, `free_model_credit_fallback`, `upstream`, `prompt`) | wb_settings.py |
| `accounts/active_realm.json` | `{"realm", "updated_at", "updated_iso"}` | wb_proxy.py:6018-6041 |
| `usage/usage.jsonl` | per-request rows (source of daily counters) | wb_proxy.py:929-999 |
| `usage/limit-events.jsonl`, `usage/estimate-daily.json` | remaining-usage estimator inputs | wb_proxy.py:2140-2145 |

### 7.3 Memory-only (reset on restart)

- Account: `cooldown_until`, `balance_until`, `breaker_until`, `degrade_until`,
  `soft_streak`, `unscoped_streak`, `fails`, `degrade_count`, `last_fail_at`,
  `last_soft_at`, `model_cooldowns`, `model_cooldowns_at`, `last_error(_detail)`,
  `token_expired` (recomputed: true iff token empty), `reserve_credits`,
  `expiring_window_days`, all daily guard fields, `free_models`, `is_paid_user` (derived).
  Exception: `model_cooldowns` are re-seeded from persisted live `modelCaps` at load.
- Pool: `_cursor`, `_page_cursors`, `_page_order_cache`, `_manual_weights`,
  `_expiry_weights`, `_remaining_weights`, `_remaining_pick_state`, `affinity`.
- Proxy globals: `_ACCOUNT_INFLIGHT`, `_key_active_concurrency`, `_chat_slots`,
  `_SWITCH_LOG`, `_RESTRICTION_EVENTS`, `_RESTRICTION_HOLD` (the realm pause is lost on
  restart even though per-account `restrictedUntil` survives), `PROMPT_DEGRADE`,
  `wb_egress._state`, `wb_tiers._tiers`, daily-usage fold cache, free-model cache,
  CreditsRefresher `_parked`.

---

## 8. Ambiguities, quirks and porting decisions

1. **Slot leak on raised 4xx** (§5.2): `raise` at wb_proxy.py:14804 and 14844 bypasses the
   post-loop release; with `account_max_concurrent > 0` each such failure permanently
   consumes a slot. Recommend: release on every exit in Rust.
2. **Key slot release re-reads the limit** (wb_proxy.py:22623-22638): if a key's limit is
   changed from N>0 to 0 mid-request the counter is never decremented, and vice versa.
   Recommend: capture "was counted" at acquire.
3. **11140 surfaces as `ContentRejected` (403)**: the final error class name says
   "content" although it is an account restriction; status and body are the upstream's.
   Keep 403 + upstream body for client compatibility.
4. **`note_success` at header time** (wb_proxy.py:14620) clears breaker/restriction
   before the stream proves healthy; mid-stream aborts are never charged to the account.
   Faithful port keeps this; flag for later review.
5. **`note_error(model=...)` overwrites** the model window (assignment, not `max`). In
   practice an account cooling a model is not picked for it, so a shorter overwrite needs
   a pinned request or a race between concurrent requests. 6004 bodies re-assert the
   longer window via `note_model_cap`. Account-level `note_error` likewise overwrites
   `cooldown_until`, while `note_soft_rate` uses `max`.
6. **`affinity_stable_key_enabled`** hashes `messages[:3]` only; the settings comment
   claims 4 messages. Code is authoritative.
7. **Affinity key serialisation** must match Python `json.dumps(sort_keys=True,
   ensure_ascii=False)` byte-for-byte to keep keys stable across the port (optional;
   a mismatch only re-binds conversations once).
8. **Shared rotation cursor** across realms/models (§3.6): port as-is.
9. **`max_attempts` counts all iterations** including refresh/degrade retries and
   realm-mismatch skips; with `total` computed once from `count_ready` at entry.
10. **`count_ready` and `ready()` have side effects** (token refresh + file write); Rust
    may refresh asynchronously but must keep "near-expiry account is unusable unless the
    refresh succeeds".
11. **Realm fallback** checks `count_ready("cn")` without the key's `allowed` set.
12. **`X-Realm` / `?realm=` are not validated**; an unknown value yields 503
    "no usable account". Consider validating to `intl|cn` (behaviour change).
13. **`detect_model_realm`** is reachable only when `target_realm` is empty, which the
    three chat handlers never pass; it still matters for internal callers
    (`follow_up_with_tool_results` passes `holder["realm"]`).
14. **Daily counters include error rows with partial usage** but exclude
    `client_aborted`; `consume_credit` includes `client_aborted` but excludes error rows.
    The two ledgers can drift; port faithfully.
15. **Timezones**: daily limits and 402 park use host local time; prompt degrade uses fixed
    UTC+8; 6004 reset stamps carry their own UTC offset. Keep all three.
16. **Windows-only errno/winerror codes** in transient/local-network detection are inert
    on Linux; Rust should classify by `std::io::ErrorKind` + the same text markers.
17. **`reset_restriction_state()`** has no HTTP route in this tree; the realm pause ends
    only by time or restart.
18. **Product auto-switch log** is cleared wholesale above 500 entries (resets every
    account's 4-switch budget).
