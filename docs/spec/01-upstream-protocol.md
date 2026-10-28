# 01 — Upstream wire protocol (WorkBuddy intl / CodeBuddy CN)

Source of truth: Python tree at tag `py-baseline-1.7.32`. Every item cites
`file:line`. "Byte-compatible" below means: identical to what the Python gateway
puts on the wire today (verified for chat headers by capturing the pooled path
against a local socket listener, see §4.1). Where the Python gateway itself is
inconsistent between call sites, that is called out as **[INCONSISTENT]**; where
the code does not determine the answer, **[AMBIGUOUS]**.

Abbreviations: `A` = `wb_accounts.py`, `I` = `wb_identity.py`, `F` =
`wb_fingerprint.py`, `UP` = `wb_upstream_pool.py`, `P` = `wb_proxy.py`,
`T` = `wb_tasks.py`, `E` = `wb_egress.py`, `IP` = `wb_ipintel.py`,
`TI` = `wb_tiers.py`, `S` = `wb_settings.py`, `SC` = `wb_scheduler.py`.

---

## 1. Realms, base URLs, endpoints

### 1.1 Realm config (`A:267-298`)

| key | `intl` | `cn` |
|---|---|---|
| `chat_upstream` | `https://www.workbuddy.ai` | `I.CN_CHAT_BASE` = `https://copilot.tencent.com` (overridable, see below) |
| `billing_upstream` | `https://www.workbuddy.ai` | `https://www.codebuddy.cn` |
| `origin` (Origin/Referer) | `https://www.workbuddy.ai` | `https://www.codebuddy.cn` |
| `domain` (X-Domain fallback) | `www.workbuddy.ai` | `copilot.tencent.com` |
| `chat_ua` | `WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0` | `WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0` |
| `billing_ua` | `WorkBuddy/5.8.4` | `WorkBuddy/5.8.3` |
| `has_checkin` | false | true |

`WB_CN_CHAT_UPSTREAM` env (read once at import, `I:268-289`): if set and it is
`http(s)://host...`, trailing `/` stripped, it replaces `CN_CHAT_BASE`;
otherwise ignored (default kept). It does **not** change X-Domain.

`get_realm_config(realm)` falls back to `intl` for unknown realm (`A:392`).

Note: `REALM_CONFIGS[*].chat_ua` uses the *default* version strings at import
time; `I.desktop_ua(realm)` (used for chat identity headers) honours the panel
override `identity_versions` (`I:204-228`). So `/v3/config`, refresh, login and
billing UAs never follow the override. **[INCONSISTENT]** but intentional-looking.

### 1.2 Chat endpoint per (realm, product) (`I:293-315`)

`endpoint_for(realm, product) -> (base, X-Domain-fallback)`:

| realm | product `workbuddy` (desktop) | `vscode` | `cli` |
|---|---|---|---|
| intl | `https://www.workbuddy.ai`, `www.workbuddy.ai` | same | same |
| cn | `CN_CHAT_BASE`, `copilot.tencent.com` | `https://www.workbuddy.cn`, `www.workbuddy.cn` | `CN_CHAT_BASE`, `copilot.tencent.com` |

Chat path: `CHAT_PATH = "/v2/chat/completions"` (`P:286`). URL =
`Account.chat_base_url() + CHAT_PATH` (`A:1810`, `P:14570`).

Note: in chat identity headers X-Domain is taken from `endpoint_for(...)[1]`
(`I:511,548,590`), NOT from the account's stored `domain`. The stored `domain`
is used only by billing/refresh headers (`A:1698,1843`).

### 1.3 Full endpoint inventory

Methods/bases: `CHAT` = account's chat base (§1.2); `CU` = realm `chat_upstream`;
`BU` = realm `billing_upstream`; `CNC` = `CN_CHAT_BASE`; `CNB` =
`https://www.codebuddy.cn`; `CNW` = `https://www.workbuddy.cn`; `WEB` =
`https://www.workbuddy.ai`.

Core (detailed in later sections):

| # | method | URL | caller | purpose |
|---|---|---|---|---|
| 1 | POST | `CHAT/v2/chat/completions` | `P:14613` open_upstream; `P:15197` probe_account; `A:2399` daily chat; `wb_window_probe.py:352` probe | chat (SSE) |
| 2 | POST | `CU/v2/plugin/auth/token/refresh` | `A:1832` | token refresh |
| 3 | POST | `CU/v2/plugin/auth/state?platform=<CLI>` | `A:4518` | OAuth: get state + authUrl |
| 4 | GET | `CU/v2/plugin/auth/token?state=<s>` | `A:4549` | OAuth: poll |
| 5 | GET | `CU/v2/plugin/login/account?state=<s>` | `A:4574` | OAuth: nickname |
| 6 | GET | `CU/v3/config` | `P:11249` | model catalogue + `modelTiers` |
| 7 | GET | `https://www.workbuddy.ai/v2/enterprises/personal/models` | `P:11490` | legacy model list (intl fallback) |
| 8 | POST | `BU/v2/billing/meter/get-user-resource` | `A:2796` | credits (intl, and CN fallback) |
| 9 | POST | `BU/billing/meter/get-user-resource-summary` | `A:2637,3043` | packages summary + tier (`IsPaidUser`) |
| 10 | POST | `BU/billing/meter/get-user-resource-free-packages` | `A:2679` | CN free packages |
| 11 | POST | `BU/billing/meter/get-user-resource-paid-packages` | `A:2691` | CN paid packages |
| 12 | POST | `CU/v2/billing/meter/checkin-activity-status` | `A:2703` | CN checkin status (note: chat base, not billing) |
| 13 | POST | `BU/billing/meter/get-enterprise-user-usage` | `A:2905-2907` | enterprise OAuth credits |
| 14 | POST | `CNB/v2/billing/meter/get-enterprise-user-usage` | `A:1183,2905` | static API key credits / probe (needs `/v2`; without it APISIX returns 401 HTML) |
| 15 | POST | `CNB/v2/billing/meter/daily-checkin` | `A:2472` | CN daily checkin |

Secondary (list only; CN growth programme, web console, exit probing):

| method | URL | caller | purpose |
|---|---|---|---|
| GET | `CNC/v2/activity/growth/tasks` | `T:120` | growth task list |
| POST | `CNC/v2/activity/growth/tasks/accept` body `{"task_codes":[...]}` | `T:226` | accept tasks (chunks of 20) |
| POST | `CNC/activity/growth/tasks/{code}/claim` body `{}` | `T:280` | claim; on HTTP 400 retried at `CNW/activity/growth/tasks/{code}/claim` with browser headers (`T:298-313`) |
| POST | `CNC/v2/report` body `[event,...]` | `T:408` | event report (growth / activity streak) |
| GET | `CNC/v2/activity/growth/energy` | `T:156` | energy balance |
| GET | `CNC/activity/growth/streak` | `T:164,204` | streak days (`data.streak.days`) |
| GET | `CNC/activity/growth/lottery/summary`; POST `.../lottery/draw` | `T:181,882` | lottery |
| GET | `CNC/activity/growth/buddy/travel/status`, `/config`; POST `/depart`, `/claim` | `T:189,421,449,486,503` | cat travel |
| GET | `CNC/activity/growth/heatmap`; POST `/activity/growth/makeup-cards/use`, `/activity/growth/redeem` | `T:806,820,860` | streak makeup / redeem |
| POST | `CNB/billing/meter/claim-gift`, `CNB/billing/meter/claim-compensation` body `{}` | `T:829-832` | one-off gifts |
| GET | `{WEB or CNW}/console/account` | `A:155-183` | web profile (uid, nickname only) |
| POST/GET | `WEB/console/as/conversations/` (+`{id}`, `{id}/session`, `{id}/stop`, `{id}/delete`, `?page=1&pageSize=N`) | `A:2214-2350` | intl "daily activity" web agent channel (plus ACP over HTTP+SSE in `wb_webagent.py`) |
| GET | `https://api.ipify.org` (through proxy) | `P:6331` | proxy exit IP |
| GET | `http://ip-api.com/json/<ip>?lang=zh-CN&fields=...` (direct) | `IP:23-94` | exit geo / hosting |

The growth/web-console/ACP channel is reward farming, not request serving; the
implementer may treat it as optional. Its header sets are in §4.6.

---

## 2. Account credential file (`accounts/*.json`)

### 2.1 Location and naming

- Directory = accounts dir (`ACCOUNTS_DIR`, Docker `/app/accounts`).
- Loader: every `*.json` in the dir, sorted by name, except
  `settings.json`, `active_realm.json`, `model_windows.json`, `quota_scan.json`
  (`A:3676,3708-3728`). Unreadable/invalid JSON → logged, skipped. An account
  with empty `uid` after derivation → skipped (`A:3721`).
- File name on save: `re.sub(r"[^A-Za-z0-9_-]", "_", uid).strip("_ ") + ".json"`;
  if that is empty, `uuid4().hex + ".json"`. Path must stay inside the dir
  (`A:1206-1212`). Note: the loaded file's own name is irrelevant; the next
  save writes to the uid-derived name (and updates `self.path`), so a file
  named differently is left behind as a duplicate. **[AMBIGUOUS]** whether
  intentional; replicate.

### 2.2 Write semantics (`A:1206-1236`)

