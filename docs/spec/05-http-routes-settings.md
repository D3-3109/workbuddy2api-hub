# 05 — HTTP surface, auth, and configuration

Baseline: tag `py-baseline-1.7.32`. All citations are `file:line` in that tree.
Nothing here was guessed; where the code is surprising it is flagged
**[quirk]** (keep for parity or fix deliberately). Items that only make sense
on a Windows/macOS desktop, the portable bundle, or a localhost-only browser are
flagged **[DROP?]** — candidates to leave out of a Linux-VPS Rust port.

Related specs: chat path (03), responses/anthropic (04), account pool (02),
background jobs (07). This document does not repeat request translation; it
covers routing, auth, HTTP mechanics, every management endpoint, and
`settings.json`.

---

## 0. Server shape

| Aspect | Value | Source |
|---|---|---|
| Server class | `GatewayServer(ThreadingHTTPServer)`, one OS thread per connection, `daemon_threads=True` (stdlib default) | wb_proxy.py:22954-22963 |
| Listen backlog | `request_queue_size = 128` (stdlib default 5 dropped SYNs under panel bursts) | wb_proxy.py:22963 |
| Protocol | `protocol_version = "HTTP/1.1"` -> keep-alive by default | wb_proxy.py:17993 |
| TCP_NODELAY | `disable_nagle_algorithm = True` (header and body are two writes; Nagle + delayed ACK cost ~40 ms per small response) | wb_proxy.py:18000 |
| `Server:` header | `wb-proxy/1.7.32`; `running_version()` parses it | wb_proxy.py:18073, 22642-22649 |
| Socket timeout | none set on `Handler` (stdlib `timeout=None`); an idle keep-alive connection holds its thread indefinitely **[quirk]** | (absence in 17992-18011) |
| Methods | `do_OPTIONS` 18632, `do_GET` 18641, `do_POST` 22031. No `do_HEAD`/`do_PUT`/`do_DELETE`/`do_PATCH`: those get stdlib `send_error(501, "Unsupported method (%r)")` with an **HTML** body | wb_proxy.py:18057-18061 |
| Max request line | 1 MiB (`max_request_line`), over-long -> drain up to 8 MiB then JSON 414 and close | wb_proxy.py:18011, 18033-18051, 18265-18284 |
| Max body | `MAX_PAYLOAD_BYTES` = env `WB_MAX_PAYLOAD_BYTES`, default 16 MiB -> 413 | wb_proxy.py:40, 19356-19412 |
| Chat concurrency | `BoundedSemaphore(MAX_CONCURRENT_CHAT)`; env `WB_MAX_CONCURRENT_CHAT` default 32, `auto`/`pool`/`dynamic` = max(32, ready accounts) at startup; wait `WB_CHAT_SLOT_WAIT` (30 s) then 503 | wb_proxy.py:52-59, 388-418, 22208-22227 |

Rust mapping: hyper/axum with HTTP/1.1 keep-alive, `TCP_NODELAY`, backlog 128,
a semaphore for chat routes. Add an idle keep-alive timeout (Python has none)
— that is a safe improvement.

### 0.1 Per-request lifecycle (`handle_one_request`, wb_proxy.py:18012-18062)

1. Reset per-request state that would otherwise leak across a keep-alive
   connection: `key_entry=None`, `_json_etag=None`, `_body_consumed=False`
   (18018-18024). A Rust port gets this for free with per-request extractors.
2. Read the request line (cap 1 MiB + 1). Empty -> close.
3. `parse_request()`, dispatch `do_<METHOD>`.
4. `wfile.flush()`.

`handle`/`finish` swallow `ConnectionReset/BrokenPipe/ConnectionAborted`
(18063-18072).

### 0.2 Access-log suppression (`log_message`, wb_proxy.py:18074-18089)

Successful (<400) **GET** requests whose path equals or starts with
`/logs`, `/usage`, `/accounts`, `/scheduler`, `/health`, `/panel/status`,
`/realm`, `/favicon.ico` (+ `/`) are not logged. Everything else (all POSTs,
all >=400) goes to the in-memory log via `log()`.

### 0.3 Pinned-account prefix `/u/<ref>/…` (wb_proxy.py:18593-18631)

Every GET and POST first runs `_apply_pinned_route()`; OPTIONS runs only the
strip step. If the path starts with `/u/`, the segment after it is
URL-decoded into `ref`, and `self.path` is rewritten to `/<tail>?<query>`.

| Condition | Result |
|---|---|
| `/u/` with empty ref or no trailing `/` | 404 `pinned account route must be /u/<ref>/v1/...` |
| `POOL is None` | 503 `pinned account '<ref>' is not usable: account pool is not ready` |
| `POOL.find_unique(ref)` -> `"is ambiguous"` | 409 |
| not found | 404 `pinned account '<ref>' <why>` |
| ok | thread-local `_PINNED.account` set; route continues on the rewritten path |

`ref` = full uid, uid 8-char prefix, or nickname (README route table).
**[quirk]** The prefix is accepted on *every* route (e.g. `/u/x/health`,
`/u/x/settings`), not only `/v1/*`; only the chat/responses/models handlers
consult `pinned_account()`. A port may restrict it to `/v1/*` and the
bare-OpenAI aliases.

---

## 1. Response conventions

### 1.1 JSON helpers

| Helper | Behaviour | Source |
|---|---|---|
| `_json(code, obj)` | `json.dumps(ensure_ascii=False)`, `Content-Type: application/json; charset=utf-8`, `Content-Length`; adds `Cache-Control: no-cache` + `ETag` only when `_json_etag` is set; adds `Access-Control-Allow-Origin: *` when `cors_origin_allowed(path)`; flushes | wb_proxy.py:18090-18117 |
| `_json_panel` / `_json_cached_panel` | Same, after `_redact_account_names(obj)` (phone-number masking `138****1234` of `nickname` values and free text; identity keys `uid,id,key,token,…` untouched). Used by all management views; **never** by LLM routes | wb_proxy.py:18175-18182, 10394-10445 |
| `_error(code, msg, type="server_error")` | Answers `Expect: 100-continue`, drains the unread body, then `{"error":{"message","type","code"[, "gateway_hint"]}}` | wb_proxy.py:18308-18319, 13520+ |
| `_anthropic_error` | `{"type":"error","error":{"type","message"}}` | wb_proxy.py:18320-18324, 16534-16541 |
| `_rate_limited(exc)` | 429, header `Retry-After: <wait>`, body `error.{message,type:"rate_limit_error",code:429,retry_after}` | wb_proxy.py:18325-18362 |
| `_download(name, obj)` | Pretty JSON (`indent=2`), `Content-Disposition: attachment; filename="<sanitised [A-Za-z0-9._-], ≤120>"`, `Cache-Control: no-store` | wb_proxy.py:18383-18399 |

`gateway_hint` (wb_proxy.py:13520) is an optional English hint string added
to OpenAI-envelope errors for recognised failure shapes (413 payload, 503
concurrency, upstream codes 11133/11135/11140, rate limits, …).

### 1.2 Conditional GET / ETag (wb_proxy.py:18118-18168, 17896-17933)

`_json_cached(code, data_fn, etag_fn)`:

1. `before = etag_fn()`; `data = data_fn()`; `after = etag_fn()`.
2. Only if `before == after != None` is a tag used (proves the body came from
   the cache entry the tag describes).
3. If `If-None-Match` matches (comma list, `W/` stripped, `*` matches) ->
   **304** with `Cache-Control: no-cache` + `ETag`, no body, no
   `Content-Length`.
4. Else 200 with `Cache-Control: no-cache` + `ETag`.

Tag format `_cache_etag(kind, key, built_at)` =
`"<hex(int(built_at*1e6))>-<sha1(kind + "\x1f" + key)[:16]>"` (strong).
The cache entries and their TTLs (`WB_STATS_TTL` 15 s, `WB_REMAINING_TTL`
15 s) are described in the usage spec; routes using it: `/usage`,
`/v1/usage`, `/usage/analytics`, `/usage/by-account`, `/usage/remaining`,
`/usage/timeseries`, `/usage/perf`.

Dashboard HTML has its own strong tag `"%x-%x-%s" % (st_mtime_ns, st_size,
ui_language)` (wb_proxy.py:19275-19312); no `Last-Modified` on purpose.

All other responses send **no** cache headers except downloads (`no-store`)
and SSE (`Cache-Control: no-cache`, `Connection: close`,
wb_proxy.py:22352-22358).

### 1.3 CORS (wb_proxy.py:371-385, 18632-18640)

`cors_origin_allowed(path)` (query stripped):

- false if path starts with any of `/v1/usage`, `/usage`, `/accounts`,
  `/settings`, `/tasks`, `/scheduler`, `/panel`, `/logs`;
- else true if it starts with `/v1`, `/chat`, `/completions`, `/models`,
  `/responses`.

When true: `Access-Control-Allow-Origin: *` on JSON, 429, SSE, download and
text responses. `OPTIONS <any path>` -> 204, `Content-Length: 0`, and if
allowed also `Access-Control-Allow-Headers: *`,
`Access-Control-Allow-Methods: GET, POST, OPTIONS`. No credentials header, no
`Vary`. `/v1/balance-summary` and `/v1/messages*` are CORS-enabled by prefix.

### 1.4 Body reading (wb_proxy.py:19314-19423, 18183-18307)

- `Transfer-Encoding: chunked` supported (decoded, size-capped; over cap
  drains then 413).
- Otherwise `Content-Length`; `> MAX_PAYLOAD_BYTES` -> 413
  `payload too large (%d bytes > %d limit)`; negative -> 400.
- Empty body -> `{}`.
- Invalid UTF-8/JSON -> 400 `invalid JSON body`.
- **[quirk]** Valid JSON that is not an object (array, string, number)
  silently becomes `{}` — except `/accounts/import`, where a bare array is
  allowed (`allow_list`).
- Errors on Anthropic routes (`/v1/messages`, `/messages`,
  `…/count_tokens`) use the Anthropic envelope.
- Any early rejection drains the unread body (`_discard_body`, handles chunked;
  if `Content-Length > MAX_PAYLOAD_BYTES` it closes instead) so the next
  keep-alive request is not parsed from leftover bytes. `Expect:
  100-continue` is answered with an interim 100 before draining.
- `BODYLESS_PANEL_POSTS` = `/console/toggle`, `/shutdown`, `/updates/check`,
  `/updates/install`, `/quota-scan/run`: body is drained before dispatch and
  never parsed (wb_proxy.py:17982-17990, 22035-22041).

A Rust server (hyper) drains/discards unread bodies itself; the only
behaviour to replicate is the error envelope and the non-object -> `{}` rule.

---

## 2. Authentication and authorisation

### 2.1 Auth classes used in the route tables

| Class | Meaning |
|---|---|
| **none** | No check at all |
| **key** | `_authorized()`: passes with a valid panel session **or** a matching enabled API key **or** when `auth_required()` is false. 401 OpenAI envelope otherwise |
| **key/anth** | Same as key but uses `_key_ok()` and answers 401 in the Anthropic envelope `authentication_error` "missing or invalid API key" |
| **panel** | Valid `X-Panel-Token` session required. API key is **not** enough. 401 `panel password required` (`invalid_request_error`) |
| **panel+local** | panel, plus `agents_client_allowed(client_address)`: peer is `127.0.0.1`/`::1`/`::ffff:127.0.0.1` and the process is not in a container/OpenWrt **[DROP?]** |

### 2.2 Gate order

GET (wb_proxy.py:18641-18730):

1. pinned prefix (0.3);
2. `/agents/cli` served with **no auth** (before the panel gate);
3. if `_is_panel_route(path)` and no panel session -> 401;
4. route table; unmatched -> 404 `not found`.