- Per-account `_save_lock`.
- Temp file `"%s.%d.%d.tmp" % (final_path, pid, thread_id)`, then
  `os.replace(tmp, final)` (atomic rename). On error the temp is unlinked and
  the exception re-raised.
- Content: `json.dump(to_dict() minus {"lastError","cooldownUntil"},
  ensure_ascii=False, indent=2)`, UTF-8, no trailing newline.
- **No field preservation**: the file is fully regenerated from the in-memory
  model; unknown keys in the original file are dropped on first rewrite.
- No fsync.
- Rewrites happen after: successful refresh (`A:1874`), credits fetch
  (`A:2781,2866,2973`), tier refresh (`A:3060`), checkin success (`A:2481`),
  daily chat (`A:2441`), activity report (`A:2528`), restriction (`P:14152`),
  6004 caps, nickname sync, pool add/import, panel edits.

### 2.3 Fields (`A:748-1049`)

Key order in written file = order of `to_dict()` (`A:1010-1049`):

| key | type | load rule | notes |
|---|---|---|---|
| `uid` | str | `data.uid` stripped; else (API key) `"apikey-"+api_key_id(token)`; else JWT `sub` | required in practice |
| `nickname` | str | kept only if `str` and not containing `$wbEncrypted`; else `""` | |
| `domain` | str | as stored; if empty → realm default domain | used for billing/refresh X-Domain |
| `realm` | `"intl"`/`"cn"` | stored, else detected from token `iss`/domain (`A:531-551`); API key forces `cn` | |
| `platform` | str | default `"CLI"` | used as `platform=` in OAuth state |
| `product` | `"workbuddy"`/`"vscode"`/`"cli"` | `I.normalize_product` (aliases `wb,desktop`/`vsc,ide`/`codebuddy`; unknown → `workbuddy`) | outbound identity |
| `enterpriseId` | str | stored, else JWT claim `enterprise_id` | |
| `accessToken` | str | **the only truly required field** | JWT or `ck_`/`pt_` static key |
| `refreshToken` | str | default `""` | |
| `expiresAt` | int epoch s | `normalize_epoch(stored)` (ms→s if >1e11) or JWT `exp`; API key → 0 | 0 = never expires by time |
| `addedAt` | float | default `time.time()` | |
| `source` | str | default `"oauth"` / `"api-key"` | `oauth`, `import`, `cockpit`, `desktop-app`, `api-key` |
| `apiKey` | bool | written only (derived from token prefix) | |
| `proxySlot` | str | slot id bound in settings `proxy_slots` | |
| `proxy` | str | legacy literal proxy URL | runtime proxy = slot url if slot enabled, else this (`A:3946-3963`) |
| `enabled` | bool | default true | API dispatch |
| `tasksEnabled` | bool | default = `enabled` | |
| `weight` | int 1..100 | default 1 | |
| `note` | str ≤200 | | |
| `disabledModels` | [str] | lowercased, dedup | |
| `restrictedUntil` | float epoch | persisted 11140 cooldown | |
| `credits` | object/null | last credits snapshot (§8.4 shape) | |
| `tier` | object/null | `{paid, package, package_code, protected_price, trial, checked_at, source:"billing"}` | |
| `lastCheckin`, `lastDailyChat`, `lastActivityReport` | `"YYYY-MM-DD HH:MM:SS"` local / null | | |
| `modelCaps` | `{model: {"at": epoch, "reset": epoch}}` | only free-cap models kept (`A:464-467,927-941`) | |

Runtime-only (never persisted): `lastError`, `cooldownUntil`, model cooldowns,
streaks, breaker/degrade/balance windows, `token_expired`.

JWT claims read (payload decoded base64url without signature check,
`A:423-441`): `sub` (uid), `exp`, `iss` (realm markers
`copilot.tencent.com|codebuddy.cn|workbuddy.cn` → cn,
`workbuddy.ai|codebuddy.ai` → intl, `A:456-458`), `enterprise_id`.

Static API keys: prefix `ck_` or `pt_` (`A:319`); `api_key_id` = text after
prefix up to first `.`, filtered to `[A-Za-z0-9_-]` (`A:326-338`); never
refreshed (`A:1822`), realm forced `cn`.

### 2.4 Redacted example (as written by `save()`)

```json
{
  "uid": "d3f1c2a4-0000-0000-0000-000000000000",
  "nickname": "user1",
  "domain": "www.workbuddy.ai",
  "realm": "intl",
  "platform": "CLI",
  "product": "workbuddy",
  "enterpriseId": "",
  "accessToken": "eyJhbGciOi...<redacted>...",
  "refreshToken": "<redacted>",
  "expiresAt": 1793000000,
  "addedAt": 1790000000.123,
  "source": "oauth",
  "apiKey": false,
  "proxySlot": "slot-1",
  "proxy": "",
  "enabled": true,
  "tasksEnabled": true,
  "weight": 1,
  "note": "",
  "disabledModels": [],
  "restrictedUntil": 0.0,
  "credits": {
    "remain": 812.5, "used": 187.5, "size": 1000.0,
    "used_percent": "18.8%", "remain_percent": "81.2%",
    "is_paid_user": false, "checkin": null,
    "earliest_expiring": null, "packages": [],
    "updated_at": 1790001234.5, "updated_iso": "2026-10-22 10:00:00"
  },
  "tier": {"paid": false, "package": "Free", "package_code": "",
           "protected_price": false, "trial": 0,
           "checked_at": 1790001234.5, "source": "billing"},
  "lastCheckin": null,
  "lastDailyChat": "2026-10-22 09:00:12",
  "lastActivityReport": null,
  "modelCaps": {}
}
```

Import formats accepted (`A:4878-5009`): export doc `{"format":"workbuddy-accounts","version":1,"accounts":[...]}`,
bare list, single object, desktop credential `{"auth":{...},"account":{...}}`,
cockpit snake_case rows (`access_token`, `refresh_token`, `expires_at` ms).
Non-API-key tokens must have exactly two dots (`A:4954`).

---

## 3. Identity, device fingerprint, IDs

### 3.1 Per-account stable IDs (`F:22-42`)

```text
derive_id(uid, salt):
  seed = salt + ":" + (uid or "anonymous")          # UTF-8
  h    = md5(seed).hexdigest()                       # 32 lowercase hex
  h    = h[0:12] + "4" + h[13:16] + "a" + h[17:32]   # force v4 nibble and variant 'a'
  return h[0:8]-h[8:12]-h[12:16]-h[16:20]-h[20:32]

machine_id(uid) = derive_id(uid, "machine")
session_id(uid) = derive_id(uid, "session")
```

Test vectors (computed with the Python code):

```text
derive_id("u-123","machine") = 98b53497-f10c-4475-a88a-9054ddd676f0   (md5 98b53497f10c5475f88a9054ddd676f0)
derive_id("u-123","session") = 0ff5c5a0-043c-464b-aad4-e1557ee75825
derive_id("","machine")      = fc1f3f60-3c8b-4dc0-a839-42ea0169ab1e   (seed "machine:anonymous")
```

Used only on **billing/non-chat** headers as `X-Machine-ID` / `X-Session-ID`
(`A:1702-1703`). Chat requests do NOT send them (`A:1663-1665`).
`get_desktop_fingerprint()` (`F:54-80`) is dead code (no callers).

### 3.2 Per-request IDs

```text
uuid7():                                   # I:318-329, RFC 9562 layout
  ms     = now_ms & (2^48-1)
  rand_a = random 12 bits; rand_b = random 62 bits (secrets)
  int    = ms<<80 | 0x7<<76 | rand_a<<64 | 0b10<<62 | rand_b
new_conversation_id() = str(uuid7())        # hyphenated 36-char
new_message_id()      = uuid7().hex         # 32 hex, no hyphens
new_turn_request_id() = new_message_id()
generate_request_id(uid) = uuid4().hex      # F:45-51, billing X-Request-ID (32 hex, v4)
new_request_trace_id() = str(uuid4())       # I:421-426, VSCode X-Request-Trace-Id (hyphenated)
span id               = secrets.token_hex(8)  # 16 hex
```

Stable conversation id (`I:337-357`):

```text
conversation_id_for(uid, session_key):
  if session_key.strip() == "": return ""
  d = sha256((uid or "") + "|" + session_key.strip()).hexdigest()[0:32]
  d[12] = '4'; d[16] = 'a'
  return d[0:8]-d[8:12]-d[12:16]-d[16:20]-d[20:32]
# conversation_id_for("u-123","pfx-0123456789abcdef") = c26f60d7-a3c6-4ac1-a98a-d43ea8b5b6a1
```

Session key used for chat (`P:15266-15278`, `P:14381-14386`, `P:10306-10357`):
first non-empty of request headers `X-Conversation-Id`, `Conversation-Id`,
`X-Session-Id`, `Session-Id`, body `conversation_id`, `session_id`,
`metadata.conversation_id`. If none: affinity key derived from the
**already-built upstream** messages:

```text
head = messages[0:2]   (messages[0:3] if affinity_stable_key setting and len>2)
key  = "pfx-" + sha256(json.dumps(head, ensure_ascii=False, sort_keys=True)).hexdigest()[0:16]
# json.dumps default separators (", ", ": ") — must be replicated for identical keys
```