`_is_panel_route` (wb_proxy.py:18560-18592) is true for paths starting with
`/accounts`, `/usage`, `/v1/usage`, `/activity`, `/tasks`, `/scheduler`,
`/settings`, `/updates`, `/logs`, `/console`, `/pricing`, `/agents`.
Not panel routes: `/`, `/dashboard`, `/ui`, `/health`, `/panel/*`, `/realm`,
`/v1/models`, `/models`, `/v1/balance-summary`, `/proxy/*`, `/catalog/*`,
`/quota-scan/*`, `/shutdown` (the last four check the panel themselves).

POST (wb_proxy.py:22031-22229), evaluated in this order:

1. pinned prefix; drain body for `BODYLESS_PANEL_POSTS`;
2. explicit panel-only routes (`/console/toggle`, `/shutdown`,
   `/settings/save`, `/updates/check|install|rollback`,
   `/pricing/refresh|mapping`, `/catalog/refresh`, `/quota-scan/run`,
   `/proxy/*`, `/agents/apply|restore|model-selection`);
3. `/panel/login|logout|password` -> `_handle_panel` (own auth);
4. any other `_is_panel_route` path without a session -> 401;
5. if the path is not an "account route" (`/accounts/*`, `/realm`,
   `/tasks*`, `/scheduler*`, `/logs*`), not a messages route and not one of
   the OpenAI paths -> **404 before any auth check**;
6. messages routes: `_key_ok()` else Anthropic 401; others `_authorized()`;
7. messages routes: 400 if Anthropic Messages is disabled (setting
   `enable_anthropic_messages` OR env `WB_ENABLE_ANTHROPIC_MESSAGES` in
   1/true/yes/on, wb_proxy.py:17936-17940, 22172-22180);
8. read body (`allow_list` only for `/accounts/import`);
9. account routes -> `_handle_accounts` (503 `account pool unavailable` if
   `POOL is None`; 404 `unknown account endpoint` for an unknown sub-path);
10. LLM routes: `model_aliases` + background-model alias rewrite, per-key
    concurrency slot, global chat slot, dispatch.

Effective classes: every `/accounts/*`, `/tasks*`, `/scheduler*`, `/logs*`
POST is **panel** (step 4 rejects first). `POST /realm` is reachable with an
API key at step 6 but `_route_realm` then returns **403** "changing the
upstream exit requires the panel session, not an API key"
(wb_proxy.py:21014-21025).

### 2.3 API key extraction (`_supplied_key`, wb_proxy.py:18400-18440)

First non-empty wins:

1. `Authorization` header. Split on the first space; if the scheme is
   `bearer` (case-insensitive) take the rest, otherwise take the **whole
   header** as the key (so `Authorization: sk-xxx` works). Surrounding
   matching `"`/`'` quotes are stripped.
2. Headers `x-api-key`, `api-key`, `x-auth-token` (case-insensitive by
   `email.message`).
3. Query `?key=`, `?api_key=`, `?api-key=`.

There is no key format requirement other than length >= 4 on save. Panel keys
are free-form; LAN auto-generated keys are `"wb-" + token_urlsafe(24)`
(wb_settings.py:377-394).

### 2.4 Key matching (wb_proxy.py:539-560, wb_settings.py:564-738)

- `configured_keys()` = `wb_settings.api_keys(ACCOUNTS_DIR)` re-read on every
  call (mtime/size-cached JSON load), so panel edits apply immediately.
- `auth_required()` = `false` if `settings.auth_disabled is True`; else
  `true` if any configured key is enabled; else `bool(API_KEY)` (CLI/env
  launcher key).
- `identify_key(supplied)`: `match_api_key(supplied, extra_keys)` where
  `extra_keys = (API_KEY,)` **only when no panel keys exist at all** (once the
  panel has any key, even a disabled one, the launcher key stops working).
- `match_api_key`: constant-time compare (`hmac.compare_digest`) against each
  enabled, non-deleted entry -> copy with `source:"panel"`; then launcher key
  -> synthetic `{"id":"launcher","name":"启动参数","realm":"","models":[],"enabled":true,"source":"launcher"}`.
- Legacy single key: settings with `api_key_set:true` and no `api_keys` list
  read as one entry `{"id":"legacy","name":"默认（跟随面板切换）",…}`
  (wb_settings.py:601-613).
- **[quirk]** A panel session short-circuits `_key_ok()` before the key is
  inspected, so a dashboard request never carries a `key_entry` (no realm
  binding, no model limit) even if it also sends a key.

### 2.5 Key entry fields and enforcement

Stored entry (normalised by `_clean_key_entry`, wb_settings.py:502-535):

| Field | Type | Normalisation | Enforced where |
|---|---|---|---|
| `id` | str | default `token_hex(6)`; duplicates get `-2`, `-3`… (`_unique_key_id` 538-561) | usage log attribution, `/settings/reveal` |
| `name` | str | default `"未命名"` | log label (`key_label`, wb_proxy.py:577-597) |
| `key` | str | trimmed; empty only for deleted rows | auth |
| `realm` | `""`/`intl`/`cn` | anything else -> `""` | `_request_realm` (wb_proxy.py:18515-18533): explicit arg > key realm > `X-Realm` header > `?realm=` > global switch; cross-realm model -> 400 (`_cross_realm_error` 18464-18480) |
| `models` | list[str] | list or `,;\n` string; lowercase; dedupe; empty = unrestricted | `key_allows_model` (fnmatch, case-insensitive; empty model with a restriction = refused) -> 400 `key_model_message`; also filters `/v1/models` when `filter_models_by_key` |
| `accounts` | list[str] | exact uids, ≤200, ≤128 bytes each | `key_open_kwargs` -> `allowed_accounts` pool filter (wb_proxy.py:13082-13091); pinned account not allowed -> error (18504-18513) |
| `concurrency_limit` | int ≥0 | also accepts legacy `max_concurrency`; 0 = unlimited | `_acquire_key_slot` per key id, in-process counter; over limit -> 429 `Rate limit reached: API key '<name>' reached its concurrency limit (N in flight)` (wb_proxy.py:22192-22202, 22606-22638) |
| `enabled` | bool | `False` if deleted | auth |
| `created_at` | str `%Y/%m/%d %H:%M` | default now | display |
| `deleted_at` | str | non-empty = soft-deleted, secret wiped, read-only history | hidden from auth and from `api_keys` |

`_acquire_key_slot`/`_release_key_slot` are defined twice (wb_proxy.py:17948
and 22606) with identical bodies; the later one wins **[quirk]**.

### 2.6 Panel password and sessions

Storage in `settings.json` (wb_settings.py:317-358):

| Key | Meaning |
|---|---|
| `panel_password_hash` | hex PBKDF2-HMAC-SHA256 digest; `""` when default |
| `panel_password_salt` | 16 random bytes, hex (32 chars) |
| `panel_password_rounds` | 120000 (`PBKDF2_ROUNDS`) |
| `panel_password_default` | `true` when the password is the literal default |

- Default password `"admin"` (`DEFAULT_PANEL_PASSWORD`). Setting it back to
  `"admin"` stores no hash, `panel_password_default:true`.
- `verify_panel_password`: no hash or default flag -> compare to `"admin"`
  (plain `==`); else derive with stored salt/rounds and `compare_digest`
  (wb_settings.py:324-341). Derived key length = SHA-256 output (32 bytes),
  hex-encoded.
- Sessions: `PanelSessions` in memory only (wb_settings.py:2992-3030).
  Token `secrets.token_urlsafe(24)` (32 URL-safe base64 chars), TTL
  `SESSION_TTL = 7*24*3600`, absolute expiry (no sliding), expired tokens
  pruned lazily on lookup. Restart logs everyone out.
- Transport: **header only** `X-Panel-Token` (wb_proxy.py:18551-18557). No
  cookie. The dashboard keeps the token in `sessionStorage` (dashboard.html:7815,
  10663).
- `?pwd=` is purely client-side: the dashboard reads it from
  `location.search` and POSTs it to `/panel/login` (dashboard.html:10699-10703).
  The server never reads `pwd`. Likewise `?key=` is moved by the dashboard into
  `localStorage` and the URL is rewritten (dashboard.html:7817-7833); the
  server additionally accepts `?key=` as an API key (2.3).
- Rotation: `/panel/password` with a non-default new password calls
  `PANEL.revoke_all()` then issues a fresh token for the caller.
- `--panel-password X` overwrites the stored password at every start
  (wb_proxy.py:22796-22800).

### 2.7 Login rate limiting (wb_proxy.py:419-513, 20508-20534)

- Bucket key = `login_rate_limit_key(peer_ip, headers)`:
  - if the peer is trusted (loopback incl. IPv4-mapped `::ffff:127.x`, or
    inside a CIDR of env `WB_TRUSTED_PROXIES`, comma list, invalid entries
    dropped): use `X-Real-IP` if it parses as an IP, else the first element
    of `X-Forwarded-For` if it parses, else the peer IP;
  - untrusted peer: always the peer IP (headers ignored).
  - `_proxy_header_ip` accepts bare v4/v6, `v4:port`, `[v6]:port`; rejects
    commas, >64 chars, garbage; output is canonical `str(ip_address)`.
- Window: failures in the last 60 s per key; at >=5 -> 429
  `too many login attempts, please wait Ns` (`rate_limit_error`, N = 60 -
  age of the oldest).
- Each failure appends a timestamp and sleeps 0.5 s before 401
  `invalid panel password`. Success clears the bucket. `_prune_login_attempts`
  drops stale buckets on every attempt.
- Only `/panel/login` is rate-limited. `/settings/save` with
  `identity_versions` and `/panel/password` also verify the password but are
  not rate-limited (they already require a session).

### 2.8 `--lan` and the launcher key (wb_proxy.py:22699-22705, 22783-22795)

- `--lan` with the default host rewrites `--host` to `0.0.0.0`.
- If `--lan` and no `--api-key`/`API_KEY`/`WB_PROXY_KEY`:
  `ensure_launcher_key()` reuses or mints `settings.launcher_key`
  (`wb-<token_urlsafe(24)>`) and uses it as `API_KEY`.
- Then, if the panel saved a single key (`api_key_set:true`) and no explicit
  `--api-key` was given, that saved key replaces `API_KEY`
  (`API_KEY_FILE_SET=True`).
- Precedence: `--api-key` > panel single key > LAN launcher key > none.
- Note that once any panel multi-key exists, `API_KEY` is ignored for
  matching (2.4), so the LAN key only protects a fresh install.
- `--lan` does not change CORS, panel auth or anything else. The startup banner
  prints `http://<ip>:<port>/?key=<API_KEY>` (wb_proxy.py:22904-22939).
- The Docker image always runs `--host 0.0.0.0 --lan` (Dockerfile `CMD`).

### 2.9 `/health` semantics (wb_proxy.py:18772-18793)

Always 200, no auth. Body:

```json
{"ok": true, "realm": "intl", "accounts": 3, "accounts_ready": 2,
 "api_key_required": true}
```

If `_key_ok()` (panel session, valid key, or auth not required) it adds
`uid`, `domain`, `issuer` (JWT `iss` of the representative account),
`credential_file` (basename), `expires_at` (epoch) of
`POOL.representative()`. Consumers: Docker `HEALTHCHECK`, the launcher's
`_probe_running_instance` (treats a JSON body **without** `accounts`, or with a
`service` key, as a foreign service, wb_proxy.py:22725-22768), and the
dashboard's shutdown confirmation loop (dashboard.html:7893).
`/health` does not check upstream reachability or account readiness; it
is a liveness probe only.

---

## 3. Route catalogue

Columns: **M** method, **Auth** (2.1), **Handler** (`wb_proxy.py` line
unless noted), **Cache** (`E` = ETag/304 via 1.2, `-` = none), **Writes**
(files touched). Response shapes are compact; `{…accounts}` means the
`account_views()` list of `Account.public()` objects (3.6).