Disabled with `WB_AFFINITY_BY_PREFIX=0`; for conversations longer than
`WB_AFFINITY_MAX_MSGS` (400) a page `(int(digest[:8],16) % 3, 3)` is used
(`WB_AFFINITY_PAGE_SIZE`, default 3) (`P:10271-10305`).

X-Conversation-ID selection in `open_upstream` (`P:14601-14606`, `I:483-497`):

| product | value |
|---|---|
| vscode | one `new_message_id()` (32 hex) per `open_upstream` call, reused across retries/accounts (`P:14478`) |
| workbuddy / cli, session_key present | `conversation_id_for(account.uid, session_key)` (hyphenated, stable) |
| workbuddy, no session_key | `new_conversation_id()` per attempt (uuid7 hyphenated) |
| cli, no session_key | `conversation_id_for` returns "" → `new_conversation_id()` |

Other callers (`probe_account`, daily chat, window probe, tasks) pass no
conversation id → fresh per request.

Turn/message ids (`I:486-497`, `P:14391`):

- `X-Conversation-Message-ID` = fresh `new_message_id()` per attempt.
- `X-Conversation-Request-ID`:
  - cli: the `turn_request_id` generated once per `open_upstream` (outside the retry loop);
  - workbuddy / vscode: fresh `new_message_id()` per attempt (`turn_request_id` ignored).
- `X-Root-Request-ID` (workbuddy, cli only) = `X-Conversation-Request-ID`.
- `X-Request-ID`: workbuddy/cli = message id; vscode = conversation-request id.

Trace headers:

```text
trace_headers(with_parent=False)   # I:396-418   (workbuddy: False; cli: True)
  trace = new_message_id(); span = token_hex(8)
  X-Trace-ID: trace
  traceparent: 00-<trace>-<span>-01
  b3: <trace>-<span>-1            # with_parent: <trace>-<span>-1-<parent>
  X-B3-TraceId: trace
  X-B3-SpanId: span
  X-B3-Sampled: 1
  [X-B3-ParentSpanId: parent]      # appended LAST when with_parent (dict insertion order)

vscode_trace_headers()             # I:429-450, no traceparent
  X-Trace-ID, b3 (4-part), X-B3-TraceId, X-B3-ParentSpanId, X-B3-SpanId, X-B3-Sampled: 1
```

### 3.3 User-Agent / version strings (`I:68-244`)

Defaults (overridable by settings `identity_versions` keys
`desktop_intl, desktop_cn, desktop_cli, vscode_product, vscode_ide, cli, sdk, node`,
values must match `^[0-9A-Za-z][0-9A-Za-z._-]{0,63}$`, `S:1868-1873`):

| key | default |
|---|---|
| desktop_intl | `5.8.4` |
| desktop_cn | `5.8.3` |
| desktop_cli | `2.161.0` |
| vscode_product | `4.12.39997873` |
| vscode_ide | `1.119.0` (env `WB_VSCODE_IDE_VERSION` if no panel override) |
| cli | `2.168.2` |
| sdk | `6.25.0` |
| node | `v22.21.1` (precedence: panel override > `WB_IDENTITY_NODE_VERSION` > default) |

```text
desktop UA intl: "WorkBuddy/{desktop_intl} workbuddy-ai/{desktop_intl} CLI/{desktop_cli}"
desktop UA cn:   "WorkBuddy/{desktop_cn} WorkBuddy/{desktop_cn} CLI/{desktop_cli}"
vscode UA:       "VSCode/{vscode_ide} CodeBuddy/{vscode_product}"
cli UA:          "CLI/{cli} CodeBuddy/{cli}"
billing UA:      "WorkBuddy/5.8.4" (intl) | "WorkBuddy/5.8.3" (cn)   (constant, no override)
web UA (console): Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0   (A:46)
web UA (T claim fallback): ... Chrome/152.0.0.0 Safari/537.36   (T:25)
```

CLI `x-stainless-os`: env `WB_IDENTITY_OS` else map `platform.system()`
`Windows→Windows, Darwin→MacOS, Linux→Linux` (`I:99-105`). `x-stainless-arch`:
env `WB_IDENTITY_ARCH` else map `x86_64/amd64→x64, aarch64/arm64→arm64,
i386/i686→ia32` (`I:108-115`). On a Linux VPS: `Linux` / `x64`.

### 3.4 X-Device-Token

Source (`A:584-616`): settings `upstream.device_token` literal, else contents of
file `upstream.device_token_file` (≤1024 bytes, stripped); cached 300 s. Sent
on chat only if realm == `cn` and product != cli (`A:1756-1760`); on billing if
product != cli (`A:1717-1719`, **any realm**, **[INCONSISTENT]** with chat).
If no token and product == vscode and no enterprise id → chat header
`X-Device-Token-Error: sdk_not_initialized` (`A:1761-1767`).

---

## 4. Chat request to upstream

### 4.1 Header-name fidelity: two serialization paths (`UP:633-692`, `A:210-233`)

All upstream HTTP goes through `wb_accounts.urlopen(req, timeout, proxy)` →
`wb_upstream_pool.urlopen` (unless `WB_UPSTREAM_KEEPALIVE` ∈ `0,off,false,no`).

- **Exact path**: when the caller sets `req.wb_exact_headers = list(headers.items())`
  the pool writes those names verbatim, in that order. Used by:
  `open_upstream` (`P:14615`), `probe_account` (`P:15199`), daily chat
  (`A:2402`). **Not** used by the window probe / quota scan (`wb_window_probe.py:352`)
  → those chat requests go out title-cased. **[INCONSISTENT]**
- **Title path** (everything else: refresh, OAuth, billing, `/v3/config`,
  tasks): names are urllib-`capitalize()`d then `.title()`d, e.g.
  `X-CodeBuddy-Request`→`X-Codebuddy-Request`, `X-Request-ID`→`X-Request-Id`,
  `X-IDE-Type`→`X-Ide-Type`, `traceparent`→`Traceparent`, `b3`→`B3`.

Wire layout produced by both paths (http.client, HTTP/1.1):

```text
<METHOD> <path?query> HTTP/1.1
Accept-Encoding: identity          # always, emitted by http.client.putrequest
Content-Length: <n>                 # only when body present (bytes body)
Host: <netloc of URL>               # e.g. www.workbuddy.ai (no :443)
<caller headers in insertion order>
[Proxy-Authorization: Basic ...]    # only plain-http target via proxy
Connection: keep-alive
```

If the caller supplies no `User-Agent`, `User-agent: Python-urllib/3.x` is inserted
after Host (`UP:675-678`); all call sites supply one. A body without
Content-Type would get `application/x-www-form-urlencoded` (`UP:666-669`); all
POST sites supply `application/json`. Header values are sent as-is
(latin-1). No `Transfer-Encoding: chunked` request bodies.

Because `Accept-Encoding: identity` is sent, upstream responses are
uncompressed; no response decompression exists anywhere.

### 4.2 Chat header sets, exact order (captured from the pooled exact path)

Composition (`A:1652-1768`): `base` dict, then `.update(identity)` from
`I.build_identity_headers`, then device-token header, then (cli/vscode in
`open_upstream`/`probe_account`) `Content-Encoding: gzip` appended last
(`P:14611-14612`). Python dict `update` keeps first-insertion position for
duplicate keys; no keys overlap here except `Content-Type`/`Accept` which are
only in base.

Base for workbuddy / vscode (`A:1744-1752`):

```text
Content-Type: application/json
Accept: application/json, text/plain, */*
X-Requested-With: XMLHttpRequest
Origin: {realm origin}                       # https://www.workbuddy.ai | https://www.codebuddy.cn
Referer: {realm origin}/
X-CodeBuddy-Request: 1
Accept-Language: en-US | zh-CN               # intl | cn
```

Base for cli (`A:1739-1742`):

```text
Content-Type: application/json
Accept: application/json
```

**workbuddy (desktop) identity** (`I:499-521`), then trace (no parent):

```text
X-Agent-Intent: craft
X-Agent-Purpose: conversation
X-Agent-Type: main
X-IDE-Name: WorkBuddy
X-IDE-Type: WorkBuddy
X-IDE-Version: 5.8.4 | 5.8.3
X-Private-Data: true
X-Product: SaaS
X-Domain: www.workbuddy.ai | copilot.tencent.com
User-Agent: WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0 | WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0
X-User-Id: {uid or "anonymous"}
Authorization: Bearer {accessToken}
X-Conversation-ID: {hyphenated uuid}
X-Conversation-Request-ID: {32hex}
X-Root-Request-ID: {same as Conversation-Request-ID}
X-Conversation-Message-ID: {32hex}
X-Request-ID: {same as Message-ID}
X-Trace-ID: {32hex}
traceparent: 00-{trace}-{span16}-01
b3: {trace}-{span16}-1
X-B3-TraceId: {trace}
X-B3-SpanId: {span16}
X-B3-Sampled: 1
```

**vscode identity** (`I:537-571`):

```text
X-Agent-Intent: craft
X-Agent-Type: main
X-IDE-Type: VSCode
X-IDE-Name: VSCode
X-IDE-Version: 1.119.0
X-Product-Version: 4.12.39997873
X-Env-ID: production
X-Product: SaaS
X-Domain: www.workbuddy.ai | www.workbuddy.cn
User-Agent: VSCode/1.119.0 CodeBuddy/4.12.39997873
X-User-Id: {uid}
Authorization: Bearer {token}
X-Request-Trace-Id: {hyphenated uuid4}
X-Conversation-ID: {32hex}
X-Conversation-Request-ID: {32hex R}
X-Conversation-Message-ID: {32hex}
X-Request-ID: {R}
X-Model-ID: {model}                              # only if model given
monitor_promptPrepareStartTime: {epoch ms}       # only if model given
monitor_httpSendTime: {same epoch ms}            # only if model given
X-Private-Data: true                             # intl only (omitted on cn unless override)
X-Trace-ID: {trace}
b3: {trace}-{span}-1-{parent}
X-B3-TraceId: {trace}
X-B3-ParentSpanId: {parent}
X-B3-SpanId: {span}
X-B3-Sampled: 1
```

**cli identity** (`I:579-605`):

```text
X-Agent-Intent: craft
X-Agent-Purpose: conversation
X-Agent-Runtime: other
X-Agent-Type: main
X-IDE-Type: CLI
X-IDE-Name: CLI
X-IDE-Version: 2.168.2
X-Product: SaaS
X-Domain: www.workbuddy.ai | copilot.tencent.com
User-Agent: CLI/2.168.2 CodeBuddy/2.168.2
X-User-Id: {uid}
Authorization: Bearer {token}
X-Conversation-ID: {hyphenated}
X-Conversation-Request-ID: {turn id}
X-Root-Request-ID: {turn id}
X-Conversation-Message-ID: {32hex}
X-Request-ID: {same as Message-ID}
X-Private-Data: false | true                     # true iff enterpriseId non-empty
x-stainless-lang: js
x-stainless-package-version: 6.25.0
x-stainless-os: Linux
x-stainless-arch: x64
x-stainless-runtime: node
x-stainless-runtime-version: v22.21.1
x-stainless-retry-count: 0
x-stainless-timeout: 1200
X-Trace-ID: {trace}
traceparent: 00-{trace}-{span}-01
b3: {trace}-{span}-1-{parent}
X-B3-TraceId: {trace}
X-B3-SpanId: {span}
X-B3-Sampled: 1
X-B3-ParentSpanId: {parent}
```

Conditional tail, appended in this order after the identity block
(`I:611-625`, then `A:1756-1767`, then `P:14611`):

```text
X-API-Key: {token}                    # token starts with ck_ / pt_
X-Enterprise-Id: {enterpriseId}       # if enterpriseId
X-Tenant-Id: {enterpriseId}           # if enterpriseId (tenant_id passed = enterprise_id, A:1729)
X-Device-Token: {token}               # cn && product != cli && token configured
X-Device-Token-Error: sdk_not_initialized   # else if vscode && !enterpriseId
Content-Encoding: gzip                # cli or vscode (open_upstream / probe_account)
```

`X-Private-Data` override (`I:620-625`, `S:1859-1865`): settings
`identity.private_data` ∈ `auto|true|false`; `true`/`false` overwrites the value
**in its existing position**, or, when the identity had none (vscode cn),
appends it after `X-API-Key` / `X-Enterprise-Id` / `X-Tenant-Id` and before the
device-token and `Content-Encoding` headers (`I:611-627`).
`X-Department-Info` is never sent (department never passed).

Not sent on chat: `X-Machine-ID`, `X-Session-ID`, `X-No-Enterprise-Id`
(billing only).

### 4.3 Body transformations (`P:13145-13251`, `P:14571-14593`)

`build_upstream_body(payload)` — client OpenAI chat body → upstream body:

1. `map_reasoning_effort` (if settings `reasoning_effort_map` on): replace an
   unsupported `reasoning_effort`/`reasoningEffort` value with catalogue default
   (`P:13332-13391`).
2. Thinking state for `deepseek*` models: enabled if `thinking.type=="enabled"`,
   or (type != "disabled" and effort != "none").
3. `messages`: role `developer` → `system` (`P:11604-11620`); prompt-mode
   (passthrough default; custom/append/degraded via `wb_prompt`); sanitize
   (fingerprint rewrites in system/developer/assistant text, tool_call
   `arguments` non-string → `json.dumps`, `None` → `"{}"`) (`P:11733-11867`);
   reasoning backfill for deepseek (every assistant gets string
   `reasoning_content`, and `reasoning` set to it or `" "`) (`P:12143-12181`).
4. If first message is not `system`, prepend
   `{"role":"system","content": SYSTEM_PROMPT}`; default `"You are a helpful assistant."`
   (`P:288`, overridable by CLI arg `--system-prompt`, `P:22775`).
5. `body = dict(payload)` (client key order kept), drop keys starting with `_`,
   set `model`, `messages` (in place).
6. Tool-call repair: merge adjacent assistant tool_calls, repack tool results,
   drop orphan tool calls (`P:11902-12089`).
7. `max_completion_tokens` → `max_tokens` (only if `max_tokens` absent and >0)
   (`P:12836-12847`); `gpt-*` with `max_tokens < 16` → 16 (`P:13196-13203`).
8. `tool_choice`: string kept (lowercased `none` → `"none"`, other strings
   verbatim); object `{type:none|auto|required}` → that string;
   `{type:function,function:{name}}` → the bare name string (or `"auto"`);
   any other shape → removed (`P:12185-12225`).
9. `tools`: root `functions` → `tools`; bare `{name,...}` wrapped as
   `{"type":"function","function":{...}}`; each `function.parameters` forced to
   `{"type":"object","properties":{...}}`, schema type aliases mapped
   (`str/text→string`, `int→integer`, `bool→boolean`, `dict→object`,
   `list→array`, `float→number`), null `description/title/format/pattern/items/default`
   removed (`P:12229-12397`). Empty tools → key removed.
10. `stop`: string → `[s]`; list filtered to non-empty strings; empty → removed
    (`P:12401-12424`).
11. `max_tokens` absent → catalogue `maxOutputTokens` if known (`P:13207-13210`).
12. deepseek, not opted out: `thinking={"type":"enabled"}` if absent; if no
    effort and `WB_DEFAULT_REASONING_EFFORT` ≠ off: catalogue `defaultEffort`,
    else `"high"` in mode `high` (the default) (`P:13220-13235`, `P:13394-13409`).
13. `stream = true` (always); `stream_options = {"include_usage": true}` if absent
    (`P:13236-13238`).

Per attempt in `open_upstream` (`P:14571-14593`):

- Optional `prompt_cache_key = "wb2a-" + uid[:8] + "-" + sha256(uid + "|" + conv)[:32]`
  only when `WB_PROMPT_CACHE_KEY=1` (`P:13429-13473`).
- cli: remove `metadata`, `conversation_id`, `previous_response_id`; set
  `cache_retention = "long"` (appended at the end unless already present)
  (`I:143-146`).
- Serialize: `json.dumps(body, ensure_ascii=False, separators=(",",":"))`, UTF-8
  (`P:13242-13251`). Key order = Python dict insertion order (Rust: keep
  insertion order, e.g. `serde_json` with `preserve_order`). Python float repr
  differs from `serde_json` in exponent form (`1e-05` vs `1e-5`); harmless to the
  upstream, affects bytes only.
- cli or vscode: gzip with `mtime=0`, compression level 9; header bytes
  `1f 8b 08 00 00 00 00 00 02 ff` (FLG=0, MTIME=0, XFL=2, OS=255) (`I:382-393`).

**[INCONSISTENT]** other chat senders:

- daily chat (`A:2386-2398`): `json.dumps` default separators `", "` / `": "`;
  gzip + Content-Encoding only for cli (not vscode).
- window probe / quota scan (`wb_window_probe.py:339-352`): default separators,
  no cli body edits, no gzip, title-cased headers (no `wb_exact_headers`).
- `probe_account` (`P:15179-15199`) matches `open_upstream`.

Streaming is requested only via body `stream:true`; `Accept` stays
`application/json` (cli) or `application/json, text/plain, */*`. Client
non-stream requests are served by aggregating the upstream SSE
(`P:15412`, `P:22574-22598`).

### 4.4 Upstream SSE response format

Response: `200`, `Content-Type: text/event-stream` (body may be chunked or
Content-Length delimited). Frames are OpenAI chat.completion.chunk lines:

```text
data: {"id":"...","object":"chat.completion.chunk","created":...,"model":"deepseek-v4.1-flash","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}

data: {"id":"...","model":"...","choices":[{"index":0,"delta":{"content":"Hello","reasoning_content":"","refusal":"","extra_fields":null,"function_call":{"name":"","arguments":""},"tool_calls":[]},"finish_reason":""}]}

data: {"id":"...","model":"...","choices":[{"index":0,"delta":{"role":"assistant","content":""}}],"usage":{"prompt_tokens":37,"completion_tokens":16,"total_tokens":53,"prompt_tokens_details":{"cached_tokens":5},"completion_tokens_details":{"reasoning_tokens":0},"cached_tokens":5,"prompt_cache_hit_tokens":5,"cache_read_input_tokens":0,"credit":0.12}}

data: {"id":"...","model":"...","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
```

The exact sample above is a composite; shapes are taken from the parser and
tests (`tests/_test_chat_stream_usage_frame.py:68-85`, `P:11516-11557`,
`P:363`). Observed/handled properties:

- Lines: `data:` prefix possibly repeated (`data: data: {...}`); `:` comments
  and `data: : heartbeat` are keepalives; blank lines separate events
  (`P:11505-11515`). Parsing is line-based (`iter_sse_lines`, `P:15299-15349`),
  `\n` split, no multi-line `data:` joining.
- Delta noise the upstream pads with: empty `reasoning_content`, `refusal`,
  `extra_fields`; `function_call` with empty `name`; empty `tool_calls: []`;
  `finish_reason: ""` (meaning null). `NOISE_KEYS = ("extra_fields","refusal","reasoning_content")`
  (`P:363`, `P:11516-11557`).
- Reasoning trace in `delta.reasoning_content` (deepseek style).
- Tool calls in `delta.tool_calls[]` with `index`, `id`, `type`, `function.{name,arguments}`
  streamed incrementally.
- **Usage**: may ride on a chunk that also carries `choices` (in-band), not
  necessarily the final choices-less chunk; may appear more than once — the
  gateway keeps the one with the largest `total_tokens` (`P:22426-22436`,
  `P:15436-15439`).
- **Credit**: `usage.credit` (float, credits charged for this request), read by
  `_extract_usage` (`P:651-663`) and subtracted from the cached balance
  (`P:886-887`).
- Cache hits: any of `prompt_tokens_details.cached_tokens`,
  `prompt_cache_hit_tokens`, `cache_read_input_tokens`, `cached_tokens`,
  `input_tokens_details.cached_tokens`, `completion_tokens_details.cached_tokens`;
  first positive wins (`P:806-833`).
- **In-stream errors**: a frame whose JSON has an `error` object
  (`{"error":{"message":"...", ...}}`) is treated as terminal by the Anthropic
  translator (`P:17139-17148`); the OpenAI passthrough forwards it unchanged.
  `finish_reason: "error"` marks the turn failed (`P:22560`). Truncated body
  (peer close with Content-Length remaining) → `IncompleteRead` (`P:15333-15339`).
  **[AMBIGUOUS]**: no captured sample of an upstream in-stream error frame
  exists in the repo; only these two shapes are handled.

### 4.5 Non-200 responses and upstream error codes

Error bodies are JSON: `{"code": <int>, "msg": "<text>", ...}`; some add
`requestId`, `displayMsg: {"en": "...", "zh": "..."}`, `retry_policy`,
`message` instead of `msg`. The gateway reads up to 600 bytes
(`P:13870-13896`); the pool buffers up to 1 MiB of an error body
(`UP:52,730-735`). Recorded samples:

```json
{"code":11140,"msg":"request illegal","requestId":"1111...","displayMsg":{"en":"Your account has been restricted due to unusual activity","zh":"账号存在异常行为，已被限制使用"}}
{"code":6004,"msg":"usage exceeds frequency limit, but don't worry, your usage will reset at 2026-10-06 21:00:09 UTC+8, alternatively, you can switch to the other models to continue using it."}
{"code":6004,"msg":"您的使用量已超出频率限制，将在 2026-10-06 14:45:04 UTC+8 重置，您也可以切换其他模型继续使用。"}
{"code":14003,"msg":"too many requests","retry_policy":{"retry":true},"displayMsg":{"en":"Too many requests. Please retry later.","zh":"请求过于频繁，请稍后重试。"}}
{"code":11101,"msg":"Unmarshal chat params failed with error: json: cannot unmarshal string into Go struct field Request.stop of type []string"}
{"code":11129,"msg":"invalid function call parameters"}
{"code":11134,"msg":"the model provider is temporarily unavailable"}
{"code":10105,"msg":"CreateConversation: 10105:you have reached the maximum number of active conversations(10), please c..."}
```

(`tests/_test_restricted_403.py:51-56`, `tests/_test_pool_backoff.py:42-64`,
`tests/_test_stop_normalization.py:6-8`, `tests/_test_tool_parameters_normalization.py:8`,
`tests/_test_breaker_fairness.py:239`, `A:731-744`.)

Code table:

| code | HTTP | meaning | gateway action | cite |
|---|---|---|---|---|
| 401 (status) | 401 | token expired/invalid | refresh once per account per request, retry same account; else `note_error("HTTP 401", 60s)` and rotate. Bare HTML 401 (openresty) is not treated as auth failure | `P:14774-14797`, `A:3383-3395` |
| 402 (status) | 402 | out of credits | park account until next local 04:00; rotate | `P:14762-14773`, `A:3178-3191` |
| 403 + 11140 | 403 | account restricted ("unusual activity") | persist `restrictedUntil = now+7200`; record realm restriction event; **stop** (no rotation by default, `WB_RESTRICTED_403_ROTATE_MAX=0`); ≥2 distinct accounts in 900 s → whole realm paused 1800 s | `P:14713-14743`, `P:14084-14182` |
| 403 other | 403 | content review | once: switch to degraded prompt and retry (passthrough/append modes); else pass through | `P:14744-14761` |
| 6004 | 429 | per-model quota window full; body names reset wall clock `YYYY-MM-DD HH:MM:SS UTC+8` | cool that model until reset; persist cap for free models; schedule window probe | `P:14639-14712`, `P:14032-14045`, `P:14200-14240` |
| 14003 | 429 | soft rate limit ("too many requests", intl) | unscoped ladder on the model: 60 s doubling to 600 s | `P:13964-14026`, `A:648-692` |
| 429 bare "usage exceeds frequency limit" | 429 | soft limit | same as 14003 | same |
| 429 + account-scope wording | 429 | account-level limit | account cooldown 600 s doubling to 7200 s | `P:13975-14066`, `A:640-682` |
| 11101 | 400 | request JSON does not unmarshal into upstream Go struct (string `stop`, object `tool_choice`, non-string `arguments`) | 400 passed through (prevented by normalisation) | `P:12399-12424`, `P:11817-11840` |
| 11128 (`11-128`) | 400 | "Illegal API invocation from an unapproved channel": missing conversation header family, or system-prompt fingerprint (Claude Code / Codex / OmO / DSH phrases) | pass-through; prevented by headers + sanitizer | `I:52-54`, `P:11624-11789` |
| 11129 | 400/5xx | invalid function call parameters (tool schema) | model-scoped: no account charge on 5xx | `P:12325-12355`, `P:13853` |
| 11133 | 400 | model_param_invalid | hint only | `P:13541` |
| 11135 | 400 | invalid_image_data | hint only | `P:13545` |
| 11148 | 400 | tool_call_sequence_broken | prevented by tool-call repair | `P:11907` |
| 11155 | 400 | deepseek thinking: previous-turn reasoning_content missing | prevented by backfill | `P:12131-12142` |
| 11102 | 5xx | service info not found (model not on this exit) | model-scoped, stop rotating, return body | `P:13850-13853`, `P:14805-14817` |
| 11134 | 5xx | model provider temporarily unavailable | model-scoped | same |
| 11150 | 5xx | reasoning effort unsupported | model-scoped | same |
| 11158 | 400/5xx | model not available for this user type (PRO-only) | avoided via `modelTiers` gating | `P:13853`, `TI:1-13` |
| 11217 | 200 body | OAuth login pending | keep polling | `A:354,4563` |
| 12403 | 400 | `/v3/config` with unknown UA | use desktop UA | `P:11078-11080` |
| 10105 | 429 | web console: too many active conversations (10) | stop old conversations | `A:82-91,731-744` |
| 10001 | 200/400 | checkin: "already"/content rejected (ambiguous) | text-classified | `A:358-384` |
| other 5xx | 5xx | transient | 502/503/504 → egress backoff if enabled; else account breaker | `P:14818-14843` |
| other 4xx | 4xx | — | re-raised to client unchanged | `P:14844` |

Rate-limit reset parsing (`P:14200-14240`): unescape `\uXXXX` if present; regex
`(?:reset at|将在)\s*(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})`, fallback
`(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})\s*UTC[+-]\d{1,2}`; offset from
`UTC([+-]\d{1,2})(?::?(\d{2}))?` (default 0); epoch = `timegm(stamp) - offset`.

11140 detection (`P:14165-14182`): regex `"code"\s*:\s*"?11140"?` or
lowercase body containing `unusual activity`, `账号存在异常行为`, `已被限制`.

Account-scope phrases for 429 (`P:13975-14003`): ASCII ones matched with `\b`
word boundaries (so `accountId` does not match), CJK as substrings; only when
no reset clock was parsed.

### 4.6 Non-chat header sets (title path; shown in insertion order, wire names title-cased)

**Billing / tasks "billing" purpose** (`A:1688-1720`), captured cn:

```text
Content-Type: application/json
Accept: application/json, text/plain, */*
X-Requested-With: XMLHttpRequest
User-Agent: WorkBuddy/5.8.4 | WorkBuddy/5.8.3
Origin: {realm origin}
Referer: {realm origin}/
Authorization: Bearer {token}
X-User-Id: {uid}
X-Domain: {account.domain or realm domain}
X-Codebuddy-Request: 1                 # source spelling X-CodeBuddy-Request
Accept-Language: en-US | zh-CN
X-Request-Id: {uuid4 hex}
X-Machine-Id: {derive_id(uid,"machine")}
X-Session-Id: {derive_id(uid,"session")}
[X-Api-Key: {token}]                   # static key
X-Enterprise-Id / X-Tenant-Id: {eid}   # if enterprise, else:
X-No-Enterprise-Id: 1
[X-Product: SaaS]                      # cn only
[X-Device-Token: ...]                  # product != cli and configured
[X-Client-Platform: web]               # added by summary / packages / enterprise-usage callers (A:2633,2909,3042)
```