Totals: **87 routes** = 35 GET + 51 POST + `OPTIONS *`, counting alias
groups once (`/`,`/dashboard`,`/ui`; `/v1/models`,`/models`;
`/usage`,`/v1/usage`; each `/v1/x` + bare `/x` LLM pair;
`/accounts/credits` + `/accounts/credits/fetch`). Counting every alias
separately: 39 GET paths + 58 POST paths = 97 method+path pairs, plus
OPTIONS. Every one of them also accepts the `/u/<ref>/` prefix (0.3).

### 3.1 OpenAI / Anthropic API

| M | Path (aliases) | Auth | Handler | Cache | Notes |
|---|---|---|---|---|---|
| POST | `/v1/chat/completions`, `/chat/completions` | key | `_dispatch_chat_post` 22231 | - | Spec 03. Concurrency: per-key slot then global chat slot. Session key headers `X-Conversation-Id`, `Conversation-Id`, `X-Session-Id`, `Session-Id` or body `conversation_id`/`session_id`/`metadata.conversation_id` (`extract_session_key` 15266) |
| POST | `/v1/completions`, `/completions` | key | `_dispatch_chat_post` 22231 | - | **[quirk]** Same chat path, no legacy `prompt` translation (spec 03 §0.3) |
| POST | `/v1/responses`, `/responses` | key | `_handle_responses` 21482 | - | Spec 04. `previous_response_id` (non-compaction) -> 400 |
| POST | `/v1/responses/compact`, `/responses/compact` | key | `_handle_responses` 21482 | - | Compaction mode (spec 04) |
| POST | `/v1/messages`, `/messages` | key/anth | `_handle_messages` 21856 | - | Off by default -> 400 Anthropic envelope (2.2 step 7). Spec 04 |
| POST | `/v1/messages/count_tokens`, `/messages/count_tokens` | key/anth | `_handle_messages_count_tokens` 21842 | - | `{"input_tokens": int}` local CJK-aware estimate; still behind the Messages switch and the per-key slot, not the chat slot |
| GET | `/v1/models`, `/models` | key | `_get_v1_models` 18800 | - | see below |
| GET | `/v1/balance-summary` | key | `_get_v1_balance_summary` 18855, `credit_balance_summary` 6078 | - | In-memory credit snapshot only |

All `/v1/*` LLM routes accept the `/u/<ref>/` prefix (0.3).
LLM POSTs first apply `settings.model_aliases` (`_apply_model_aliases` 337)
and the background-model alias (`_apply_background_model_alias` 316,
env `WB_BACKGROUND_MODEL_TARGET` default `deepseek-v4.1-flash`,
`WB_BACKGROUND_MODEL_NAMES` default `gpt-5.6-terra,gpt-5.6-luna,gpt-5.3-codex`,
wb_proxy.py:305-312).

`GET /v1/models` query: `realm` (also `X-Realm`; key binding wins, 2.5).

```json
{"object":"list","realm":"intl",
 "data":[{"id":"gpt-5.5","object":"model","created":1761200000,"owned_by":"workbuddy",
          "name":"…","description":"…",
          "capabilities":{"vision":true,"tool_calls":true,"reasoning":true},
          "supports_vision":true,"supports_images":true,"supports_tool_calls":true,
          "supports_reasoning":true,"vision":true,"multimodal":true,
          "abilities":{"vision":true,"functionCall":true,"function_call":true,"reasoning":true},
          "input_modalities":["text","image"],"output_modalities":["text"],
          "context_length":400000,"max_input_tokens":…,"max_output_tokens":…,
          "max_completion_tokens":…,"reasoning_efforts":["low","medium","high"],
          "reasoning_default_effort":"medium","credits":…,"vendor":"…","tags":[…],
          "entitlement":{"source":"catalog","required":"standard","at":1761190000}}],
 "catalog":{"source":"remote|cache|legacy|endpoint|bundled","fetched_at":…,"file_mtime":…}}
```

Full per-model field list: `model_entry` wb_proxy.py:10903-11060 (also
`architecture`, `modalities`, `context_windows`, `is_default`,
`always_reasoning`, `reasoning_can_disable`, `reasoning_fixed_effort`,
`temperature`, `top_p`, `output_clamp`, `max_output_tokens_clamped`,
`vision_note`). Filters applied in order: pinned account's
`disabledModels`; `filter_models_by_key` + key `models`;
`models_only_available`; then `entitlement` added for PRO-only models
(`wb_tiers.requires_pro`). Catalogue cache 300 s per realm
(`fetch_models` 10861). Side effect: `wb_modelsdev.refresh_async` may write
`accounts/model.json` (models.dev index) in the background. 502 on fetch error.

`GET /v1/balance-summary`:

```json
{"balance":1234.5,"unit":"credit","realm":"intl","accounts_enabled":4,
 "accounts_with_credit_data":3,"updated_at":1761200000.0}
```

`balance` is `null` when no account has a known `credits.remain`. No account
identity is returned. `realm=all` sums both.

### 3.2 Health, realm, dashboard static, console

| M | Path | Auth | Handler | Cache | Response / effect |
|---|---|---|---|---|---|
| GET | `/health` | none (identity fields need key) | `_get_health` 18772 | - | 2.9 |
| GET | `/realm` | none | `_get_realm` 18797 | - | `{"current":"intl","options":["intl","cn"]}` |
| POST | `/realm` | panel (key -> 403) | `_route_realm` 21014 | - | body `{"realm":"intl"\|"cn"}`; other values ignored. Writes `accounts/active_realm.json` `{"realm","updated_at","updated_iso"}` (`save_persisted_realm` 6032). Reply `{"ok":true,"current":…,"persisted":true}` |
| GET | `/`, `/dashboard`, `/ui` | none | `_dashboard` 19251 | E (own tag) | `text/html; charset=utf-8`. Replaces the literal `data-ui-language="__WB_UI_LANGUAGE__"` with the `ui_language` setting. 500 JSON if the file is missing |
| GET | `/console/state` | panel | `_get_console_state` 18747 | - | constant `{"supported":true,"mode":"web","visible":false}` **[DROP?]** legacy |
| POST | `/console/toggle` | panel | `_route_console_toggle` 18752 | - | constant `{"ok":true,"mode":"web","url":"/dashboard?tab=logs","visible":false}` **[DROP?]** legacy; the dashboard now just opens `/dashboard?tab=logs` |
| POST | `/shutdown` | panel | `_route_shutdown` 18758 | - | `{"ok":true,"shutting_down":true}`, then `os._exit(0)` after 0.4 s. On a systemd/Docker VPS the supervisor restarts it; keep or drop by policy |

No `/favicon.ico` route (404 JSON); it is only in the log-quiet list.

### 3.3 Panel auth

All POST, JSON body, own auth (`_handle_panel` 20503).

| Path | Auth | Body | Response |
|---|---|---|---|
| GET `/panel/status` | none | – | `{"panel_password_required":true,"panel_password_is_default":bool,"authenticated":bool,"api_key_set":bool}` (`_get_panel_status` 18734; `api_key_set` reflects only the launcher/single `API_KEY`) |
| POST `/panel/login` | none, rate-limited (2.7) | `{"password":str}` | `{"ok":true,"token":str,"using_default_password":bool}`; 401 / 429 |
| POST `/panel/logout` | none | `{}` | revokes the `X-Panel-Token` given (if any); `{"ok":true}` always |
| POST `/panel/password` | panel | `{"current":str,"new":str}` | 401 `current password is wrong`; 400 if `len(new)<4`; writes `settings.json`; non-default new -> revoke all sessions; `{"ok":true,"token":str,"using_default_password":bool}` |

### 3.4 Settings and API keys

| M | Path | Auth | Handler | Cache | Notes |
|---|---|---|---|---|---|
| GET | `/settings` | panel | `_get_settings` 19143 -> `runtime_settings_view` 10104 | - | 5.2 |
| GET | `/settings/reveal?id=<key id>` | panel | `_get_settings_reveal` 19240 | - | `{"id":…,"key":"<plaintext>"}`; 404 `no such key` |
| POST | `/settings/save` | panel | `_handle_settings_save` 19424 | - | Partial update; every key optional. Writes `accounts/settings.json`. Reply = per-key echo + full `runtime_settings_view()`. 5.3 |

### 3.5 Proxy slots (outbound egress proxies)

All **panel**. The GET is checked inline (it is not a `_is_panel_route`
path); POSTs go through `_handle_proxy_slots` 20116 after the generic body
read.

| M | Path | Body | Response / effect |
|---|---|---|---|
| GET | `/proxy/slots` (18710) | – | `{"slots":[slot…]}` |
| POST | `/proxy/slots` | any | same as GET |
| POST | `/proxy/slots/save` | `{"slots":[{id?,name,url,enabled,…}]}` (400 if not a list) | Each entry normalised by `wb_settings._clean_slot_entry`; dedupe by `url`; new ids `slot-<n>` from the monotonic `proxy_slot_seq`. Unbinds accounts naming removed slots (writes their account files), reapplies to the pool. Writes `settings.json`. -> `{"slots":[…]}` |
| POST | `/proxy/slots/test` | `{"id":"slot-1"}` | 404 `no such proxy slot`. Probes exit IP via the proxy then geo/ASN (`probe_proxy_intel` 6338). On success stores `ip`, `probed_at`, and (if geo answered) `country,country_code,ip_type,isp,asn` into the slot. -> `{ok,exit_ip,latency_ms,error,intel_ok,country,country_code,ip_type,isp,asn,id,slot,name}` |
| POST | `/proxy/discover` | `{}` | Probes `http://$WB_PROXY_DISCOVER_HOST:<port>` for each port in `WB_PROXY_DISCOVER_PORTS` (defaults `cli-proxy-mihomo`, `17901-17910`, wb_proxy.py:6311-6330, `_proxy_port_range` 6314). -> `{"candidates":[{…probe,url,reachable}]}` |

Slot object: `{"id":"slot-1","name":"","url":"http://…","enabled":true,"ip":"","country":"","country_code":"","ip_type":"","isp":"","asn":"","probed_at":0,"bound":2,"label":"…"}`
(`bound` = enabled accounts bound to it, `label` = name or derived
country/type or id; `proxy_slots_view` 6390, `slot_label` 6381).
Any other `/proxy/<x>` POST -> 404.

### 3.6 Accounts management

All **panel** (GET via the panel gate, POST via step 4 of 2.2). All responses
go through `_json_panel` (phone masking) unless noted. POSTs need
`POOL != None` (else 503). `uid` refers to `Account.uid`.

`Account.public()` object (wb_accounts.py:1082-1159), referred to as
**ACCT**: `uid, nickname, domain, realm, platform, product, enterpriseId,
enabled, tasksEnabled, weight, note, disabledModels[], source, apiKey(bool),
credentialMask, proxySlot, proxy, expiresAt, expiresIn("expired"|"3d 4h"|null),
tokenExpired, needRelogin, hasRefreshToken, lastError, lastErrorDetail,
inCooldown, cooldownFor, restrictedFor, modelCooldowns, softStreak,
unscopedStreak, breakerFor, degradeFor, fails, failWindow, lastFailFor,
addedAt, file, credits{…}, tier, reserveCredits, reserveBlocked,
dailyTokenLimit, dailyTokensToday, dailyLimitBlocked, dailyCreditLimit,
dailyCreditsToday, creditLimitReached, modelDailyTokenLimit,
modelDailyTokens{}, lastCheckin, lastDailyChat, lastActivityReport,
canCheckin, canDailyChat, canReportActivity, machineId, sessionId`.
`credits` snapshot keys (when fetched): `remain, size, used, remain_percent,
used_percent, packages[], earliest_expiring, is_enterprise, is_paid_user,
checkin, updated_at, updated_iso`.

| M | Path | Handler | Request | Response | Writes |
|---|---|---|---|---|---|
| GET | `/accounts?realm=` | `_get_accounts` 18972 | `realm` (default current; `all` = both) | `{"accounts":[ACCT],"storage":"<ACCOUNTS_DIR>","usable":int}`. Runs `apply_daily_guards()` first | – |
| GET | `/accounts/export?realm=&uid=&secrets=&download=` | `_get_accounts_export` 18984 | `realm` intl/cn else all; `uid` repeatable or comma list (404 `no such account: …` for unknown); `secrets` default `1` (`0/false/no` strips tokens); `download=1/true/yes` -> attachment `workbuddy-accounts-<uid8|realm->-YYYYmmdd-HHMMSS.json` | `{"format":"workbuddy-accounts","version":…,"exportedAt":…,"count":n,"accounts":[{"account":…,"auth":…}]}` (`wb_accounts.build_export_document`). Nickname masked even with secrets **[quirk]** | – |
| GET | `/accounts/credits` | `_get_accounts_credits` 18912 | – | Refreshes **all** accounts' credits upstream (paced sweep), unless a sweep is already running -> `{"accounts":[ACCT],"busy":bool}` **[quirk]** a GET with upstream side effects | account files |
| POST | `/accounts/credits`, `/accounts/credits/fetch` | `_route_accounts_credits_fetch` 20671 | `{uid?}` or `{realm?:"intl"\|"cn"\|"all"}` | `{"results":[{uid,ok,credits,error}],"accounts":[ACCT]}`; multi-account and sweep busy -> `{"results":[],"accounts":…,"busy":true}`. Gap between accounts env `WB_CREDITS_SWEEP_GAP_MS` (default 0.8-1.2 s) | account files |
| GET | `/accounts/credits/detail?uid=&refresh=` | `_get_account_credits_detail` 18938 | unknown/missing uid -> first account | `{"ok":true,"uid","nickname","realm","credits"}` or `{"ok":false,…,"error"}` (always 200). Fetches upstream if `refresh` in 1/true/yes or no packages cached | account file |
| POST | `/accounts/credits/detail` | `_route_account_credits_detail` 20694 | `{uid, refresh=true}` | same; unknown uid -> `{"ok":false,"error":"未找到指定账号"}` | account file |
| GET | `/accounts/credits/grants` | `_get_account_credits_grants` 18927, `credit_grants` 6204 | – | `{"ok","generated_at","generated_iso","summary":{…},"accounts":[{uid,nickname,realm,fetched_at,fetched_iso,count}],"rows":[{uid,nickname,realm,name,product,size,used,remain,unit,status:"active\|available\|used_up\|expired",create_at,create_iso,expire_at,expire_iso,days_left,no_expiry,grant_reason,action}]}`; memory only | – |
| GET | `/accounts/login/poll?state=` | `_get_accounts_login_poll` 19028, `AccountPool.poll_login` wb_accounts.py:4538 | `state` | `{"status":"pending\|ok\|error\|expired\|unknown","message"?, "account"?:ACCT}`. On ok: adds account (writes file), cn accounts auto-checkin. Login window `LOGIN_TTL_SECONDS=600` | account file |
| POST | `/accounts/login/start` | `_route_accounts_login_start` 21101 | `{realm?, platform?="CLI"}` | `{"state","authUrl","realm","platform"}` (not masked); 502 on upstream failure / TLS | – (in-memory login table) |
| POST | `/accounts/login/cancel` | 21115 | `{state}` | `{"cancelled":bool}` | – |
| POST | `/accounts/refresh` | 21184 | `{uid?}` (none = all) | `{"results":[{uid,ok,error}]}` | account files |
| POST | `/accounts/sync-profile` | 21196 | `{uid?}` or `{realm?}` | `{"updated":[{uid,nickname}],"failed":[{uid,error}],"accounts":[ACCT]}` | account files |
| POST | `/accounts/test` | `_route_accounts_test` 21229 | `{uid, model?="deepseek-v4.1-flash"}` | `{"ok","uid","model","elapsed_ms","reply"?, "status"?, "error"?, "network"?:true}` (not masked); 404 unknown uid | – (clears failure counters on success) |
| POST | `/accounts/test` (no uid) | `_route_accounts_test_all` 21256 | `{realm?, model?}` | `{"ok":true,"summary":{realm,model,tested,passed,failed,networkDown,elapsed_ms},"network":{…},"results":[row],"accounts":[ACCT]}`; probes enabled accounts of the realm, `PROBE_CONCURRENCY=4` | – |
| POST | `/accounts/set` | `_route_accounts_set` 21289 | `{uid, proxySlot?, proxy?, enabled?, tasksEnabled?, weight?(1-100), note?, disabledModels?[]}` each applied independently | `{"account":ACCT}`; 400 `uid required`, 400 weight range, 404, 400 `nothing to update…` | account file |
| POST | `/accounts/product` | `_route_accounts_product` 20623 | `{product:"workbuddy"\|"vscode"\|"cli", uid? \| realm?}` | `{"ok":true,"product","changed":[uid8],"accounts":[ACCT]}`; 400 otherwise | account files |
| POST | `/accounts/set-all` | 21354 | `{enabled:bool}` | `{"accounts":[ACCT]}` | account files |
| POST | `/accounts/delete` | 21358 | `{uid}` | `{"deleted":bool,"accounts":[ACCT]}`; deletes the account file | account file removed |
| POST | `/accounts/import` | `_route_accounts_import` 21433 | Body: export document, bare array, single account object, or `{account,auth}` desktop credential; or `{"data":<doc>, dryRun?, overwrite?, realm?}` | dry run `{"dryRun":true,"count","result":{added,updated,skipped,invalid},"accounts"}`; real `{"count","result":{"added":[…],"updated":[…],"skipped":[{uid,reason}],"invalid":[{index,reason}]},"accounts"}`; 400 `realm must be intl or cn`, 400 `cannot read the document: …` | account files |
| POST | `/accounts/add-api-key` | 21366 | `{key \| accessToken, nickname?, uid?}`; key must have `ck_`/`pt_` prefix | Probes the cn enterprise billing endpoint first; on failure 400 and nothing written. `{"ok":true,"uid","nickname","updated":bool,"probe":{ok,used,limit,cycle_end},"account":ACCT}` | account file |
| POST | `/accounts/import/desktop` | `_route_accounts_import_desktop` 21119 | `{}` scan / `{path, realm?}` / `{all:true, realm?}` / `{recoverKey:true, force?}` / `{keyText:str}` / `{forgetKey:true}` | scan `{"detected":[…],"accounts","pool_uids","atrest":{…}}`; import `{"imported":[ACCT],"accounts"}`; key ops `{"ok","keyId"?,"msg","atrest"}` | account files |

`/accounts/import/desktop` reads the WorkBuddy **desktop app's** credential
files on the gateway host and (Windows only) recovers the at-rest key from the
running client's memory (`wb_atrest`). **[DROP?]** on a Linux VPS: there is
no desktop client. The CLI equivalents `--info`, `--import-desktop`, and the
first-run desktop scan (wb_proxy.py:22848-22877) are **[DROP?]** too.
The `--info <file>` import of a credential *file* could be kept as a plain
JSON import.

### 3.7 Usage and analytics

All **panel** (`/usage*`, `/v1/usage`, `/activity*`). Shared query params
`range`, `since`, `until` (`range_query` 784 -> `range_window` 747):
`today|day|1d` (local midnight), `week|w` (Monday 00:00), `month|m`
(1st 00:00), `custom` (epoch `since`/`until`, swapped if reversed, negatives
dropped), anything else = all time. `realm`: `intl`, `cn`, `all`
(missing -> header `X-Realm` -> current realm, except analytics where missing
= both).

| M | Path | Handler | Extra params | Cache | Response (top-level keys) |
|---|---|---|---|---|---|
| GET | `/usage`, `/v1/usage` | `_get_v1_usage` 18861, `usage_snapshot` 3288 | – | E | `requests, errors, prompt_tokens, completion_tokens, reasoning_tokens, cached_tokens, total_tokens, credit, cost_cny, cost_missing{}, by_model{}, by_model_realm{}, by_model_acct{}, ttft/wall/gen sums+samples, started, since, log_file, realm, usd_cny, accounts_map{uid:{nickname,realm}}, account{uid,domain,issuer,credential_file,expires_at,accounts,accounts_ready}` |
| GET | `/usage/recent` | `_get_usage_recent` 18875, `recent_usage` 5818 | `limit` 1-1000 (100), `page` ≥1 | - | `{"rows":[usage row],"total","page","limit","total_pages","usd_cny"}` |
| GET | `/usage/analytics` | `_get_usage_analytics` 19034 | – | E | `{"realm","window","summary":{"window":{…},"all_time":{…}},"accounts":[…],"models":[…],"keys":[…],"usd_cny"}` |
| GET | `/usage/by-account` | 19046 | – | E | `{"accounts":[…]}` |
| GET | `/usage/remaining` | 19053, `_remaining_payload` 2883 | – | E (TTL `WB_REMAINING_TTL` 15 s) | `{"window_seconds":86400,"generated_at","generated_iso","activity_floor","activity_floor_iso","budgets":[…],"models":[{realm,model,total_remaining,budget,pairs,available,cooling,unknown,estimated}],"rows":[…]}` |
| GET | `/usage/perf` | 19080, `perf_stats` 1060 | `sample` 10-20000 (5000) | E | `requests, success, errors, client_aborted, success_rate_pct, cache_hit_pct, ttft_ms{p50…}, generation_ms, wall_ms, tokens_per_sec, sampled, sample_capped, sample_from, by_model, by_model_realm, by_model_acct, …` |
| GET | `/usage/timeseries` | 19060, `usage_timeseries` 4717 | `bucket` (seconds) | E | `{"ok":true,"realm","bucket_seconds","since","until","series":[…], …}`; auto bucket minute (≤6 h) / hour (≤14 d) / day |
| GET | `/activity/history` | `_get_activity_history` 18889, `wb_activity.query` wb_activity.py:285 | `range` today/1d/7d/30d/90d/all (7d), `uid`, `task` checkin/daily_chat, `result` ok/failed (+aliases), `limit` (100, max 1000) | - | `{"rows":[{ts,uid,nickname,realm,task,trigger,ok,message}],"total","limit","range","filters":{uid,task,result}}`; 400 on unknown filter |

**Mismatch:** the dashboard requests `/usage/timeseries?…&bucket_seconds=86400`
(dashboard.html:16583, heat map) but the handler only reads `bucket`
(wb_proxy.py:19066). The parameter is ignored and the auto bucket applies (a
366-day custom window resolves to day buckets anyway, so the output matches by
accident). A port should accept both names.

Usage-row shape (`usage.jsonl`, `record_usage` 836): `at, iso, model,
requested_model, realm, account, key, stream, outcome, status, error, message,
elapsed_ms, ttft_ms, gen_ms, tokens_per_sec, cache_hit_pct, reasoning_effort,
usage_missing, cost_policy`, token fields `prompt_tokens, completion_tokens,
reasoning_tokens, cached_tokens, total_tokens, credit`, plus prompt
fingerprint fields. Details belong to the usage spec.

### 3.8 Pricing

All **panel** (`/pricing` prefix).

| M | Path | Handler | Request | Response | Writes |
|---|---|---|---|---|---|
| GET | `/pricing` | `_get_pricing` 19124, `PriceRefresher.status` wb_pricing.py:2576 | – | `{interval_minutes, enabled, running, master_enabled, policies, models, current{}, current_at, current_at_label, last_run, last_run_label, next_run, next_run_label, last_error, last_models, last_extra, logs[], gaps[], gap_summary{total,or_missing,variant_unmatched,aliases,variants_enabled}, variant_inherit, overrides_file, policies_file, timeline}` (absolute paths exposed) | – |
| POST | `/pricing/refresh` | `_route_pricing_refresh` 20944 | `{}` | Starts `PRICING.run_once` on a thread; `{…status,"ok":true,"msg"}`; master switch off -> `{"ok":false,"msg":"价估算已关闭，请先启用"}` (200) | `usage/pricing-policies.jsonl`, `usage/pricing-timeline.jsonl` |
| POST | `/pricing/mapping` | `_route_pricing_mapping` 20974 | `{model, or_id?}`; `or_id` must match `^[A-Za-z0-9._-]+/[A-Za-z0-9._:-]+$`; empty = delete | `{"ok":true,"model","or_id","known":bool,"msg"}`, then triggers a refresh | `usage/pricing-overrides.json` |