**`/v3/config`** (`P:11075-11096`, GET, timeout 10 s, up to two attempts: `chat_ua`
then `billing_ua`):

```text
Accept: application/json, text/plain, */*
User-Agent: {realm chat_ua}            # NOT the account identity UA; unknown UA → 400/12403, UA without "CLI/<ver>" → reduced list (37 models)
Origin: {realm origin}
Referer: {realm origin}/
Authorization: Bearer {token}
X-User-Id: {uid}
[X-Api-Key: {token}]                   # static key (otherwise 54 instead of 56 models)
```

Response: `{"data": {"agents": [{"name": "cli", "models": ["id", ...]}, ...] | {name: {...}},
"models": [{"id": ..., "credits": "x0.00", "maxInputTokens", "maxOutputTokens",
"supportsImages", "supportsToolCall", "supportsReasoning", "reasoning": {...}, ...}],
"modelTiers": [{"modelIds": [...], "requiredUserType" | "tier", "enabled", "accessMode", "badge"}]}}`.
Empty agent list under HTTP 200 = unusable credential (`P:11135-11176`).
Picked account = `POOL.representative(realm)`; result cached 300 s per realm
(`P:10861-10902`); manual refresh cooldown 30 s; daily forced refresh loop
(`P:11283-11395`).

**Legacy model list** `GET https://www.workbuddy.ai/v2/enterprises/personal/models`
with `account.headers()` (billing purpose) (`P:11484-11504`); response
`data.agents[].models[]`.

**Refresh** — see §5. **OAuth** — see §6.

**Web console** (`A:1915-1925`, intl):

```text
Authorization: Bearer {token}
X-User-Id: {uid}
Content-Type: application/json
Accept: application/json, text/plain, */*
Origin: https://www.workbuddy.ai
Referer: https://www.workbuddy.ai/app
User-Agent: {WEB_USER_AGENT Edge 140}
```

**Profile** `GET {web origin}/console/account` (`A:164-174`): headers
`Authorization`, `Accept`, `x-client-platform: web`, `Origin: https://www.workbuddy.cn`
(always the CN origin, even for intl — **[INCONSISTENT]**), `Referer:
https://www.workbuddy.cn/profile/account-settings`, `User-Agent` (web). Response
`{"uid": ..., "nickname": ...}`; uid mismatch → error.

Growth/tasks endpoints (`T`) use `account.headers("chat")` (full chat identity
headers, title-cased, GET without body) except `/v2/report` against the billing
base and `claim-gift`/`claim-compensation` which use billing headers.

---

## 5. Token refresh

### 5.1 Request (`A:1821-1876`)

```http
POST {realm chat_upstream}/v2/plugin/auth/token/refresh HTTP/1.1
Accept-Encoding: identity
Content-Length: 2
Host: www.workbuddy.ai | copilot.tencent.com
Content-Type: application/json
Accept: application/json, text/plain, */*
X-Requested-With: XMLHttpRequest
User-Agent: WorkBuddy/5.8.4 | WorkBuddy/5.8.3
Origin: https://www.workbuddy.ai | https://www.codebuddy.cn
Referer: {origin}/
X-Refresh-Token: {refreshToken}
X-Auth-Refresh-Source: plugin | workbuddy        # intl | cn
X-User-Id: {uid}
X-Domain: {account.domain or realm domain}
X-Codebuddy-Request: 1
Accept-Language: en-US | zh-CN
[X-Enterprise-Id: {eid}]
Connection: keep-alive

{}
```

Notes: no `Authorization` header. Uses `chat_upstream` of the realm (so
`WB_CN_CHAT_UPSTREAM` applies), not the product endpoint. Timeout 30 s, via
`http_json` (3 attempts, sleep `1.0*attempt` s, retry only on `_retryable`:
SSL errors, URLError, OSError family, HTTP ≥ 500; never on TLS verify errors)
(`A:135-147`, `A:236-265`). Per-account `_refresh_lock` serialises refreshes.

### 5.2 Response handling

```text
data = payload.data or {}; data = data.data or data          # tolerate double envelope
token = data.accessToken
missing → token_expired=True, last_error "refresh returned no token (<msg>)", return False
else:
  access_token  = token
  refresh_token = data.refreshToken or old refresh_token     # rotation optional
  expires_at    = jwt_exp(token) or old expires_at           # response expiresAt ignored
  token_expired = False; clear last_error, cooldown_until
  save() if the account has a path whose dir exists
```

Exception: if `str(exc)` contains `401`, `Authorization` or `Unauthorized` →
`token_expired=True`, error "登录已过期 (HTTP 401)，请重新登录"; else "refresh failed: ...".
API-key accounts: return False, no request. Empty refreshToken: `token_expired=True`.
The response `code` is not checked (only presence of `accessToken`).

### 5.3 Triggers

| trigger | condition | cite |
|---|---|---|
| pick-time (`Account.ready`) | `exp` known and `exp - now ≤ 120 s` (including already expired) → synchronous refresh; result decides readiness | `A:1634-1646` |
| chat 401 | once per account per `open_upstream`; success → retry same account | `P:14774-14791` |
| billing 401 | summary / get-user-resource exception text with 401 → refresh then retry once | `A:2641-2657`, `A:2801-2816` |
| scheduler keepalive | daily at hour 22 (and once 10 s after start): `exp ≤ 0` or `exp - now < lead`; lead = `WB_KEEPALIVE_LEAD_HOURS` (default 48 h, valid 1..720 h) | `SC:44-79`, `SC:396-411`, `SC:87-90,151-155` |
| quota scan | `exp - now < 120` | `wb_quota_scan.py:291-297` |
| window probe, panel | see `wb_window_probe.py:292`, `P:21191` | |

---

## 6. OAuth device-style login

### 6.1 Start (`A:4516-4536`)

```http
POST {chat_upstream}/v2/plugin/auth/state?platform=CLI
Content-Type: application/json
Accept: application/json, text/plain, */*
X-Requested-With: XMLHttpRequest
User-Agent: {billing_ua}
Origin: {origin}
Referer: {origin}/

{}
```

`platform` from the panel request, default `"CLI"`, URL-quoted (`P:21101-21105`).
Response `data.state`, `data.authUrl` (both required, else error). The user
opens `authUrl` in a browser. State kept in memory 600 s (`LOGIN_TTL_SECONDS`).
Direct connection (no account proxy).

### 6.2 Poll (`A:4538-4600`)

```http
GET {chat_upstream}/v2/plugin/auth/token?state={state}
(same 6 headers as start, no body)
```

`http_json(..., retries=2, timeout=30)`. Response:

- `code == 11217` → pending (show `msg`);
- `code != 0` → error;
- `code == 0`, `data.accessToken` present → success; fields used:
  `data.accessToken`, `data.refreshToken`, `data.expiresAt` (ms or s), `data.domain`.
- network error → reported as pending.

Then nickname: `GET {chat_upstream}/v2/plugin/login/account?state={state}` with
the same headers plus `Authorization: Bearer {token}`, plain `urllib.request.urlopen`
(not the pool, no proxy, timeout 15 s), `data.nickname`; errors ignored.

Account created with `uid = jwt sub`, `nickname or uid[:8]`, `domain = data.domain
or realm domain`, `source = "oauth"`; saved; CN accounts immediately run a checkin.

---

## 7. Transport: pool, keep-alive, proxy, TLS, timeouts, retries

### 7.1 HTTP version and ALPN

- HTTP/1.1 only, `http.client`. No HTTP/2 anywhere.
- **ALPN differs by path** (measured on Python 3.14 against a local TLS server
  offering `h2,http/1.1`):
  - pooled path (default; all chat and almost all other traffic): **no ALPN
    extension**. The pool passes an explicit shared context
    (`UP:150-183`, `UP:573-585`) built by `ssl._create_default_https_context()`
    (3.11) or `http.client._create_https_context(None)` (3.13+); neither sets
    ALPN, because http.client only adds `http/1.1` ALPN when it builds the
    context itself.
  - urllib fallback path (socks/non-http proxy string, 3xx replay,
    `WB_UPSTREAM_KEEPALIVE=0`, the OAuth nickname call): http.client builds the
    context → ALPN `http/1.1` (measured on 3.14; CPython 3.10-3.12
    `HTTPSConnection.__init__` does the same, bpo-40968).
  - Production image is `python:3.11-alpine` (`Dockerfile:2`); the above holds
    there too (code-reading, not measured on 3.11).
- `post_handshake_auth = True` on 3.13+ pooled contexts; with `WB_CA_BUNDLE` the
  context is `ssl.create_default_context()` + extra CA file (`UP:174-179`).

### 7.2 TLS / JA3 relevance