The `/pricing/refresh` branch inside `_handle_accounts` (20586) is
unreachable: the explicit route at 22083 runs first.

### 3.9 Catalog, quota scan

| M | Path | Auth | Handler | Request | Response | Writes |
|---|---|---|---|---|---|---|
| POST | `/catalog/refresh` | panel | `_route_catalog_refresh` 20958, `refresh_catalog` 11287 | `{realm:"intl"\|"cn"}` | always 200 `{"ok":bool,"realm","source":"remote"\|null,"count","file_mtime"?,"cooldown"?,"msg"}`; 30 s cooldown per realm | `catalog/product-config-<realm>.json` (archive copy, never read back) |
| POST | `/quota-scan/run` | panel | `_route_quota_scan_run` 20914 | none (bodyless) | `{"ok":true,"sent":int,"msg"}`; 503 no pool; 500 on exception | `accounts/quota_scan.json` |

No GET exists for catalog or probes. Catalog metadata rides on `/v1/models`
(`catalog`); quota-scan state on `/settings` (`quota_scans`) and on
`/scheduler` (`quota_scans`, `windows`).

### 3.10 Tasks, check-in, daily chat, scheduler

All **panel**. Growth tasks exist only for `cn` accounts; daily chat only for
`intl`. Shapes masked via `_json_panel`. Upstream work is synchronous inside
the request (a full `/tasks/run` over many accounts can take minutes).

| M | Path | Handler | Request | Response | Writes |
|---|---|---|---|---|---|
| GET | `/tasks?uid=` | `_get_tasks` 19096, `growth_snapshot` 6281 | `uid` (cn) or `all`/missing -> first cn task account | `{"tasks":[…],"summary":{…},"account":ACCT,"accounts":[{uid,nickname}]}`; no cn accounts -> `{"tasks":[],"summary":{},"accounts":[],"msg":"未找到参与任务的国内版账号"}`. Cache `WB_TASKS_CACHE_TTL` (20 s) per uid | – |
| POST | `/tasks/run` | `_route_tasks_run` 20725 | `{uid? \| "all", claim?=true}` | `{"ok":true,"credit_added":int,"logs":[str],"accounts_count":int}`; errors as `{"ok":false,"msg"}` (200). 1.5 s between accounts | account files, activity log |
| POST | `/tasks/claim` | 20765 | `{uid (required, not all), code}` | `{"ok":bool,"msg","credit"}` | account file |
| POST | `/tasks/travel` | 20812 | `{uid?}` | `{"ok":true,"results":[{uid,nickname,action,msg,reward_credit}],"msg","accounts_count"}` | account files |
| POST | `/tasks/streak-bonus` | 20786 | `{uid?}` | `{"ok":true,"msg":"<nick>: <msg>\n…"}` | account files |
| POST | `/accounts/checkin` | `_route_accounts_checkin` 21027 | `{uid?}`; none = cn, task-active, non-enterprise accounts | `{"results":[{uid,nickname,ok,msg\|error,…}],"accounts":[ACCT]}` | account files, `usage/activity_history.jsonl` |
| POST | `/accounts/daily-chat` | 21049 | `{uid?}` or `{uids:[…]}` (exact list; empty list = nobody) | `{"results":[…],"accounts":[ACCT]}` | account files, activity log |
| POST | `/accounts/daily-chat-web` | 21079 | `{uid?}` | `{"results":[{uid,nickname,ok,conversation?,error?}],"accounts"}`; does **not** set `lastDailyChat` | activity log |
| GET | `/scheduler` | `_get_scheduler` 19119, `Scheduler.status` wb_scheduler.py:534 | – | `{"enabled","mode","mode_cn","mode_intl","last_run_time","next_run_time","windows":[≤10],"quota_scans":[≤6],"logs":[≤20]}`; no scheduler -> `{"enabled":false,"msg":"未运行"}` | – |
| POST | `/scheduler/trigger` | 20909, `trigger_now` wb_scheduler.py:327 | `{}` | `{"ok":true,"msg":"已触发后台调度执行"}` or `{"ok":false,"msg":"已有巡检正在执行…"}`; cycle runs on a thread | via cycle |
| POST | `/scheduler/toggle` | 20937 | `{}` | flips `SCHEDULER.enabled` (**in memory only**, lost on restart **[quirk]**); returns `status()` | – |

### 3.11 Logs

All **panel**.

| M | Path | Handler | Request | Response |
|---|---|---|---|---|
| GET | `/logs?limit=&level=&tag=&search=&since_id=` | `_get_logs` 19192, `get_logs` 10586 | `limit` (200; ignored when `since_id>0`), `level` exact (upper-cased), `tag` case-insensitive exact, `search` substring of msg/tag, `since_id` | `{"total","logs":[{id,ts,time?,level,tag,msg}],"max_id","mode":"db"\|"memory","stored"?,"db_bytes"?}`. Memory ring = last 2000 entries (`LOG_BUFFER`); with `log_db_enabled` reads `usage/logs.sqlite3` |
| GET | `/logs/export` | `_get_logs_export` 19219 | – | `text/plain; charset=utf-8` attachment `wb-proxy-YYYYmmdd-HHMMSS.log`, last 5000 lines `[ts] [LEVEL] [tag] msg`, phone-masked. No `Cache-Control` |
| POST | `/logs/clear` | `_route_logs_clear` 20970 | `{}` | clears memory ring and the DB table; `{"ok":true}` |

### 3.12 Updates **[DROP?]**

All **panel**. Release discovery talks to GitHub (`WB_UPDATE_REPO`). Install
and rollback only queue a request for an executor: the systemd unit
`workbuddy2api-hub-updater.path` (request dir `WB_UPDATE_REQUEST_DIR`, default
`/run/workbuddy2api-hub-updater`), an external executor
(`WB_UPDATE_EXECUTOR=external` or `executor.json`), or the portable bundle
updater. A Rust binary would likely be updated by the package manager or a
container pull instead. Keep `GET /updates` (version + latest release) if the
"update available" badge is wanted; the install/rollback/backups trio is
**[DROP?]**.

| M | Path | Handler | Request | Response |
|---|---|---|---|---|
| GET | `/updates` | `_get_updates` 19148 | – | `{current_version, latest_version, latest_tag, update_available, enabled, checking, last_attempt, last_success, last_error, release_url, published_at, msg?}` + `wb_update_request.status()` (`install_enabled`, `installation{state,version,message,requested_at,pid,port,root}`) |
| GET | `/updates/backups` | `_get_update_backups` 19171 | – | `{"source":"systemd"\|"local"\|"","backups":[{version,bytes,…}],"current_version","keep_backups","keep_backups_max":3,"rollback_enabled"}` |
| POST | `/updates/check` | `_route_update_check` 20852 | bodyless | runs one GitHub check inline (ignores the daily switch) -> status + `ok:true`; no checker -> `{"ok":false,"msg":"更新检查未运行"}` |
| POST | `/updates/install` | `_route_update_install` 20866 | bodyless | 202 `{"state":"queued","version","message","ok":true}`; 409 `{"ok":false,"msg"}` (no check / no newer / already running); 503 no checker |
| POST | `/updates/rollback` | `_route_update_rollback` 20889 | `{version}` | 202 queued / 409 |

Writes: `settings.json` (`update_check` state), request dir `request.json`.

### 3.13 Agents (one-click client configuration) **[DROP?]**

`wb_agents` writes client config files (Claude Code `~/.claude/settings.json`,
Codex `~/.codex/…`, OpenCode, Crush, DSH, ZCode `~/.zcode/v2/provider_config.json`)
**on the machine that runs the gateway**. It only works when the browser is on
the same host, so on a VPS it is dead weight.

| M | Path | Auth | Handler | Response |
|---|---|---|---|---|
| GET | `/agents/available` | panel | inline 18726 | `{"enabled":bool}` (loopback peer and not container/OpenWrt) |
| GET | `/agents` | panel | `_get_agents` 20322 | not local -> `{"enabled":false,"reason":…}`; else `{"enabled":true,"clients":[{id,label,desc,installed,configured,applied,at,base_url,model,config_paths,files,protocol,external_change}],"models":[{id,name?,context_window?,max_output?,supports_vision?,reasoning_efforts?}],"selected_models":[…],"models_configured":bool,"model_embedding_clients":["crush","dsh","opencode","zcode"],"keys":[{id,name,enabled}],"global_key_set","auth_required","gateway":{"base_url":"http://127.0.0.1:<port>/v1"}}` |
| GET | `/agents/model-selection` | panel | 20361 | `{"configured":bool,"selected_models":[…]}` |
| POST | `/agents/model-selection` | panel | 20486 | body `{models:[str]}` (400 if missing/not list) -> `{"ok":true,"configured":true,"selected_models":[…]}`; writes `settings.agent_models` (≤200) |
| POST | `/agents/apply` | panel+local (403) | 20395 | body `{client\|client_id, base_url (http/https), model?, models?[≤80], key_id?("__global" or id)}` -> `{client,base_url,model,files:[{path,existed,backup,bytes}],applied_at,warnings[]}`; writes client files + `accounts/agent-backups/`, `accounts/integration-state.json` |
| POST | `/agents/restore` | panel+local | 20471 | body `{client}` -> `{…,"restored":[{path,action}]}` |
| GET | `/agents/cli` | **none** | `_get_agents_cli` 20296 | `text/plain` source of `wb_agents.py` (`Content-Disposition: inline`) for `curl … -o wb_agents.py` on a remote machine. Keep only if the Rust port still ships that script |

### 3.14 Unreachable / dead code paths

- `_handle_accounts` branch `/pricing/refresh` (20586) can never run
  (22083 matches first).
- `GET /proxy/slots` (18710) and `POST /proxy/slots` (20118) return the same
  payload; only the GET is used.
- `/accounts/credits/fetch` is an alias nobody calls (the dashboard uses
  `POST /accounts/credits`).
- `/console/state`, `/console/toggle` are legacy constants (#427).

---

## 4. Dashboard <-> handler cross-check

Method: grep of `dashboard.html` for `getJSON(` / `postJSON(` / `fetch(` /
`api(` with literal paths, plus `window.open` and template strings
(66 distinct paths). The dashboard sends `Authorization: Bearer <key>` (from
`localStorage`, only ASCII-printable) and `X-Panel-Token` on every call
(`authHeaders`, dashboard.html:7844-7850); GETs use `cache:'no-store'`, polled
usage reads use `cache:'no-cache'` to take part in ETag revalidation
(dashboard.html:7913-7949). 401/403 on any call -> login gate.

### 4.1 Dashboard calls with no matching handler behaviour

| Call | Finding |
|---|---|
| `GET /usage/timeseries?…&bucket_seconds=86400` (16583) | Handler reads `bucket`, ignores `bucket_seconds` (3.7) |
| `GET /settings` field `daily_chat_web_concurrency` (dashboard.html:11613) | **Not returned** by `runtime_settings_view` (wb_proxy.py:10104-10234); the field always renders as the fallback `1` even after a successful save (`/settings/save` does persist it, wb_proxy.py:19871-19877). Port should add it (plus a `_max` of 10) |

| `GET /settings` field `api_keys[].concurrency_limit` (dashboard.html:11807) | **Not returned** (wb_proxy.py:10111-10124). The dashboard maps the missing value to 0 and `saveApiKeys` posts `concurrency_limit: 0` for every row (dashboard.html:14109), so saving *any* key silently resets every key's concurrency limit to unlimited unless that row was re-edited. Port: include `concurrency_limit` in the view |

Every other dashboard path exists in the handler with the expected method:
`/accounts` (+ `?realm=all`), `/accounts/add-api-key`, `/accounts/checkin`,
`/accounts/credits` (POST), `/accounts/credits/detail` (POST),
`/accounts/credits/grants`, `/accounts/daily-chat`, `/accounts/daily-chat-web`,
`/accounts/delete`, `/accounts/export`, `/accounts/import`,
`/accounts/import/desktop`, `/accounts/login/start|poll|cancel`,
`/accounts/product`, `/accounts/refresh`, `/accounts/set`, `/accounts/set-all`,
`/accounts/sync-profile`, `/accounts/test`, `/activity/history`, `/agents`,
`/agents/apply`, `/agents/available`, `/agents/model-selection` (GET+POST),
`/agents/restore`, `/catalog/refresh`, `/dashboard?tab=logs`, `/health`,
`/logs`, `/logs/clear`, `/panel/login|logout|password|status`, `/pricing`,
`/pricing/mapping`, `/pricing/refresh`, `/proxy/discover`, `/proxy/slots`,
`/proxy/slots/save`, `/proxy/slots/test`, `/quota-scan/run`, `/realm`
(GET+POST), `/scheduler`, `/scheduler/toggle`, `/scheduler/trigger`,
`/settings`, `/settings/reveal`, `/settings/save`, `/shutdown`, `/tasks`,
`/tasks/claim|run|streak-bonus|travel`, `/updates`, `/updates/backups`,
`/updates/check|install|rollback`, `/usage`, `/usage/analytics`,
`/usage/by-account`, `/usage/perf`, `/usage/recent`, `/usage/remaining`,
`/usage/timeseries`, `/v1/models` (`?realm=intl|cn|all`). `/v1` alone is only
displayed as the base URL string (dashboard.html:14573, 14612).

`/settings/save` keys the dashboard sends (42 call sites): every key in 5.3
except `auth_disabled`, `api_key`, `restart_scheduler` and the legacy flat
limit keys / `pricing_refresh_hours`. Those remain accepted for older clients.

### 4.2 Handler routes the dashboard never calls

| Route | Who uses it |
|---|---|
| All OpenAI/Anthropic POSTs, `/models`, `/v1/balance-summary`, `/v1/usage` | API clients |
| `GET /accounts/credits` | none (dashboard uses POST); **[quirk]** GET with upstream side effects |
| `GET /accounts/credits/detail` | none (POST used) |
| `POST /accounts/credits/fetch` | alias, none |
| `GET /logs/export` | none — the dashboard builds its own export from `GET /logs?limit=5000` (dashboard.html:15138) |
| `GET /console/state`, `POST /console/toggle` | legacy, none |
| `GET /agents/cli` | `curl` from remote machines (README) |
| `POST /proxy/slots` | alias of GET, none |
| `/`, `/ui` | aliases of `/dashboard` (the dashboard itself only opens `/dashboard?tab=logs`) |

---

## 5. Configuration

### 5.1 CLI arguments (`_parse_cli_args`, wb_proxy.py:22669-22697)

| Arg | Default | Env fallback | Effect |
|---|---|---|---|
| `--info PATH` | – | – | Import a WorkBuddy desktop `*.info` credential at startup (`_report_first_run` 22848) **[DROP?]** (desktop) |
| `--host` | `127.0.0.1` | `HOST` | Bind address |
| `--port` | `8788` | `PORT` | Bind port |
| `--lan` | off | – | host -> `0.0.0.0` if still default; force/mint launcher key (2.8) |
| `--api-key` | – | `API_KEY`, then `WB_PROXY_KEY` | Launcher key (2.4) |
| `--system-prompt` | `"You are a helpful assistant."` (288) | – | `SYSTEM_PROMPT` injected when the request has no system message |
| `--user-agent` | official client UA | – | overrides `wb_accounts.USER_AGENT` |
| `--usage-dir` | `./usage` | `WB_PROXY_USAGE_DIR` (module default only) | Moves `usage.jsonl`, `limit-events.jsonl`, `estimate-daily.json`, `usage-recent-index.json`, activity history, `logs.sqlite3`, pricing data dir |
| `--accounts-dir` | `./accounts` | `ACCOUNTS_DIR` | Account files and `settings.json` |
| `--import-desktop` | off | – | Print/import desktop credentials then exit **[DROP?]** |
| `--panel-password` | – | – | Overwrite panel password at every start |
| `--probe-only` | off | – | Exit 0 if a gateway answers `/health` on the port, 3 if free, 1 if a foreign service holds it **[DROP?]** (Windows launcher helper) |

Startup order (`main` 22651, `_bootstrap_runtime` 22770): probe for a running
instance (GET `/health`) and exit if found; resolve dirs; init log DB; LAN
key; panel-key override; `--panel-password`; load pool, proxy slots, reserve
credits; `resize_chat_slots`; daily guards; persisted realm; start
Scheduler, catalog daily refresher, PriceRefresher, CreditsRefresher,
UpdateChecker; first-run desktop scan; bind. Windows console-close handler
(`install_console_close_handler` 243) **[DROP?]**.

### 5.2 Environment variables

Read at import time unless noted "per call". Booleans accept
`1/true/yes/on` (or the inverse list for default-on switches).

| Variable | Default | Consumer / meaning | Source |
|---|---|---|---|
| `HOST`, `PORT` | `127.0.0.1`, `8788` | CLI defaults | wb_proxy.py:22672-22673 |
| `API_KEY`, `WB_PROXY_KEY` | – | launcher key | 22677 |
| `ACCOUNTS_DIR` | `<script>/accounts` | accounts dir | 5999, wb_pricing.py:1230 |
| `WB_PROXY_USAGE_DIR` | `<script>/usage` | usage dir | 528, wb_activity.py:85, wb_pricing.py:1438 |
| `WB_PROXY_CATALOG_DIR` | `<script>/catalog` | catalog archive dir | 523 |
| `WB_PRICING_DIR` | usage dir | pricing policy/timeline/overrides dir | wb_pricing.py:1435 |
| `WB_PRICING_FILE` | `<script>/pricing/pricing.json` | bundled price snapshot | wb_pricing.py:785-789 |
| `WB_PROXY_DEFAULT_REALM` | `intl` | initial realm before `active_realm.json` | 191 |
| `WB_MAX_PAYLOAD_BYTES` | 16777216 | body cap | 40 |
| `WB_MAX_CONCURRENT_CHAT` | `32` (`auto`) | chat semaphore | 52 |
| `WB_CHAT_SLOT_WAIT` | 30 | seconds to wait for a chat slot | 59 |
| `WB_TRUSTED_PROXIES` | – | CIDRs whose `X-Real-IP`/`X-Forwarded-For` is trusted for login rate limit | 455 |
| `WB_ENABLE_ANTHROPIC_MESSAGES` | off | OR'd with setting | 17939 (per call) |
| `WB_ENABLE_FREE_MODEL_CREDIT_FALLBACK` | off | OR'd with setting | 14326 (per call) |
| `WB_BACKGROUND_MODEL_TARGET` / `WB_BACKGROUND_MODEL_NAMES` | `deepseek-v4.1-flash` / `gpt-5.6-terra,gpt-5.6-luna,gpt-5.3-codex` | background-model alias | 305-312 |
| `WB_DEFAULT_REASONING_EFFORT` | – (`off/none/0/false/no` = off) | chat path | 13404 |
| `WB_PROMPT_CACHE_KEY` | off | inject `prompt_cache_key` | 13440 |
| `WB_PRESERVE_REQUEST_CONTRACT` | off | keep client fields | 11808 |
| `WB_MODEL_SCOPED_ERRORS_PENALIZE` | off | 5xx charges account | 13901 |
| `WB_RESTRICTED_403_ROTATE_MAX`, `WB_RESTRICTION_WAVE_MIN`, `WB_RESTRICTION_WAVE_WINDOW`, `WB_RESTRICTION_HOLD_SECONDS` | 0, 2, 900, 1800 | account-restriction handling | 14091-14094 |
| `WB_AFFINITY_BY_PREFIX`, `WB_AFFINITY_DEBUG`, `WB_AFFINITY_MAX_MSGS`, `WB_AFFINITY_PAGE_SIZE` | on, off, 400, 3 | session affinity | 10271-10305 |
| `WB_STATS_TTL`, `WB_REMAINING_TTL`, `WB_COUNT_TTL`, `WB_TASKS_CACHE_TTL` | 15, 15, 30, 20 s | response caches | 1820, 2166, 4660, 6272 |
| `WB_USAGE_CACHE`, `WB_USAGE_CACHE_MIN_BYTES`, `WB_USAGE_CACHE_MIN_SECONDS` | on, 1 MiB, 900 | aggregate checkpoint files (per call) | 8749-8789 |
| `WB_RECENT_INDEX`, `WB_RECENT_INDEX_MIN_BYTES`, `WB_RECENT_INDEX_MIN_SECONDS` | off, 256 KiB, 900 | recent-page index (per call) | 5183-5204 |
| `WB_USAGE_DAY_BUCKETS`, `WB_USAGE_DAY_KEEP_DAYS`, `WB_USAGE_HOUR_KEEP_DAYS`, `WB_USAGE_HOUR_KEEP_HOURS` | on, 32, 2, 48 | aggregate retention; **env wins over `settings.usage`** (per call) | 7313-7385 |
| `WB_ROW_MEMO_MAX`, `WB_ROW_COST_MEMO_MAX` | built-in | memo caps | 4016-4017 |
| `WB_CREDITS_SWEEP_GAP_MS` | 800-1200 | pacing between accounts | 5938 |
| `WB_PROXY_DISCOVER_HOST`, `WB_PROXY_DISCOVER_PORTS` | `cli-proxy-mihomo`, `17901-17910` | `/proxy/discover` | 6311-6315 |
| `WB_FAIL_WINDOW` | 600 | breaker failure-decay window | wb_accounts.py:660 |
| `WB_WEB_TURN_TIMEOUT`, `WB_WEB_RETRY_BACKOFF`, `WB_WEB_RETRY_BACKOFF_MAX` | 120, 5, 30 | daily-chat web channel defaults (settings override) | wb_accounts.py:57-67 |
| `WB_KEEPALIVE_LEAD_HOURS` | 48 (1..720) | token keep-alive lead | wb_scheduler.py:44-60 |
| `WB_UPSTREAM_KEEPALIVE`, `WB_UPSTREAM_POOL_SIZE`, `WB_UPSTREAM_POOL_IDLE` | on, 2, 90 s (min 5) | upstream connection pool (per call) | wb_upstream_pool.py:61-87 |
| `WB_CA_BUNDLE`, `SSL_CERT_FILE` | – | custom CA | wb_upstream_pool.py:106-126 |
| `WB_CN_CHAT_UPSTREAM` | built-in | override cn chat base (http(s) only) | wb_identity.py:278 |
| `WB_IDENTITY_OS`, `WB_IDENTITY_ARCH`, `WB_IDENTITY_NODE_VERSION`, `WB_VSCODE_IDE_VERSION` | detected | identity headers | wb_identity.py:100-167 |
| `WB_WEB_TOOLS_PROXY`, `WB_WEB_ENGINES`, `WB_MAX_WEB_ROUNDS`, `WB_WEB_CACHE_TTL`, `WB_WEB_COOLDOWN`, `WB_WEB_COOLDOWN_ERROR`, `WB_WEB_MIN_INTERVAL`, `WB_WEB_BUDGET` | –, `duckduckgo,bing,wikipedia,stackoverflow,news`, –, 600, 600, 60, 0.5, 25 | local web tools | wb_webtools.py:102-106, 318, 633, 962 |
| `WB_UPDATE_REPO` | upstream repo | release check | wb_updates.py:40 |
| `WB_UPDATE_INSTALL_ENABLED`, `WB_UPDATE_EXECUTOR`, `WB_UPDATE_REQUEST_DIR`, `WB_APP_ROOT`, `WB_UPDATE_PROFILE` | –, –, `/run/workbuddy2api-hub-updater`, `/app`, – | updater **[DROP?]** | wb_update_request.py:19-71, wb_update_external.py:58-65 |
| `WB_ATREST_KEYID`, `WORKBUDDY_CONFIG_DIR`, `CODEBUDDY_CONFIG_DIR`, `LOCALAPPDATA` | – | desktop credential import **[DROP?]** | wb_atrest.py:334-722, wb_accounts.py:4696 |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `WB_PROXY_API_KEY` | – | wb_agents client configs **[DROP?]** | wb_agents.py:38, 696-720 |

### 5.3 `accounts/settings.json` schema

File: `<ACCOUNTS_DIR>/settings.json` (`settings_path`, wb_settings.py:233).
One JSON object, UTF-8, `indent=2`, `ensure_ascii=False`.

**I/O rules** (wb_settings.py:243-296):

- `load()` caches the parsed dict on `(path, st_mtime, st_size)` and returns
  a shallow copy. Missing or unparseable file -> `{}` (silently).
- `save()` writes `settings.json.tmp` then `os.replace`, and invalidates the
  cache. All setters do load -> mutate top-level key -> save under a
  process-wide `RLock`. There is no cross-process lock.
- Unknown keys survive every write (setters only touch their key; nested
  `upstream`/`usage` use `deep_merge`).
- Getters never raise: invalid stored values fall back to the default. The
  HTTP layer (`/settings/save`) is strict and answers 400; the setters in
  `wb_settings` clamp or raise `ValueError` as noted.
- Several getters migrate on read and rewrite the file (`limits` fold,
  `pricing_refresh_hours` -> minutes).

**Normalisation codes** used in the table:
`T` = only JSON `true` counts (anything else = false/default);
`N` = default-on: missing = true, otherwise only `true` is true;
`NF` = `is not False` (missing or any non-false = true).
"Save" = validation in `_handle_settings_save` (wb_proxy.py:19424-20114).
Booleans posted to `/settings/save` must be real JSON booleans (400
otherwise) unless noted.

#### Auth and keys

| Key | Type | Default | Read rule | Save validation / clamp | Consumer |
|---|---|---|---|---|---|
| `panel_password_hash` | hex str | absent (= default) | 2.6 | via `/panel/password` (≥4 chars) or `--panel-password` | `verify_panel_password` |
| `panel_password_salt` | hex str (32) | absent | | | |
| `panel_password_rounds` | int | 120000 | | | |
| `panel_password_default` | bool | absent=true | `is True` | | `/panel/status`, `/settings` |
| `api_keys` | list[key entry] | absent | 2.5; soft-deleted rows kept | `api_keys` list of objects; `deleted_api_key_ids` list = upsert mode, else replace mode; per row: blank `key` = keep stored, `len<4` -> 400, empty -> 400, `realm` ∈ `""/intl/cn`; omitted fields keep stored values | auth, usage attribution |
| `api_key` / `api_key_set` | str / bool | absent | legacy single key; deleted when `api_keys` is written | `api_key` (≥4 or `""`) sets both and replaces `API_KEY` at runtime | launcher key override |
| `launcher_key` | str `wb-…` | minted on first `--lan` | | never via HTTP | `--lan` |
| `auth_disabled` | bool | false | `T` | `bool(x)` (not strict) | `auth_required()` |

#### Limits (`limits`) — wb_settings.py:753-1023

`limits` = `{ <guard>: {"global": int≥0, "intl": int≥0|null, "cn": int≥0|null} }`
for guards `reserve_credits`, `daily_token_limit`, `daily_credit_limit`,
`model_daily_token_limit`, `expiring_window_days`. `null` = inherit global.
Defaults: all `global:0` (off) except `expiring_window_days: 7`.
Legacy flat top-level keys of the same names are folded into `limits` and
removed on the first read.

Save: `limits` object (400 if not a dict), per guard either a dict of scopes
or (legacy) a flat top-level number = global. Global must be a non-bool
non-negative integer (400); realm overrides accept `null`/`""` = inherit.
Effects: `reserve_credits` -> `POOL.apply_reserve_credits()`; the three daily
guards -> `apply_daily_guards(refresh=True)`; `expiring_window_days` ->
`POOL.apply_expiring_window()`. Consumers: wb_proxy `apply_daily_guards`
(2008), wb_accounts pool (`limit_values`, wb_accounts.py:3978), dispatch.

#### Scheduling and daily tasks

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `schedule.checkin` | list[int 0-23] | `[9,21]` | list or comma string; sorted unique; invalid/empty -> default per key | `schedule` object; each supplied key must yield ≥1 valid hour (400) | wb_scheduler |
| `schedule.travel` | list[int] | `[9,21]` | same | same | wb_scheduler |
| `schedule.keepalive` | list[int] | `[22]` | same | same | wb_scheduler |
| `schedule.cat` | list[int] | `[1]` | same | same | wb_scheduler |
| `schedule.daily_chat` | list["HH:MM"] | absent (= follow checkin hours) | list or comma/space string, full-width colon OK, ≤24 | `schedule.daily_chat`: empty clears; invalid non-empty -> 400 | wb_scheduler |
| `daily_chat_web` | bool | true | `N` | strict bool | wb_accounts daily chat |
| `daily_chat_model` | str | `""` (= `deepseek-v4.1-flash`) | trimmed | ≤64 chars, `^[A-Za-z0-9._:-]+$` or empty (400) | wb_accounts |
| `daily_chat_web_retry` | bool | true | `N` | strict bool | wb_accounts |
| `daily_chat_web_attempts` | int 1-5 | 3 (`WEB_TURN_ATTEMPTS`) | out of range -> default | int 1..5 else 400 | wb_accounts |
| `daily_chat_web_timeout` | int s 1-3600 | 120 (`WB_WEB_TURN_TIMEOUT`) | same | int 1..3600 else 400 | wb_accounts |
| `daily_chat_web_retry_backoff` | int s 0-60 | 5 (`WB_WEB_RETRY_BACKOFF`) | same | int 0..60 else 400 | wb_accounts |
| `daily_chat_web_fallback` | bool | false | `T` | strict bool | wb_accounts |
| `daily_chat_web_fallback_models` | list[str] | `[]` | list or comma string; ≤20, ≤128 bytes each, dedupe | list/tuple/str else 400 | wb_accounts |
| `daily_chat_web_random_prompt` | bool | false | `T` | strict bool | wb_accounts |
| `daily_chat_web_prompts` | list[str] | `[]` | list or newline string; ≤500, ≤200 bytes each | list/tuple/str else 400 | wb_accounts + `wb_webprompts` builtins |
| `daily_chat_web_concurrency` | int 1-10 | 1 | out of range -> default | int 1..10 else 400 | wb_scheduler (not echoed by `GET /settings`, 4.1) |

#### Dispatch and request path

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `account_max_concurrent` | int 0-10 | 0 (unlimited) | non-int/out of range -> 0 | strict int, 0..10 else 400 | pool dispatch |
| `affinity_stable_key_enabled` | bool | false | `T` | strict | `affinity_route` 10306 |
| `remaining_priority_enabled` | bool | false | `T` | strict | dispatch weights |
| `egress_backoff_enabled` | bool | false | `T` | strict; turning off clears pool weights | wb_egress accounting |
| `realm_fallback_enabled` | bool | false | `T` | strict | intl -> cn fallback |
| `auto_switch_product` | bool | false | `T` | strict | 429 identity rotation |
| `auto_switch_products` | list ⊆ `workbuddy,vscode,cli` | all three | unknown dropped | list of known ids else 400 | same |
| `free_model_credit_fallback` | bool | false | `T` (OR env) | strict | `deepseek-v4.1-flash` -> `-sg` fallback |
| `reasoning_effort_map` | bool | true | `NF` | strict | chat path |
| `enable_anthropic_messages` | bool | false | `T` (OR env) | strict | `/v1/messages` gate |
| `local_web_tools` | bool | false | `T` | strict | wb_webtools |
| `model_aliases` | object `{alias: model}` or list `[{from,to}]` | `{}` | lowercased alias; self-aliases dropped | **no HTTP writer** (hand-edit only) | `_apply_model_aliases` 337 |
| `upstream.header_timeout_seconds` | int ≥1 | 120 | invalid -> default | `upstream` object; int ≥1, unknown key -> 400 | upstream timeouts (wb_proxy.py:14240-14270) |
| `upstream.idle_timeout_seconds` | int ≥1 | 300 | same | same | same |
| `upstream.device_token` | str | `""` | trimmed | str | `X-Device-Token` (`resolve_device_token`, wb_accounts.py:584) |
| `upstream.device_token_file` | str path | `""` | | str | same |
| `prompt.mode` | `passthrough`/`custom`/`append` | `passthrough` | lowercase; invalid -> default | invalid -> 400 | `apply_prompt_mode` 13121, wb_prompt |
| `prompt.file` | str path | `""` | | str | wb_prompt |
| `prompt.text` | str ≤64 KiB | `""` | | >64 KiB -> 400 | wb_prompt |
| `identity.private_data` | `auto`/`true`/`false` | `auto` | invalid -> auto | `identity.private_data` else 400 | `X-Private-Data` header (wb_accounts) |
| `identity_versions` | object, keys `desktop_intl, desktop_cn, desktop_cli, vscode_product, vscode_ide, cli, sdk, node` | `{}` | each `^[0-9A-Za-z][0-9A-Za-z._-]{0,63}$`; invalid file -> `{}` | **requires `panel_password` in the same body** (401 otherwise); empty dict deletes the key | `wb_identity.configure_versions` (startup 6003 and on save) |

#### Catalog, models listing, quota probes

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `filter_models_by_key` | bool | false | `T` | strict | `/v1/models` |
| `models_only_available` | bool | false | `T` | strict | `/v1/models` |
| `window_probe` | bool | true | `N` | strict | wb_window_probe |
| `quota_scan_models` | list[str] | `[]` (off) | list or `,;\n` string, dedupe | list or null, strings only, ≤200 else 400 | wb_quota_scan |

#### Pricing and credits

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `pricing_enabled` | bool | true | `N` | strict; wakes refresher | wb_pricing master switch |
| `pricing_refresh_minutes` | float | 5.0 | ≤0 -> 0 (off); non-number -> 5 | number ≥0, clamp ≤43200; also accepts `pricing_refresh_hours` (×60) | `PriceRefresher.set_interval` |
| `pricing_refresh_hours` | float | legacy | migrated ×60 on first read, then deleted | | |
| `pricing_variant_inherit` | bool | true | `N` | strict; change triggers a refresh | wb_pricing |
| `credits_refresh_hours` | float | 12.0 | negative/non-number -> 12; clamp ≤720; 0 = off | number ≥0 | `CreditsRefresher` |

#### Logs, usage retention

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `log_db_enabled` | bool | false | `T` | strict | `apply_log_db_settings` 10625 -> `usage/logs.sqlite3` |
| `log_db_max_rows` | int or null | null (no cap) | ≤1e8 | `null`/`""`/0/garbage -> null | LogStore trim |
| `log_db_max_days` | int or null | **7** when absent; null when explicitly cleared | ≤36500 | same | LogStore trim |
| `log_db_max_bytes` | int or null | null | ≤100 GiB | same | LogStore trim |
| `usage.hour_keep_hours` | int 1-336 | 48 | invalid -> default | int 1..336 else 400 | usage aggregates (env `WB_USAGE_HOUR_KEEP_*` wins) |
| `usage.day_keep_days` | int 1-366 | 32 | same | int 1..366 else 400 | usage aggregates (env `WB_USAGE_DAY_KEEP_DAYS` wins) |

#### Panel UI preferences

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `ui_language` | `zh`/`zh-Hant`/`en` | `zh` | invalid -> zh | exact else 400 | dashboard HTML injection |
| `section_max_height` | int px 0-4000 | 640 (0 = no cap) | bool/neg/garbage -> 640; clamp | whole number ≥0 (fractions, bools, Infinity -> 400) | dashboard |
| `accounts_collapsed` | bool | false | `T` | strict | dashboard |
| `accounts_separate_tab` | bool | false | `T` | strict | dashboard |
| `key_before_hidden` | bool | false | `T` | strict | dashboard |
| `hidden_pages` | list[str] | `[]` | `^[a-z][a-z0-9_-]{0,31}$`, never `settings`, ≤32 | list of strings else 400; reply = normalised list | dashboard nav |

#### Updates **[DROP?]**

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `update_check_enabled` | bool | **false** | `T` | strict; true wakes checker | wb_updates daily GitHub check |
| `update_check` | `{last_attempt, last_success, latest_version(≤32), startup_checks[≤12]}` | zeros | sanitised | internal only | wb_updates |
| `keep_backups` | int 0-3 | 1 | clamp | strict int, clamped; prunes immediately (`pruned_backups` in reply) | updater |

#### Proxy slots, agents

| Key | Type | Default | Read rule | Save | Consumer |
|---|---|---|---|---|---|
| `proxy_slots` | list[slot] | `[]` | dedupe by url; missing ids shown as `slot-N` | via `/proxy/slots/save` / `/test` | pool egress (`apply_proxy_slots`) |
| `proxy_slot_seq` | int | 0 | monotonic id counter | internal | |
| `agent_models` | list[str] or absent | absent (= never chosen) | ≤200 strings; non-list -> treated absent | `/agents/model-selection` | wb_agents **[DROP?]** |

#### `/settings/save` only (not persisted as such)

| Body key | Effect |
|---|---|
| `restart_scheduler` (truthy) | `SCHEDULER.stop(); start()`; reply `scheduler:"restarted"` |
| `panel_password` | required companion of `identity_versions` |
| `deleted_api_key_ids` | switches `api_keys` to upsert mode |

`/settings/save` reply: per-key echoes (`api_keys_saved`, `auth_disabled`,
`limits`, each limit key's global value, `pricing_refresh_started`,
`pruned_backups`, `quota_scan_models_saved`, `api_key_set`, …) merged with the
full `runtime_settings_view()`.

### 5.4 `GET /settings` payload (`runtime_settings_view`, wb_proxy.py:10104-10234)

Every key in 5.3 resolved through its getter, plus:
`panel_password_is_default`, `api_key_set`, `api_key_set_by_panel`,
`api_key_masked` (`abcd******wxyz` or all `*` if ≤8 chars), `auth_required`,
`api_keys[{id,name,realm,models,accounts,enabled,masked,source,created_at}]`
(**[quirk]** `concurrency_limit` is not included, so the dashboard's key editor
always starts at 0/blank; saving a row that omits the field keeps the stored
value, but the dashboard sends `concurrency_limit: 0` and so **clears it on
every key save**, see 4.1), `deleted_api_keys[{id,name,realm,created_at,deleted_at}]`,
flat `reserve_credits`, `daily_token_limit`, `daily_credit_limit`,
`model_daily_token_limit` (global values), `identity_version_defaults`,
`daily_chat_web_*_default` / `_max` constants,
`daily_chat_web_builtin_prompts`, `daily_chat_model_default`,
`account_max_concurrent_max`, `account_inflight` (live per-account in-flight
counts), `keep_backups_max`, `quota_scan_interval_hours`, `quota_scans`
(≤12 recent rows), `accounts_dir`, `usage_dir`, `settings_file` (absolute
paths), `version: "1.7.32"` (hard-coded literal, separate from
`server_version`).

---

## 6. Data directories and files

Docker mounts `accounts/` and `usage/` as volumes (Dockerfile `VOLUME`,
docker-compose.yml). `catalog/` and `pricing/` live in the image.
`.gitignore` treats `accounts/*.json`, `usage/*.json[l]`, `usage/*.sqlite3*`,
`catalog/*.json` as runtime data.

### 6.1 `accounts/` (`ACCOUNTS_DIR`, `--accounts-dir`) — secrets, keep private

| File | Format | Purpose | Writer / reader |
|---|---|---|---|
| `<uid>.json` (uid sanitised to `[A-Za-z0-9_-]`) | JSON object, `indent=2` | One account: `uid, nickname, domain, realm, platform, product, enterpriseId, accessToken, refreshToken (plain text), expiresAt, addedAt, source, apiKey, proxySlot, proxy, enabled, tasksEnabled, weight, note, disabledModels, restrictedUntil, credits, tier, lastCheckin, lastDailyChat, lastActivityReport, modelCaps` (`to_dict` minus `lastError`, `cooldownUntil`). Atomic write via `<path>.<pid>.<tid>.tmp` | `Account.save` wb_accounts.py:1206; deleted by `/accounts/delete` |
| `settings.json` | JSON | Section 5.3 | wb_settings |
| `active_realm.json` | `{"realm","updated_at","updated_iso"}` | Persisted global realm switch | `save_persisted_realm` / `load_persisted_realm` wb_proxy.py:6006-6043 |
| `model_windows.json` | JSON | Per (account, model) quota-window reset times for the window probe | wb_window_probe.py:33 |
| `quota_scan.json` | JSON | Last scan time per (account, model) for the 6 h quota scanner | wb_quota_scan.py:36 |
| `output_probes.json` | JSON | Measured upstream max-output clamps per model (annotates `/v1/models`) | wb_probes.py:18 |
| `model.json` | JSON | Local cache of the models.dev index (context/output limits) | wb_modelsdev.py:28 |
| `integration-state.json`, `agent-backups/` | JSON / file copies | wb_agents state and backups of client configs **[DROP?]** | wb_agents.py:59-60 |
| `README.txt` | text | shipped note | – |

`NON_ACCOUNT_FILES = settings.json, active_realm.json, model_windows.json,
quota_scan.json` are skipped when loading accounts (wb_accounts.py:3676).
**[quirk]** `output_probes.json`, `model.json`, `integration-state.json` are
not on that list, so `AccountPool.load` (wb_accounts.py:3708) parses
them as `Account` objects at startup and drops them only because they carry
no `uid` (or logs them as unreadable). A port should load only files that
look like accounts, or move these state files to a separate directory.

### 6.2 `usage/` (`USAGE_DIR`, `--usage-dir`, `WB_PROXY_USAGE_DIR`)

| File | Format | Purpose |
|---|---|---|
| `usage.jsonl` | JSONL, append-only | One row per request (3.7). Source of every usage view |
| `limit-events.jsonl` | JSONL `{at, account, model, realm, reset, sample}` | Rate-limit cap events; feed `/usage/remaining` budgets (wb_proxy.py:2141, 2470-2510) |
| `estimate-daily.json` | JSON (schema 1) | Per-day budget estimate archive (2145) |
| `usage-recent-index.json` | JSON | Line-offset index for `/usage/recent` paging, only when `WB_RECENT_INDEX=1` (2149) |
| `usage-aggregate-cache-<kind>.json` (`snapshot`, `by_account`, `analytics`, `series`, `remaining`) | JSON | Restart accelerator: incremental aggregates + log offset/inode/tail signature; ignored if mismatched; safe to delete (`WB_USAGE_CACHE=0` disables). Legacy `usage-aggregate-cache.json` read as fallback only (wb_proxy.py:8696-8716, usage/README.txt) |
| `usage-summary.json` | JSON | Legacy, never read; safe to delete |
| `activity_history.jsonl` | JSONL `{ts, uid, nickname, realm, task, trigger, ok, message}` | Check-in / daily-chat attempts for `/activity/history` (wb_activity.py:22, 41) |
| `logs.sqlite3` (+ `-wal`, `-shm`, `-journal`) | SQLite table `logs(id INTEGER PK, ts, time, level, tag, msg)` + level index | Durable runtime log, only with `log_db_enabled` (wb_logstore.py:49-57) |
| `pricing-policies.jsonl` | JSONL | Price policy versions fetched from OpenRouter (wb_pricing.py:1465) |
| `pricing-timeline.jsonl` | JSONL | Which policy applied when (wb_pricing.py:1466) |
| `pricing-overrides.json` | JSON `{model: or_id}` | Panel-entered model -> OpenRouter id mappings (`/pricing/mapping`) |
| `pricing-markup.json` | JSON `{model: factor}` | Optional hand-edited price multipliers next to the overrides file (wb_pricing.py:1530-1538) |
| `README.txt` | text | shipped note |

`WB_PRICING_DIR` moves the four pricing files elsewhere.

### 6.3 `catalog/` (`WB_PROXY_CATALOG_DIR`)

| File | Format | Purpose |
|---|---|---|
| `product-config-<realm>.json` (`intl`, `cn`) | JSON | Copy of the last successful upstream `/v3/config` response. Written only, never read (catalog/README.txt, wb_proxy.py:11398-11407) |

The desktop app cache `~/.workbuddy-ai/cache/acc-product-config-v3.json`
(`PRODUCT_CONFIG_CACHE`, wb_proxy.py:362) is read as a catalogue source when
present **[DROP?]** on a VPS (no desktop app).

### 6.4 `pricing/`

| File | Format | Purpose |
|---|---|---|
| `pricing.json` | JSON `{"meta":{generated_at, base_currency:"CNY", usd_cny, sources, note}, "models":{…}}` | Bundled OpenRouter price snapshot (USD per 1M tokens, converted with `usd_cny`; optional `bands`). Overridable via `WB_PRICING_FILE` (wb_pricing.py:785-789) |

### 6.5 Outside the data dirs

| Path | Purpose |
|---|---|
| `dashboard.html` (next to the script) | Served by `/`; must be shipped with the binary (embed it) |
| `wb_agents.py` (next to the script) | Served by `/agents/cli` **[DROP?]** |
| `/run/workbuddy2api-hub-updater/{request,status,backups,executor}.json` | Updater handshake **[DROP?]** |
| `/etc/systemd/system/workbuddy2api-hub-updater.path` | Presence enables systemd install **[DROP?]** |

---

## 7. Port checklist (HTTP/settings)

1. Route table in 3.x with identical paths, aliases, methods, status codes
   and JSON envelopes; 404 `{"error":{"message":"not found",…}}` fallback; 501
   for unknown methods may become 405 (no client depends on it).
2. Auth order of 2.2, including "panel session satisfies key routes" and
   "panel-only routes reject API keys".
3. Key extraction (2.3) and constant-time matching; launcher-key retirement
   once a panel key exists.
4. PBKDF2-SHA256 120 000 rounds, 16-byte hex salt, hex digest; default
   `"admin"` stored as no hash. In-memory sessions, `X-Panel-Token`, 7-day
   absolute TTL.
5. Login rate limit 5 / 60 s per real client IP with the trusted-proxy rule.
6. CORS prefix rules (1.3); `OPTIONS` 204.
7. ETag/304 on the seven usage reads and the dashboard (1.2).
8. 16 MiB body cap, chunked bodies, non-object JSON -> `{}`.
9. `settings.json` read-tolerant / write-strict semantics, atomic writes,
   unknown keys preserved, the two read-time migrations.
10. Fix while porting (behaviour changes, decide explicitly): include
    `concurrency_limit` and `daily_chat_web_concurrency` in `GET /settings`;
    accept `bucket_seconds` on `/usage/timeseries`; make
    `/scheduler/toggle` persistent or document it as volatile; add an idle
    keep-alive timeout; restrict `/u/<ref>/` to API routes; exclude non-account
    JSON files from the account loader.
11. Drop candidates: `/accounts/import/desktop`, `--info`,
    `--import-desktop`, `--probe-only`, desktop catalogue cache, at-rest key
    recovery, `/agents*` (incl. `/agents/cli`), `/updates/install|rollback|backups`
    and the updater env vars, `/console/*`, Windows console handler.