The ClientHello is CPython's default client `SSLContext` on the image's OpenSSL
(Alpine OpenSSL 3.x): minimum TLS 1.2, max TLS 1.3, CPython 3.10+ default cipher
string `@SECLEVEL=2:ECDH+AESGCM:ECDH+CHACHA20:ECDH+AES:DHE+AES:!aNULL:!eNULL:!aDSS:!SHA1:!AESCCM`
(17 suites on the dev box: 3 TLS 1.3 + ECDHE/DHE AES-GCM, CHACHA20, AES-CBC-SHA256/384),
OpenSSL default groups/sigalgs/extension order, SNI = target host,
`check_hostname=True`, `CERT_REQUIRED`, system CA store, no ALPN on the pooled
path, session resumption via the process-wide context. This does **not** match a Node/Electron client hello (the
real WorkBuddy/CodeBuddy clients); the Python gateway is not JA3-faithful today.
A Rust port using rustls will produce a different (also non-Node) fingerprint
and will send ALPN unless explicitly cleared. Recommendation for fidelity with
the current gateway: `rustls` with `alpn_protocols = []`, TLS 1.2+1.3; or
OpenSSL-backed `native-tls`/`openssl` with default ciphers to be closest to the
Python baseline. **[AMBIGUOUS]** whether upstream WAF scores JA3; no code or
comment indicates it does.

Note the header-level fidelity work (exact header order/case, compact JSON,
gzip) targets the Node client, while TLS is Python's — keep both as-is.

### 7.3 Connection pool (`UP:335-470`, `UP:767-852`)

- Key = `(scheme, host, port, proxy_string_as_given, identity_digest)`.
  identity_digest = for the first present of lowercase headers
  `authorization`, `x-refresh-token`, `x-user-id`:
  `"<field>:" + sha256("<field>\0<value>")[:16]`; empty if none
  (`UP:600-630`). Example: `Authorization: Bearer abc` →
  `authorization:33c1033670e04478`. Different accounts never share a TCP/TLS
  connection; refresh requests (no Authorization) get their own key.
- Idle connections per key: `WB_UPSTREAM_POOL_SIZE` (default 2, min 1);
  over capacity → close oldest. Idle TTL `WB_UPSTREAM_POOL_IDLE` (default 90 s,
  min 5 s); sweep all keys on every acquire/release. LIFO reuse.
- Liveness check before reuse: TLS `pending() > 0` → dead; non-blocking
  `recv(1)`: `EWOULDBLOCK`/`SSLWantRead` → alive, anything else (EOF, data,
  error) → dead (`UP:231-263`). On reuse the socket timeout is reset to the
  request's timeout.
- Return to pool only when the body was read to its natural end (Content-Length
  exhausted or chunked terminator + trailer) and the response is not
  `Connection: close`; otherwise close (`UP:266-324`).
- Non-2xx: body read (≤1 MiB) so the connection can be reused, then
  `HTTPError(status, reason, headers, BytesIO(body))`. 3xx → `PoolBypass` and the
  whole request is re-sent through urllib (follows redirects; POST→GET
  downgrade per urllib) (`UP:839-852`).
- **Retry-once rule** (`UP:809-838`): only when the connection was *reused* and
  the failure is a stale-connection shape (`BrokenPipe`, `ConnectionReset`,
  `ConnectionAborted`, `SSLEOFError`, `SSLError` containing `UNEXPECTED_EOF`)
  raised while sending or before the status line; body must be bytes/None; the
  retry uses a brand-new connection. Timeouts never retried here. Other send
  OSErrors → `URLError(err)`; other status-line ConnectionErrors raised as-is.

### 7.4 Proxy semantics

- Per-account proxy string = enabled proxy slot `url` (settings
  `proxy_slots[{id,name,url,enabled,...}]`) bound by `proxySlot`, else legacy
  `proxy` field (`A:3946-3963`, `S:2815-2882`). Empty = direct.
- Pool accepts only `http://[user[:pass]@]host[:port][/...]` or bare
  `[user[:pass]@]host[:port]`; IPv6 `[::1]:8080`; userinfo URL-decoded; non-digit
  port → bypass. Missing port → 443 for https targets, 80 for http (mirrors
  urllib's quirk) (`UP:490-528`, `UP:567-571`).
- HTTPS target via proxy: `CONNECT host:port HTTP/1.1` + `Host: host:port`
  (+ `Proxy-Authorization: Basic b64(user:pass)` only if both user and password
  non-empty) then TLS inside (`UP:572-581`). Plain-http target via proxy:
  absolute-URI request line and `Proxy-authorization` on the request.
- `no_proxy` / system bypass list (`urllib.request.proxy_bypass(host)`) honoured:
  bypassed → direct and the direct pool key (`UP:543-555`, `UP:738-749`).
- `socks5://`, `https://` (proxy scheme) etc.: pool raises `PoolBypass`, urllib
  `ProxyHandler({"http": s, "https": s})` is used (`A:190-207`). urllib does
  **not** speak SOCKS: measured, it sends an HTTP `CONNECT` to the socks
  host:port, which fails. So effectively only HTTP proxies work; a Rust port may
  add SOCKS5 support as an improvement, but byte-compat baseline = HTTP CONNECT.
- The urllib fallback path sends `Connection: close` and title-cased headers
  (exact-header ordering is lost).
- Proxy exit probing: `GET https://api.ipify.org` through the proxy, 12 s
  (`P:6325-6335`); geo lookup direct (§10.2).
- The OAuth start/poll and nickname calls never use a proxy.

### 7.5 Timeouts

| call | timeout | cite |
|---|---|---|
| chat (open_upstream) | `header_timeout` = settings `upstream.header_timeout_seconds` (default **120 s**, int ≥1) used as the socket timeout for connect + TLS + request + response headers; then the socket timeout is switched to `idle_timeout` (default **300 s**) for every subsequent read; no total cap | `P:14243-14294`, `S:1522-1561`, `P:14617-14619` |
| probe_account | 30 s | `P:15169` |
| window probe / quota scan | 60 s | `wb_window_probe.py:39` |
| daily chat | 20 s | `A:2404` |
| refresh, OAuth start/poll, get-user-resource, legacy models | 30 s | |
| summary / packages / enterprise / checkin | 15 s; checkin-status 10 s | |
| `/v3/config` | 10 s | `P:11260` |
| nickname, profile | 15 s | |
| ipify 12 s; ip-api 8 s | | |

Python's single `timeout` is per socket operation (connect, each send/recv),
not a deadline. Replicate as connect timeout = read timeout = N; streaming idle
per-read timeout = 300 s.

### 7.6 Chat retry loop (`P:14330-14993`) summary

`max_attempts = max(2, ready_count) + 1 (+4 if auto product switch)`; pinned
account → 1. Each attempt picks a not-yet-tried account of the realm (session
affinity first). Per outcome:

- success (status 2xx after headers) → account `note_success(model)`; egress
  success (unless deferred to end of body); return.
- 429 → §4.5; `continue` (next account), or switch product (WB→VSC→CLI among
  allowed, ≤4 per (uid,model) per 60 s) when enabled.
- 403 → restriction / content review.
- 402 → park, continue. 401 → refresh-and-retry-same or cool 60 s.
- 400 → raised to client immediately.
- 5xx → model-scoped codes stop; 502/503/504 with egress backoff on → sleep
  `min(egress_wait, 5 s)`; else account breaker (3 fails in 600 s window → 30 min,
  doubling to 6 h).
- TLS verify error → stop immediately.
- Local network (gaierror, ENETUNREACH, EHOSTUNREACH, …) → no charge, sleep
  `min(0.3*n, 1.5)`.
- Transient (ECONNRESET, EPIPE, ETIMEDOUT, ssl/timeout/502/503/504/IncompleteRead
  text) → egress backoff or degrade counter; sleep `min(0.6*n, 2)`.
- Pool exhausted but transient hits occurred → clear tried set, sleep
  `min(1.5*n, 3)`, repick.
- ECONNREFUSED is **not** transient (dead proxy) → charge + 60 s cooldown.

Mid-stream failure before any byte reached the client → one transparent reopen
(`P:22503-22526`).

---

## 8. Credits / balance / tier

All POST with billing headers (§4.6), JSON body, via `http_json` (3 attempts).
Dispatch (`A:3085-3122`), serialised per account by `_credits_lock`:

```text
api key        → enterprise (v2 path)
realm cn       → cn_detailed; if ok and size==0 and enterprise → enterprise
                 if detailed says auth failure → stop (need relogin)
                 on exception / failure and enterprise → enterprise
fallback       → get-user-resource
after ok: revive 402 park if remain > 0; refresh tier if older than 24 h
```

### 8.1 CN detailed (`A:2630-2783`), headers + `X-Client-Platform: web`

1. `POST {BU}/billing/meter/get-user-resource-summary` body `{}` →
   `{"code":0,"data":{"IsPaidUser":bool,"SubscriptionPackageName":str,
   "SubscriptionPackageCode":str,"IsProtectedPriceUser":bool,"ProTrialStatus":int,
   "Packages":[{"PackageCode":..,"CycleTotalCapacity":..,"CycleRemainCapacity":..,"CycleUsedCapacity":..}]}}`.
   `code != 0` → error. 401-ish exception → refresh + one retry.
2. If any `PackageCode`: `POST .../get-user-resource-free-packages`
   body `{"PackageCodes":[...],"PageNumber":1,"PageSize":200,"Status":[0]}`
   → `data.Accounts[]`.
3. `POST .../get-user-resource-paid-packages`
   body `{"PackageCodes":[...],"PageNumber":1,"PageSize":200,"Status":[0,3],"NeedRenewInfo":true}`
   → `data.Accounts[]`.
4. `POST {CU}/v2/billing/meter/checkin-activity-status` body `{}`, 10 s →
   `data.{today_checked_in, streak_days, daily_credit, today_credit, active}`.

Body serialization here is `json.dumps(body)` default (`", "`, `": "`,
`ensure_ascii=True`) (`A:2682,2694`).

Accounts deduped by `AccountId`.

### 8.2 Fallback / intl (`A:2785-2868`)

```json
POST {BU}/v2/billing/meter/get-user-resource
{"PageNumber": 1, "PageSize": 100, "ProductCode": "p_tcaca", "Status": [0, 3],
 "PackageEndTimeRangeBegin": "<now local YYYY-MM-DD HH:MM:SS>",
 "PackageEndTimeRangeEnd": "2036-01-01 00:00:00"}
```

Response path `data.Response.Data.Accounts[]` (Tencent-cloud style envelope).

Package account fields read (`A:2539-2628`): `PackageName`, `PackageCode`,
`CycleCapacitySizePrecise|CycleCapacitySize|CapacitySizePrecise|CapacitySize`,
same for `Remain` and `Used`, `AccountAttributes[{Key:"grantReason",Value}]`,
`CreateTime` (ms), `CycleEndTime|ExpiredTime`, `DeductionEndTime` (ms; > 730 days
away = never expires), `CycleStartTime`, `InUsage`, `AutoRenewFlag|SupportAutoRenew`,
`ProductName`, `SubProductName`, `ResourceId`, `DealName`, `CapacityUnit`,
`Status`, `AccountId`. Time strings parsed as `YYYY-MM-DD HH:MM:SS` local
(T replaced by space, first 19 chars).

### 8.3 Enterprise (`A:2896-2975`)

`POST {BU}/billing/meter/get-enterprise-user-usage` (OAuth) or
`/v2/billing/meter/get-enterprise-user-usage` (static key), body `{}`,
`X-Client-Platform: web` → `data.{credit (used), limitNum, cycleStartTime,
cycleEndTime|cycleResetTime}`. Probe for a new static key (`A:1174-1204`) uses
custom headers: `Authorization`, `X-API-Key`, `Content-Type`, `Accept:
application/json`, `User-Agent: {cn chat_ua}`; success iff `code == 0`.

### 8.4 Stored `credits` shape

```json
{"remain": 0.0, "used": 0.0, "size": 0.0,
 "used_percent": "0.0%", "remain_percent": "100.0%",
 "is_paid_user": false, "is_enterprise": true,
 "checkin": {"today_checked_in": false, "streak_days": 0, "daily_credit": 0, "today_credit": 0, "active": true},
 "earliest_expiring": {"name": "...", "package_code": "...", "remain": 0, "cycle_end_time": "...", "days_left": 3.5},
 "packages": [{"name","package_code","product_name","sub_product_name","grant_reason","resource_id","deal_name",
               "create_time","remain","used","size","unit","in_usage","auto_renew","cycle_start_time",
               "cycle_end_time","expire_time","no_expiry","days_left","is_expired","status"}],
 "updated_at": 1790000000.0, "updated_iso": "2026-10-22 10:00:00"}
```

(`is_enterprise` only on enterprise results.) Between refreshes,
`usage.credit` from each chat response is subtracted locally (`P:886-887`,
`A:1263`). Background refresher: one stalest account per 1800 s tick, TTL from
settings `credits_refresh_hours`, failed accounts parked 6 h (`A:3523-3660`).

### 8.5 Tier (`A:396-420`, `A:3019-3062`, `TI`)

From the summary payload: `paid = bool(IsPaidUser)` (absent → unknown, never
"free"), `package = SubscriptionPackageName`, `trial = int(ProTrialStatus)`.
TTL 24 h. Skipped for enterprise / static key / restricted accounts.
`/v3/config` `modelTiers[]` → `{model: "free"|"standard"}`; entries with
`enabled:false` skipped; badge "订阅优先"/"priority" without
`accessMode:"exclusive"` is not a gate; `requiredUserType|tier` in
`free, personal_free_user, personal-free-user` → free, any other non-empty →
standard (paid). Free accounts are not given `standard` models (`A:1613-1615`).

### 8.6 Checkin (CN) (`A:2453-2506`, `A:358-384`)

`POST https://www.codebuddy.cn/v2/billing/meter/daily-checkin` body `{}`, billing
headers, 15 s. Classification of `(code, msg)`: msg contains any of
`处理中, 请勿重复操作, 系统繁忙, 稍后再试` → pending; `code==0` → failed if msg
contains `失败` else checked; `code==10001` → already if msg contains `已签到`
else failed; else failed. HTTP 400 with "已签到" body → already. Only
checked/already write `lastCheckin`.

---

## 9. Account-level failure ladders (affects which account the next request uses)

| ladder | trigger | base | cap | cite |
|---|---|---|---|---|
| soft (account) | 429 with account-scope wording | 600 s ×2^(n-1) | 7200 s | `A:640-682` |
| unscoped (model) | 429 soft shape, no reset clock | 60 s ×2^(n-1) | 600 s | `A:648-692` |
| model cooldown | 429 other | until reset or 60 s | — | `P:14662-14671` |
| breaker | `fails ≥ 3` (5xx, unknown) | 1800 s ×2^(n-3) | 21600 s | `A:650-698` |
| degrade | `degrade_count ≥ 5` (transport) | 600 s ×2^(n-5) | 7200 s | `A:663-704` |
| restricted | 403 11140 | 7200 s fixed, persisted, first hit only | — | `A:671,3152-3176` |
| balance | 402 | next local 04:00 | — | `A:619-632` |
| generic | other errors / 401 after failed refresh | 60 s (3 s if pool has 1 account) | — | `A:3130-3150` |

Streak windows reset if the previous event is older than `WB_FAIL_WINDOW`
(default 600 s; 0 disables decay) (`A:659-662`, `A:3224-3272`). Any success
clears every ladder (including `restrictedUntil`) and the model's cooldown
(`A:3291-3314`).

---

## 10. Egress backoff and IP intel

### 10.1 Egress-level transient backoff (`E:1-86`, `UP:751-765`)

Off unless settings key `egress_backoff_enabled` is true (`S:1957-1967`).
Egress key = `(scheme, host, port, proxy_string)` with system-bypass folded to
direct (no identity component; all direct accounts share one key).

```text
note_transient(key):
  state = [streak, last]; if absent or now-last > 60 s → [0, now]
  streak += 1; last = now
  wait = min(1.0 * 2^min(streak-1, 20), 15.0) * uniform(0.75, 1.25)
note_success(key): delete state
```

In `open_upstream`, each egress key is charged at most once per request
(`P:14465-14471`); the request thread sleeps `min(wait, 5.0)` (`P:13700`) then
retries. Charged on: transport exceptions classified transient, HTTP
502/503/504, and mid-stream read failures (`P:22499-22502`). Success recorded
after headers, or at end of a fully read stream when deferred.

### 10.2 IP intel (`IP:1-107`)

```http
GET http://ip-api.com/json/{urlquote(ip, safe="")}?lang=zh-CN&fields=status,message,country,countryCode,isp,org,as,hosting,proxy,mobile
User-Agent: wb-proxy
```

Plain HTTP, direct (never through the probed proxy), timeout 8 s, plain
`urllib.request.urlopen`. `status != "success"` → all empty. Mapping:
`hosting true → "datacenter"` (机房), `false → "residential"` (住宅), else `""`;
`country`, `countryCode` upper, `isp or org`, `as`. Slot default name
`"<country> <住宅|机房>"`. Exit IP first fetched through the proxy from
`https://api.ipify.org` (12 s).

---

## 11. Open questions / ambiguities for the implementer

1. **Header case on non-chat paths**: billing/refresh/OAuth/config go out with
   urllib title-casing (`X-Codebuddy-Request`, `X-Request-Id`, `X-Machine-Id`,
   `Traceparent`, `B3`, `X-Ide-Type` for task calls using chat headers). Byte
   compatibility with today means reproducing the title-casing; fidelity with
   the official client would mean the original spelling. Decide explicitly.
2. Window probe / quota scan chat requests skip compact JSON, gzip and exact
   headers (§4.3). Daily chat uses non-compact JSON and gzips only for cli.
   Likely oversights in Python; replicating them is optional.
3. Billing `X-Device-Token` is sent for intl too (if configured), while chat
   restricts it to cn (§3.4).
4. Profile request always sends the CN `Origin`/`Referer` (§4.6).
5. Refresh response `expiresAt` is ignored (JWT `exp` only); OAuth poll uses it.
6. Account file name is derived from uid on save, not preserved (§2.1).
7. No captured sample of an in-stream upstream error frame (§4.4); handle both
   `{"error":{...}}` frames and `finish_reason:"error"`.
8. ALPN: pooled path sends none; urllib fallback sends `http/1.1` (§7.1). There
   is no evidence the upstream inspects TLS fingerprints.
9. `SOCKS` proxy strings do not work in Python (HTTP CONNECT is sent to the
   SOCKS port). A Rust port may implement SOCKS5; that is a behaviour change.
10. 11128 is triggered both by missing conversation headers and by prompt
    fingerprints; the sanitizer rules (`P:11624-11803`) are request-content
    transformations and belong in the request-translation spec, but they are
    required to avoid this upstream error.
