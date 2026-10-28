# 03 — `/v1/chat/completions` path: request normalization and output contract

Baseline: tag `py-baseline-1.7.32`. All citations are `file:line` in that tree.
Behaviour marked **[verified]** was confirmed by running the Python functions
against synthetic inputs (no Python file was modified). Items marked
**[quirk]** are faithful descriptions of current behaviour that a port may want
to keep for parity or fix deliberately; items marked **[ambiguity]** need a
decision.

---

## 0. Scope, routes and request lifecycle

### 0.1 Routes served by this handler

`do_POST` (wb_proxy.py:22031) accepts these paths for the OpenAI chat handler
(wb_proxy.py:22158-22163, dispatch at 22231-22347):

| Path | Handler |
|---|---|
| `/v1/chat/completions`, `/chat/completions` | `_dispatch_chat_post` -> chat path |
| `/v1/completions`, `/completions` | **same chat path, no translation** (see 0.3) |
| `/v1/responses`, `/responses`, `/v1/responses/compact`, `/responses/compact` | `_handle_responses` (out of scope, wb_proxy.py:22244-22246) |

Any path may be prefixed with `/u/<ref>/` to pin one account
(`_apply_pinned_route`, wb_proxy.py:18593-18631). A malformed prefix answers
404 `"pinned account route must be /u/<ref>/v1/..."`; an unknown ref 404, an
ambiguous ref 409, a pool not ready 503. All use the OpenAI error envelope
(section 2.3).

Unknown POST paths answer 404 `{"error":{"message":"not found","type":"invalid_request_error","code":404}}`
(wb_proxy.py:22163).

### 0.2 Ordered lifecycle of one chat request

This is the exact order of gates and side effects. Each gate that fails ends
the request with the shown status.

1. Pinned-route resolution (wb_proxy.py:22032-22033).
2. Path routing; unknown path -> 404 (wb_proxy.py:22158-22163).
3. Auth: `_authorized()` (wb_proxy.py:22168-22169, 18534-18549). Fail -> 401.
4. Body read and JSON parse: `_payload_or_error` (wb_proxy.py:22181, 19395-19412). Fail -> 413 or 400.
5. Model alias table rewrite (T1) and background-model rewrite (T2) (wb_proxy.py:22186-22191).
6. Per-API-key concurrency slot (wb_proxy.py:22193-22202). Fail -> 429.
7. Global chat slot semaphore, waits up to `WB_CHAT_SLOT_WAIT` seconds (default 30) (wb_proxy.py:22221-22223). Fail -> 503.
8. Background-request block, compiled-in `BLOCK_BACKGROUND_REQUESTS = False` (wb_proxy.py:12917, 22234-22243). If enabled and matched -> 400.
9. `build_upstream_body(payload)` (T3-T21) (wb_proxy.py:22249). Logged diagnostic line (22250-22262).
10. Session key and prompt fingerprint computed (wb_proxy.py:22263-22264).
11. Realm resolution and three local model gates (wb_proxy.py:22269-22279): cross-realm (400), global ban (400), per-key model restriction (400).
12. `open_upstream_with_fallback` (wb_proxy.py:22280-22284): account pick, per-attempt body tweaks (T22-T27), retries. Errors map to 403/503/502/429/upstream status (wb_proxy.py:22285-22333).
13. Stream (`_chat_stream_response`, wb_proxy.py:22349) or non-stream (`_chat_nonstream_response`, wb_proxy.py:22574) output.
14. Usage row appended to `usage.jsonl` (section 5).
15. `finally`: global chat slot released (22226-22227), then key slot released (22228-22229).

The upstream is **always** called with `stream: true` (T21), regardless of
what the client asked. `want_stream = bool(payload.get("stream"))`
(wb_proxy.py:22265) only selects the output mode.

**[quirk]** `bool(payload.get("stream"))` is Python truthiness: `"false"`
(a string), `1`, `[0]` all mean streaming. Only `false`, `null`, `0`, `""`,
absent mean non-stream.

### 0.3 `/v1/completions` is not a legacy-completions implementation

**[ambiguity]** `/v1/completions` and `/completions` are routed to the very
same `_dispatch_chat_post` (wb_proxy.py:22158-22161, 22244-22249). There is no
`prompt` -> `messages` conversion anywhere on this path (grep for
`payload.get("prompt")` only hits the settings handler at wb_proxy.py:20061).
A legacy body `{"model":..., "prompt":"..."}` is forwarded with
`messages = [{"role":"system","content":SYSTEM_PROMPT}]` plus the stray
`prompt` key, and the reply is a `chat.completion` / `chat.completion.chunk`
object, never `text_completion`. The Rust port must decide whether to keep this
(parity) or implement a real translation.

### 0.4 Local web tools (`wb_webtools.py`) are NOT on this path

`wb_webtools.py` is a gateway-side executor for `web_search` / `web_fetch`
(DuckDuckGo/Bing/Wikipedia/StackOverflow/Google News HTML/API scraping plus a
guarded URL fetcher; wb_webtools.py:1-56). It is wired only into the Responses
translation: `responses_to_chat` swaps the client's web tool declarations for
the gateway's functions and sets the private marker `_web_tools=True`
(wb_proxy.py:16494-16502) when the panel switch `local_web_tools` is on
(wb_settings.py:2518-2529, default **off**). The tool loop
(`follow_up_with_tool_results`, wb_proxy.py:16044) runs only in the Responses
handlers (wb_proxy.py:21741, 21797-21810).

On `/v1/chat/completions` nothing calls `install_tool_defs` or the executor,
and any client-sent `_`-prefixed key (including `_web_tools`) is stripped by
T10. Port conclusion: chat path ignores `wb_webtools` entirely.

---

## 1. Request transformations, in execution order

Notation: **Trigger** / **Behaviour** / **Toggle (default)** / **Avoids**
(upstream error code the step exists to prevent).

The body is built once per request (wb_proxy.py:22249) and reused for every
retry attempt (`prebuilt_body`, wb_proxy.py:14375-14376), except after a 403
content-review degrade (T25), which rebuilds it.

### T1. Operator model alias table

- Source: `_apply_model_aliases` (wb_proxy.py:337-354); table from
  `wb_settings.model_aliases` (wb_settings.py:1296-1329).
- Trigger: `payload.model` is a string and `model.strip().lower()` is a key
  of the alias table.
- Behaviour: replace `payload.model` with the target (target case preserved).
  Unknown names pass through. Runs before anything else reads `model`, so the
  logged, gated, billed and forwarded model are all the target.
- Table format in `settings.json` key `model_aliases`: either an object
  `{"alias": "target"}` or a list `[{"from"|"alias"|"name": ..., "to"|"target"|"model": ...}]`.
  Keys are lowercased and stripped; an entry whose alias equals the target
  (case-insensitive) or is empty is dropped (wb_settings.py:1299-1323).
  A settings read error yields `{}` (wb_proxy.py:22186-22189).
- Toggle: presence of entries. Default: empty table.
- Avoids: upstream "model not found" for client-only names (e.g. `claude-*`).

### T2. Background-model redirection

- Source: `_apply_background_model_alias` (wb_proxy.py:316-334), constants
  wb_proxy.py:305-313, detection `background_request_reason`
  (wb_proxy.py:13000-13019).
- Trigger, all three required:
  1. `model.strip().lower()` is in `WB_BACKGROUND_MODEL_NAMES`
     (env, comma list, lowercased; default `gpt-5.6-terra,gpt-5.6-luna,gpt-5.3-codex`);
  2. `client_metadata` is an object yielding background fields, and
  3. a background keyword matches (below) and the request is not an
     operator compaction.
- Background detection (`turn_metadata_fields`, wb_proxy.py:12954-12981):
  for each `client_metadata` value that is a string starting with `{`, parse
  it as JSON and copy any of `request_kind, turn_trigger, thread_source, kind,
  trigger, source`; also copy direct keys `request_kind, turn_trigger,
  thread_source`. Empty -> not background.
- `is_compaction_request` (wb_proxy.py:12984-12997): `request_kind` contains
  `compact` and `thread_source` (lowercased) is NOT one of
  `memory_consolidation, memory, ambient, suggestion, auto_review, autoreview, title`
  -> not background.
- Keyword match: join all field values with spaces, lowercase, substring
  search for any of `memory_consolidation, memory, memory-write, memory_write,
  memorywriting, ambient, suggestion, auto_review, auto-review, autoreview,
  title, compaction, compact, summariz` (wb_proxy.py:12920-12935).
- Behaviour: `payload.model = WB_BACKGROUND_MODEL_TARGET` (env, default
  `deepseek-v4.1-flash`).
- Toggle: env vars only. Always active.
- Avoids: credit spend from desktop-client background jobs (not an upstream error).

Example:

```json
// before
{"model":"gpt-5.6-terra","client_metadata":{"x-codex-turn-metadata":"{\"request_kind\":\"memory\",\"thread_source\":\"memory_consolidation\"}"},"messages":[...]}
// after
{"model":"deepseek-v4.1-flash", ...unchanged...}
```

The same reason string feeds the optional block at lifecycle step 8, whose
400 message is `background_request_message` (wb_proxy.py:13068-13072). The
block is compiled off.

### T3. Reasoning-effort mapping

- Source: `map_reasoning_effort` (wb_proxy.py:13332-13391), called first in
  `build_upstream_body` on a shallow copy (wb_proxy.py:13149).
- Trigger: setting `reasoning_effort_map` is not `false`
  (wb_settings.py:1748-1756, default **on**); body has key `reasoning_effort`
  (checked first) or `reasoningEffort`; value is a non-empty string; the
  model has a catalog `reasoning` block (`model_reasoning_meta`,
  wb_proxy.py:13281-13283, from `model_catalog_meta` wb_proxy.py:13254-13278).
- Behaviour (value only; the key spelling is kept):
  - Requested value in `off|none|disabled|disable` (case-insensitive):
    keep, unless `reasoning.canDisableThinking is False`, then replace with
    fallback.
  - Else if `supportedEfforts` non-empty: keep when it matches
    case-insensitively, else replace with fallback.
  - Else if a pinned `reasoning.effort` exists: keep when equal, else fallback.
  - Else: keep.
  - Fallback `_fallback_effort` (wb_proxy.py:13310-13329): `defaultEffort`;
    else (only when no `supportedEfforts`) `effort`; else first of
    `supportedEfforts`; else no change.
- Catalog lookup (wb_proxy.py:13254-13278): realm =
  `detect_model_realm(model)` or current realm; uses the in-memory
  `/v1/models` cache entries, falling back to `wb_catalog.STATIC_CN_MODELS` /
  `STATIC_INTL_MODELS`. Case-insensitive id match. Never triggers a network fetch.
- Avoids: upstream rejection of unsupported effort levels (code not named in source).

Example **[verified]** (catalog: deepseek-v4.1-flash `supportedEfforts=[low,high,max]`, `defaultEffort=high`):
`{"model":"deepseek-v4.1-flash","reasoning_effort":"medium"}` -> `"reasoning_effort":"high"`.

### T4. Effective thinking state (computed, not written)

- Source: wb_proxy.py:13153-13163.
- For models whose id lowercased starts with `deepseek`:
  `thinking_enabled = True` if `thinking.type == "enabled"`, or if
  `thinking.type != "disabled"` and the effort (either spelling) lowercased is
  not `none`. Otherwise false. Non-deepseek: always false.
- Consumed by T8.

### T5. Role normalization

- Source: `normalize_roles` (wb_proxy.py:11604-11620), applied to
  `payload.messages or []` (wb_proxy.py:13164).
- Behaviour: any dict message with `role == "developer"` is copied with
  `role = "system"`. Non-dict items pass through.
- Toggle: none.
- Avoids: **11128** (developer role rejected).

### T6. Gateway prompt mode

- Source: `apply_prompt_mode` (wb_proxy.py:13121-13142), config
  `prompt_mode_config` (wb_proxy.py:13110-13118), `wb_prompt.py` (whole file),
  settings `prompt` object (wb_settings.py:1627-1680).
- Settings: `prompt.mode` in `passthrough|custom|append` (default
  `passthrough`; unknown -> passthrough), `prompt.file` (path), `prompt.text`
  (inline, max 64 KiB, wb_settings.py:1634). Read error -> passthrough.
- Prompt text (`wb_prompt.load_prompt`, wb_prompt.py:47-62): inline text if
  non-blank, else file contents, else `DEFAULT_PROMPT` (wb_prompt.py:24-32).
  A file read error -> text `""` -> messages unchanged (fail open).
- Degrade window: `PROMPT_DEGRADE` (wb_proxy.py:13107), process-memory, set by
  T25, active until the next 00:00 Asia/Shanghai (fixed +08:00,
  wb_prompt.py:104-113, 116-136). Lost on restart.
- Behaviour (`wb_prompt.apply_mode`, wb_prompt.py:90-101):

| mode | not degraded | degraded |
|---|---|---|
| passthrough | unchanged | `rewrite(DEGRADED_PROMPT)` |
| append | insert `{"role":"system","content":text}` after the leading run of system/developer messages | `rewrite(DEGRADED_PROMPT)` |
| custom | `rewrite(text)`: drop **all** system/developer messages anywhere, prepend one system message | same as not degraded |

  `DEGRADED_PROMPT` = `"You are a helpful assistant. Respond in the user's language, follow the user's instructions, and be direct and concise."` (wb_prompt.py:35-36).
- Runs after T5, so "developer" messages are already "system" here.
- Avoids: 403 content review false positives from client system prompts.

### T7. Fingerprint sanitization

- Source: `sanitize_messages` (wb_proxy.py:11846-11868), `sanitize_content`
  (11790-11803), `sanitize_text` (11764-11789), `has_fingerprint`
  (11733-11763), `sanitize_tool_calls` (11812-11845), constants 11624-11732.
- Env toggle: `WB_PRESERVE_REQUEST_CONTRACT` in `1|true|yes|on`
  (default **off**, wb_proxy.py:11806-11809).
- Per message (dict only; each is shallow-copied):
  - `identity` rules apply only to roles `system`, `developer`, `assistant`.
  - `content` is sanitized unless preserve is on and role is `tool` or `function`.
    String content -> `sanitize_text`; list content -> each part with
    `type=="text"` and a `text` key gets `sanitize_text` on `text`; other parts untouched.
  - `reasoning_content` (string) -> `sanitize_text(identity=False)`.
  - `tool_calls` -> `sanitize_tool_calls`:
    - `function.arguments` string: sanitized with `identity=False` unless preserve is on;
    - `null` -> `"{}"`;
    - any other JSON type -> `json.dumps(value, ensure_ascii=False)` (default separators `", "`/`": "`), always, even with preserve on.
- `has_fingerprint(text, identity)` gate: returns true when any of
  1. literal (case-sensitive) substring of `SANITIZE_FEATURES`:
     `x-anthropic-billing-header`, `cc_entrypoint=`, `You are Claude Code`,
     `Main branch (`, `You are a coding agent running in the Codex CLI`,
     `github.com/anthropics/`, `11128` (wb_proxy.py:11624-11632);
  2. case-insensitive `x-anthropic-billing-header` or
     `sisyphus-junior - focused executor from ohmyopencode`
     (lowercase with fold of U+0130/U+0131 -> `i`, U+017F -> `s`, wb_proxy.py:11724, 11741-11747);
  3. identity only: `DeepSeek[ \t]+Harness` (ci) regex hit, or the DSH GUI sentence regex hit, or one of the identity verbs present and `SANITIZE_IDENTITY_RE` matches (wb_proxy.py:11748-11763).
- `sanitize_text` rewrite sequence (only when the gate fires), wb_proxy.py:11773-11789:
  1. `Sisyphus-Junior - Focused executor from OhMyOpenCode` (ci) -> `Sisyphus-Junior - Focused executor`.
  2. identity only, in order:
     a. `SANITIZE_IDENTITY_RE` (wb_proxy.py:11678-11684) -> `You are a helpful AI assistant.`
        Regex (ci): `You are\s+(?:an?\s+|the\s+)?(?:[\w.-]+\s+){0,3}?(?:agent|assistant|model|AI|LLM)?[\s,]*(?:powered by|built on|built by|created by|developed by|made by|trained by)[^\n]*?(?:\.(?=\s|$)|(?=\n|$))`
     b. DSH GUI sentence (ci): `(You are interacting with the user through the DeepSeek[ \t]+Harness[ \t]+Web[ \t]+GUI) whose URL the runtime context names\.` -> `\1 whose URL is named in the runtime context.`
     c. `DeepSeek[ \t]+Harness` (ci) -> `DeepSeek`.
  3. literal replacements `SANITIZE_REWRITES` (wb_proxy.py:11633-11643), all roles:
     - `You are Claude Code, Anthropic's official CLI for Claude` -> `You are Claude Code, Anthropic's official CLI tool for Claude`
     - `Main branch (you will usually use this for PRs)` -> `Default branch (you will usually use this for PRs)`
     - `You are a coding agent running in the Codex CLI, a terminal-based coding assistant.` -> `You are a coding agent running in the Codex CLI tool, a terminal-based coding assistant.`
     - `To give feedback, users should report the issue at https://github.com/anthropics/claude-code/issues` -> `To provide feedback, ...` (same tail)
     - `11128` -> `11-128`
  4. remove `(?i)x-anthropic-billing-header:[^;\r\n]*;?\s*`.
  5. if `cc_` present: repeatedly remove `(?i)\bcc_[a-z0-9_]+=[^;\r\n]*;?\s*` until stable.
  6. remaining bare `(?i)x-anthropic-billing-header` -> `x-anthropic-billing-hdr`.
  7. **`.strip()` the whole text.**
- **[quirk]** Rules 3-6 and the strip also hit `user` and `tool` content (only
  identity rules 2a-2c are role-gated). Example: user text `"error 11128 please"`
  becomes `"error 11-128 please"` **[verified]**. Leading/trailing whitespace of
  any text that triggered the gate is lost.
- Avoids: **11128** ("Illegal API invocation from an unapproved channel") / WAF 400 on client fingerprints.

Example **[verified]**:

```json
// before (system after T5)
{"role":"system","content":"You are an AI agent powered by DeepSeek Harness. x-anthropic-billing-header: cc_version=1; cc_entrypoint=cli;"}
// after
{"role":"system","content":"You are a helpful AI assistant."}
```

### T8. `reasoning_content` backfill (DeepSeek multi-turn)

- Source: `backfill_reasoning_content` (wb_proxy.py:12143-12181), call at 13169-13171.
- Trigger: model lowercased starts with `deepseek`, and either
  `thinking_enabled` (T4) or `has_trace` = any dict message has a non-empty
  string `reasoning` or has the key `reasoning_content` (any value).
- Behaviour, for every dict with `role=="assistant"` (copied):
  - if `reasoning_content` is not a string: set it to `reasoning` when that is
    a string, else `""`;
  - if `reasoning` is not a non-empty string: set `reasoning` to
    `reasoning_content` if non-empty else `" "` (single space).
- Toggle: none (gated on model prefix and thinking state).
- Avoids: **11155** ("the reasoning content from the previous turn must be passed back in thinking mode") and the upstream `len(reasoning) > 0` check.

Example **[verified]**: `{"role":"assistant","content":null,"tool_calls":[...]}` ->
`{"role":"assistant","content":null,"tool_calls":[...],"reasoning_content":"","reasoning":" "}`.

### T9. Mandatory leading system message

- Source: wb_proxy.py:13172-13173.
- Trigger: messages empty, or `messages[0].role != "system"`.
- Behaviour: prepend `{"role":"system","content":SYSTEM_PROMPT}`.
  `SYSTEM_PROMPT` defaults to `"You are a helpful assistant."`
  (wb_proxy.py:288, 6045) and is set from CLI `--system-prompt`
  (wb_proxy.py:22679-22680, 22775).
- Avoids: upstream requirement of a system message (CLI help text: "required upstream").
- **[ambiguity]** `messages[0].get(...)` assumes a dict. A non-dict first
  element (or `messages` given as a string, which T5 iterates character by
  character) raises inside `build_upstream_body`, which runs outside any
  try block (wb_proxy.py:22249). The exception escapes the handler; the client
  gets no JSON error (connection closed). Rust should return 400.

### T10. Body assembly and private-key strip

- Source: wb_proxy.py:13174-13183.
- Behaviour: `body = shallow copy of the (T3-mapped) payload`; every top-level
  key whose string form starts with `_` is removed; `body.model` = model (post
  T1/T2); `body.messages` = result of T5-T9. All other client keys pass
  through untouched (e.g. `temperature`, `top_p`, `n`, `user`, `metadata`,
  `response_format`, `parallel_tool_calls`, `client_metadata`, `prompt`).
  `client_metadata` is not stripped on the default identity; see T23 for the CLI identity drop list.
- **[quirk]** model missing in the request: `body.model = ""` (wb_proxy.py:13146,
  13182), while the usage/error rows use `"hy4-preview"` as the label
  (wb_proxy.py:22266). The upstream receives an empty model.

### T11. Tool-call pairing repair (three passes, fixed order)

Source: wb_proxy.py:13184-13192. Order matters: merge, then repack, then cleanup.
Avoids: **11148** (`tool_call_sequence_broken`) in all three cases. No toggle.

**T11a. `merge_adjacent_tool_calls`** (wb_proxy.py:11902-11956). Walk the list;
for an assistant message `m` whose output predecessor `prev` is also an assistant:
- Form 1: `m.tool_calls` non-empty list, `m.content` empty (`None`, `""` or
  `[]`, `_empty_content` wb_proxy.py:11874-11887), `prev.tool_calls` non-empty list
  -> `prev.tool_calls += m.tool_calls`, fold reasoning, drop `m`.
- Form 2: `m` has no `tool_calls` key, `m.content` non-empty string,
  `prev.tool_calls` non-empty, `prev.content` empty -> `prev.content = m.content`,
  fold reasoning, drop `m`.
- Reasoning fold (`_merge_reasoning_content`, wb_proxy.py:11890-11899):
  if `m.reasoning_content` is a non-empty string, `prev.reasoning_content =
  prev + "\n" + m` (or just `m` when prev empty).
- **[quirk]** `prev` is mutated in place: it is the copy made by T7/T8, so the
  client payload is unaffected, but T8's mirrored `reasoning` field on `prev`
  is not updated by the fold.

**T11b. `repack_tool_result_blocks`** (wb_proxy.py:11959-12025). Needs >= 3
messages. For each assistant with non-empty `tool_calls`: collect its call ids;
scan forward. `role=="tool"` with a `tool_call_id` in the id set -> result;
a tool message with an unknown id, a non-dict, or any assistant ends the batch;
any other message (system/user/...) is an "intruder". Output: the assistant,
then all results (original order), then the intruders. Changed only if there
was at least one result and at least one intruder.

**T11c. `cleanup_orphan_tool_calls`** (wb_proxy.py:12028-12088).
`keep = (ids of assistant tool_calls) ∩ (tool_call_id of tool messages)`
(only non-empty string ids count). Each assistant's `tool_calls` filtered to
`keep` (key removed when nothing remains, in place). Every `role=="tool"`
message whose id is not in `keep` is dropped. A tool_call without a string
`id` is dropped too.

Example **[verified]** (after T11a+T11b+T11c):

```json
// before
[{"role":"assistant","content":null,"tool_calls":[{"id":"a",...}]},
 {"role":"assistant","content":"","tool_calls":[{"id":"b",...}]},
 {"role":"developer","content":"image notice"},
 {"role":"tool","tool_call_id":"a","content":"ok"},
 {"role":"tool","tool_call_id":"zzz","content":"orphan"}]
// after
[{"role":"assistant","content":null,"tool_calls":[{"id":"a",...}]},
 {"role":"tool","tool_call_id":"a","content":"ok"},
 {"role":"system","content":"image notice"}]
```

### T12. `max_completion_tokens` -> `max_tokens`

- Source: `translate_max_completion_tokens` (wb_proxy.py:12836-12847), call 13193.
- Behaviour: always **pop** `max_completion_tokens`. If `max_tokens` already
  exists, nothing else. Else `int(value)`; if `> 0` set `max_tokens`. Invalid
  or non-positive values are silently dropped.
- Avoids: upstream does not know `max_completion_tokens`.

### T13. GPT minimum `max_tokens`

- Source: wb_proxy.py:13194-13203.
- Trigger: `max_tokens` present and model lowercased starts with `gpt-`.
- Behaviour: `int(max_tokens) < 16` -> set to `16` (logged). Non-integer
  values are left as is.
- Avoids: 400 `integer_below_min_value` (#364, #252).

Example **[verified]**: `{"model":"gpt-5.5","max_tokens":1}` -> `"max_tokens":16`.

### T14. `tool_choice` normalization

- Source: `normalize_tool_choice` (wb_proxy.py:12185-12225), call 13204.
- Behaviour (only when the key exists):
  - string: if `strip().lower() == "none"` -> `"none"`; any other string kept
    verbatim (e.g. `"auto"`, `"required"`, `" Auto "` unchanged).
  - object, `type` (lowercased, stripped):
    - `none` -> `"none"`;
    - `auto` / `required` -> that string;
    - `function` -> `function.name` or top-level `name`, stripped; empty -> `"auto"`.
      **The upstream receives the bare function name as a string**, e.g. `"ls"`;
    - anything else -> key removed.
  - any other type (number, list, null, bool) -> key removed.
- `tools` are never removed because of `tool_choice:"none"` (wb_proxy.py:12192-12208).
- Avoids: **11101** (object `tool_choice` cannot unmarshal into the upstream's string field).

### T15. Tools normalization

- Source: `normalize_tools` (wb_proxy.py:12358-12397), call 13205.
- Legacy `functions`: if `functions` exists and `tools` does not, pop
  `functions` and set `tools = [{"type":"function","function":f}]` for each
  dict `f`. (If both exist, `functions` passes through untouched.)
- If `tools` is missing, empty, or not a list -> key removed.
- For each dict entry (non-dicts dropped):
  - flat form (`name` present and `function` absent) -> `{"type":"function","function":{"name": name or "", "description": description or "", "parameters": parameters or {}, ["strict"]}}`, then parameters normalization;
  - otherwise copy, force `type = "function"`, and if `function` is a dict normalize its parameters.
- Empty result -> key removed.
- Parameters normalization `_normalize_tool_parameters` (wb_proxy.py:12325-12355):
  `parameters` null/non-dict -> `{"type":"object","properties":{}}`; else copy,
  force `type="object"`, ensure `properties` is a dict, then `_deep_normalize_schema`.
- `_deep_normalize_schema` (wb_proxy.py:12241-12322), recursive, mutating:
  1. remove keys `description, title, format, pattern, items, default` whose value is `null`;
  2. string `type`: lowercase/strip; aliases `str|text -> string`, `int -> integer`,
     `bool -> boolean`, `dict -> object`, `list -> array`, `float -> number`;
     other non-standard type -> `object` if `properties` present, `array` if
     `items` present, else remove `type`. List `type`: map each truthy entry through the
     alias table (lowercased).
  3. `type=="object"` or (`properties` present and no type) -> type `object`, ensure dict `properties`;
  4. `required`: list -> keep `str(x)` for non-null, non-blank entries, remove key if empty;
     non-blank string -> `[s.strip()]`; anything else -> removed;
  5. `items`: list -> first dict element normalized (or `{"type":"string"}` when none);
     dict -> normalized; null -> removed;
  6. recurse into `properties` values, `$defs` / `definitions` values, and `anyOf`/`oneOf`/`allOf` dict members.
  Not handled: `additionalProperties`, `patternProperties`, `not`, `if/then/else`, `prefixItems`.
- Toggle: none (not affected by `WB_PRESERVE_REQUEST_CONTRACT`).
- Avoids: **11129** (schema validator) and **11101**.

Example **[verified]**:

```json
// before
{"tools":[{"name":"ls","description":null,"parameters":{"type":"dict","properties":{"p":{"type":"str","description":null}},"required":[null,"p"]}}],
 "tool_choice":{"type":"function","function":{"name":"ls"}}}
// after
{"tools":[{"type":"function","function":{"name":"ls","description":"","parameters":{"type":"object","properties":{"p":{"type":"string"}},"required":["p"]}}}],
 "tool_choice":"ls"}
```

### T16. Stop normalization

- Source: `normalize_stop` (wb_proxy.py:12401-12424), call 13206.
- Behaviour: string -> `[s]` (empty string -> remove); list/tuple -> keep
  non-empty strings; any other type -> remove. Empty result -> key removed.
- Avoids: **11101** (`cannot unmarshal string into Go struct field Request.stop of type []string`).

Example: `"stop":"END"` -> `"stop":["END"]`; `"stop":["", 3]` -> key removed.

### T17. Default `max_tokens` from catalog

- Source: wb_proxy.py:13207-13210, `model_default_max_output_tokens` (13286-13296).
- Trigger: `max_tokens` absent after T12/T13.
- Behaviour: set to catalog `maxOutputTokens` when it is a positive int.
  Examples **[verified]** from the bundled tables: deepseek-v4.1-flash 128000,
  gpt-5.5 128000, glm-5.3 48000, hy3 64000.
- Toggle: none.
- Ordering: T13's clamp runs before T17, so a catalog value is never clamped (all are >= 16 anyway).

### T18. DeepSeek thinking injection

- Source: wb_proxy.py:13211-13235, `default_reasoning_effort_mode` (13394-13409).
- Trigger: model lowercased starts with `deepseek`, AND NOT opted out
  (`thinking.type == "disabled"`), AND effort (either spelling, post-T3)
  lowercased is not `none`.
- Behaviour:
  - `thinking` absent -> `thinking = {"type":"enabled"}` (an existing `thinking` of any shape is kept).
  - effort absent/empty (both spellings) and mode != `off` -> `chosen = catalog defaultEffort`;
    if none and mode is `high` -> `"high"`; if chosen -> set `reasoning_effort`.
- Env `WB_DEFAULT_REASONING_EFFORT`: `off|none|0|false|no` -> `off`;
  `catalog|model|default` -> `catalog`; anything else (including unset) -> `high` (**default**).
- Rationale (source comment): `thinking.enabled` alone yields zero reasoning tokens upstream.
- **[quirk]** `"reasoning_effort":"off"` or `"disabled"` is not an opt-out here
  (only `none` is), so `thinking:{"type":"enabled"}` is still injected.

### T19. `stream: true` forced

wb_proxy.py:13236. Always.

### T20. `stream_options` default

- wb_proxy.py:13237-13238. Key absent -> `{"include_usage": true}`.
  A client-provided `stream_options` (even `{"include_usage": false}`) is kept verbatim.
- **[quirk]** if a client disables `include_usage` and the upstream then sends
  no usage, the client stream gets no usage frame (section 3.6); the usage row
  falls back to an estimate (section 5).

### T21. Full build example **[verified]**

Input (default settings, bundled deepseek catalog):

```json
{"model":"deepseek-v4.1-flash","stream":false,"stop":"END","max_completion_tokens":512,
 "_private":1,
 "messages":[{"role":"user","content":"hi"}]}
```

Output body (before per-attempt tweaks):

```json
{"model":"deepseek-v4.1-flash","stream":true,"stop":["END"],
 "messages":[{"role":"system","content":"You are a helpful assistant."},{"role":"user","content":"hi"}],
 "max_tokens":512,"thinking":{"type":"enabled"},"reasoning_effort":"high",
 "stream_options":{"include_usage":true}}
```

Wire key order follows Python dict insertion order: client keys in original
order (`model`/`messages`/`stream` overwritten in place, appended if absent),
then `max_tokens`, `thinking`, `reasoning_effort`, `stream`, `stream_options`.
Order is not semantically required, but byte parity with the official client
was a stated goal of T24.

---

### Per-attempt transformations (inside `open_upstream`, per account tried)

### T22. `prompt_cache_key` injection (opt-in)

- Source: `inject_prompt_cache_key` (wb_proxy.py:13444-13473); gate env
  `WB_PROMPT_CACHE_KEY` in `1|true|yes|on` (default **off**, 13429-13441); used at 14574-14577.
- Behaviour: an existing non-blank `prompt_cache_key` is kept. Else
  `conv` = body `conversation_id` / `conversationId` (first non-blank) else the
  session key; key = `"wb2a-" + (uid[:8] or "-") + "-" + sha256(uid + "|" + conv).hex[:32]`.

### T23. CLI-identity body tweaks

- Source: wb_proxy.py:14582-14587; constants wb_identity.py:143-146.
- Trigger: `account.is_cli_identity()`.
- Behaviour: drop `metadata`, `conversation_id`, `previous_response_id`; set `cache_retention = "long"`.

### T24. Wire encoding

- `encode_upstream_body` (wb_proxy.py:13242-13251): compact JSON
  (`separators=(",",":")`, `ensure_ascii=False`, UTF-8).
- CLI or VSCode identity: gzip and send `Content-Encoding: gzip` (wb_proxy.py:14591-14593, 14611-14612).
- POST `account.chat_base_url() + "/v2/chat/completions"` (wb_proxy.py:286, 14570).
  Header construction belongs to the identity spec.
- Timeouts (`upstream_timeouts`, wb_proxy.py:14243-14266): header timeout
  `upstream.header_timeout_seconds` (default 120 s) for connect/TLS/first byte;
  then the socket switches to `upstream.idle_timeout_seconds` (default 300 s)
  per read (wb_proxy.py:14269-14295; defaults wb_settings.py:1522-1527). No total cap.

### T25. Content-review degrade retry (403)

- Source: wb_proxy.py:14744-14755.
- Trigger: upstream 403 that is not the account-level restriction (11140),
  first such 403 in the request, prompt mode `passthrough` or `append`.
- Behaviour: trigger the degrade window (T6), rebuild the body from the
  original payload (`build_upstream_body(payload)`, so T6 now rewrites the
  system prompt to `DEGRADED_PROMPT`), retry the same account.
- `fp` and the handler's diagnostic log still describe the pre-degrade body.

### T26. Free-model credit fallback (opt-in)

- Source: wb_proxy.py:14905-14928; map wb_proxy.py:14318-14320; gate settings
  `free_model_credit_fallback` (wb_settings.py:1711-1713) or env
  `WB_ENABLE_FREE_MODEL_CREDIT_FALLBACK` (default **off**).
- Trigger: `(realm, model) == ("intl","deepseek-v4.1-flash")` and every eligible
  account has an active 6004 cap for that model.
- Behaviour: rerun `open_upstream` with `model = "deepseek-v4.1-flash-sg"`
  (body rebuilt). The response gets `_wb_billed_model`, which makes the usage
  row record the paid model with `requested_model` = the client's name.

### T27. Realm fallback intl -> cn (opt-in)

- Source: `open_upstream_with_fallback` (wb_proxy.py:15036-15080),
  `realm_fallback_target` (15011-15033); gate settings `realm_fallback_enabled`
  (wb_settings.py:1981, default **off**).
- Trigger: realm not explicit (no key-bound realm, no `X-Realm`, no `?realm=`;
  wb_proxy.py:22269-22282), first attempt raised `RateLimited` or
  `RuntimeError("no usable account...")`, realm is `intl`, model not exclusive
  to one realm, cn pool has a ready account.
- Behaviour: one retry with `target_realm="cn"`, same prebuilt body.

---

## 2. Validation, limits and error responses

### 2.1 Body reading

`_read_payload` (wb_proxy.py:19356-19394):

- Cap `MAX_PAYLOAD_BYTES = int(env WB_MAX_PAYLOAD_BYTES or 16777216)` (wb_proxy.py:40).
- `Content-Length > cap` -> **413** without reading; `_discard_body` sets
  `close_connection` for oversized declared bodies (wb_proxy.py:18220-18225).
- `Transfer-Encoding` containing `chunked` (case-insensitive) is decoded
  manually (wb_proxy.py:19314-19355); running total > cap -> drain the rest, then 413.
  Invalid hex chunk size or premature EOF -> 400.
- Negative or non-integer Content-Length -> 400.
- Empty body (`Content-Length: 0` or absent) parses as `{}` (no error).
- Body must decode as UTF-8 (strict for Content-Length; chunked uses
  `"replace"`) and parse as JSON, else 400.
- **[quirk]** valid JSON that is not an object (array, string, number) silently
  becomes `{}` (wb_proxy.py:19388-19394). `{}` then flows on: model `""`,
  messages = `[system]`, sent upstream.
- `Expect: 100-continue`: the stdlib `BaseHTTPRequestHandler.parse_request`
  (HTTP/1.1, wb_proxy.py:17993) already sends `100 Continue` for every HTTP/1.1
  request carrying it, before any handler code runs. `_error` paths send a
  second `100 Continue` (wb_proxy.py:18286-18307, 18308-18314); multiple 1xx
  responses are legal HTTP. Rust: answer `100 Continue` once before reading
  the body.

| Case | HTTP | `error.message` | `error.type` |
|---|---|---|---|
| too large | 413 | `payload too large (<n> bytes > <cap> limit)` | `invalid_request_error` |
| bad JSON | 400 | `invalid JSON body` | `invalid_request_error` |

(wb_proxy.py:19399-19412). The 413 also carries
`gateway_hint: "single request body exceeds the gateway cap (WB_MAX_PAYLOAD_BYTES, default 16MB); raise it and restart wb-proxy"`.

Request line limit: 1 MiB (`max_request_line`, wb_proxy.py:18011). Longer ->
414 `request line too long (limit 1048576 bytes); put long content in the POST body, not the URL`
(wb_proxy.py:18036-18053), then close.

### 2.2 Error envelope (OpenAI shape)

Non-stream errors on this path go through `_error` (wb_proxy.py:18308-18319)
or `_rate_limited` (18325-18362), both written via `_json` (18090-18117):

```json
{"error": {"message": "<text>", "type": "<type>", "code": <http status int>, "gateway_hint": "<optional>"}}
```

- `code` is the **HTTP status as an integer**. Default `type` is `server_error`.
- `gateway_hint` only when `gateway_hint(status, message)` (wb_proxy.py:13520-13582)
  is non-empty. First matching rule wins, in this order: 413 + "payload too large";
  503 + "concurrent chat limit"; 11133 / model_param_invalid / "invalid request
  parameters"; 11135 / invalid_image_data / "replace the image"; 11140 /
  账号存在异常行为 / "account has been restricted" / "unusual activity";
  "no usable account" / "no healthy account"; 本机网络不可用 / "local network
  unavailable"; context-length phrases; status 429 or "rate limit" / "frequency
  limit"; status 402 or insufficient+credit; session + (not found | expired);
  content + (reject | policy); no such model / model not found / unsupported
  model; 403 + waf. Hint strings must be copied verbatim from those lines.
  Matching is on the lowercased message, except the Chinese literals.
- Headers: `Content-Type: application/json; charset=utf-8`, `Content-Length`,
  `Access-Control-Allow-Origin: *` when `cors_origin_allowed(path)`
  (wb_proxy.py:375-385: path starts with `/v1`, `/chat`, `/completions`,
  `/models`, `/responses` and not with a management prefix such as `/v1/usage`).
  JSON uses `ensure_ascii=False` and Python default separators (`", "`, `": "`).
- `_error` answers `Expect: 100-continue` and drains any unread body first
  so keep-alive stays aligned (wb_proxy.py:18183-18235).

`_rate_limited` (429) differs:

```json
{"error": {"message": "<text>", "type": "rate_limit_error", "code": 429, "retry_after": <int seconds>}}
```

plus header `Retry-After: <seconds>`, and **no `gateway_hint`**.
`wait = max(1, int(exc.wait or 60))`. Text = the exception's local message
when it has one, else `"upstream rate limit reached for this model; retry in <wait>s"`
followed by `" - " + detail[:200]` when an upstream detail exists.
`wait` for upstream 429s is `retry_after_seconds(model, realm)`: the shortest
positive account throttle wait in the realm, else 60 (wb_proxy.py:13770-13777, 14948-14952).

### 2.3 Error table for the chat path

| Origin | HTTP | message | type | cite |
|---|---|---|---|---|
| bad pinned prefix | 404 | `pinned account route must be /u/<ref>/v1/...` | invalid_request_error | 18612-18615 |
| pinned ref unknown / ambiguous | 404 / 409 | `pinned account '<ref>' <why>` | invalid_request_error | 18623-18628 |
| pool not ready for pin | 503 | `pinned account '<ref>' is not usable: account pool is not ready` | invalid_request_error | 18619-18622 |
| unknown path | 404 | `not found` | invalid_request_error | 22163 |
| bad or missing key | 401 | `invalid api key - send it as 'Authorization: Bearer <key>' or '?key=<key>'; copy the value from the panel's 设置 page` (or the `no key is configured ...` variant) | invalid_request_error | 18534-18549 |
| body too large | 413 | see 2.1 | invalid_request_error | 19399-19406 |
| bad JSON | 400 | `invalid JSON body` | invalid_request_error | 19407-19411 |
| per-key concurrency | 429 | `Rate limit reached: API key '<name>' reached its concurrency limit (<n> in flight); retry shortly` | rate_limit_error (via `_error`: no Retry-After, no retry_after) | 22193-22202 |
| global chat slots | 503 | `gateway is at its concurrent chat limit (<N> in flight); retry shortly` | server_error | 22221-22223 |
| background block (compiled off) | 400 | `background_request_message(reason)` | invalid_request_error | 22234-22243 |
| cross-realm model | 400 | Chinese text (18478-18480) | invalid_request_error | 22271-22273 |
| banned model (tables empty by default) | 400 | `banned_model_message` (13075-13079) | invalid_request_error | 22274-22276 |
| key model restriction | 400 | `key_model_message` (13093-13100) | invalid_request_error | 22277-22279 |
| pinned account unusable | 403/409/503 | `exc.message` | invalid_request_error | 22285-22289, 14351-14363 |
| content rejected | 403 | `upstream 403: <detail or "content rejected">` | invalid_request_error | 22290-22296 |
| local network down | 503 | `本机网络不可用：<detail> - 请检查本机网络/代理后重试（本次不计入账号熔断）` | invalid_request_error | 22297-22304 |
| TLS verify failure | 502 | hint text | invalid_request_error | 22305-22309 |
| rate limited | 429 | see `_rate_limited` | rate_limit_error | 22310-22315 |
| upstream HTTP error passed through (400, model-scoped 5xx, unhandled codes) | upstream status | `upstream <code>: <first 600 bytes of body>` | server_error | 22316-22322 |
| pool empty | 503 | `no usable account for realm '<r>': ... - add or enable one at the dashboard (/)` | server_error | 22328-22332, 14991-14993 |
| other exception | 502 | `upstream unreachable: <exc>` | server_error | 22333 |
| non-stream aggregation failure | 502 | `upstream stream error: <exc>` | server_error | 22580-22587 |

Rows from "per-key concurrency" down append an error row to `usage.jsonl`
(section 5.2), except the global-slot 503, the background block and the three
local model gates, which return without `record_error`.

Auth (`_key_ok`, wb_proxy.py:18441-18452): a valid panel session token
(`X-Panel-Token`) also authorises; else the key from `Authorization`
(Bearer, case-insensitive scheme, optional quotes), `x-api-key`, `api-key`,
`x-auth-token`, or query `key|api_key|api-key` (18400-18440); with no key
configured and auth not required, everything passes (wb_proxy.py:546-552).

### 2.4 Concurrency limits

- Global: `BoundedSemaphore(MAX_CONCURRENT_CHAT)`; env `WB_MAX_CONCURRENT_CHAT`
  (default 32; `auto|pool|dynamic` = one per ready account, floor 32)
  (wb_proxy.py:48-59, 388-418). Waits `WB_CHAT_SLOT_WAIT` s (default 30), then 503.
- Per key: `key_entry.concurrency_limit` (int, 0 = unlimited), counted per key
  id (wb_proxy.py:22606-22640; an identical earlier definition at 17945-17979 is
  shadowed).
- Per account (default off): settings `account_max_concurrent`
  (wb_settings.py:148-160); `open_upstream` skips busy accounts
  (wb_proxy.py:14489-14509). The account slot stays bound to the upstream
  response until it is closed (wb_proxy.py:14632, 13650-13689).

---

## 3. Streaming output contract (`stream` truthy)

Source: `_chat_stream_response` (wb_proxy.py:22349-22572).

### 3.1 Response head

Sent as soon as the upstream returns 2xx headers, before any body byte is read
(wb_proxy.py:22351-22358):

```
HTTP/1.1 200 OK
Server: wb-proxy/1.7.32 Python/<ver>
Date: <...>
Content-Type: text/event-stream; charset=utf-8
Cache-Control: no-cache
Connection: close
Access-Control-Allow-Origin: *          (only when cors_origin_allowed)
```

No `Content-Length` and no chunked transfer encoding: the body ends at
connection close. After the head is sent, every failure is reported in-band
(3.7). Errors before the head (section 2.3) are normal JSON responses.

### 3.2 Framing

- Each event is exactly `data: <json>\n\n` (wb_proxy.py:22398). No `event:` or
  `id:` lines. JSON is `json.dumps(..., ensure_ascii=False)`, default separators.
- Heartbeat: `SSEHeartbeat` (wb_proxy.py:15354-15409), a daemon thread with
  interval `SSE_HEARTBEAT_SECONDS = 15.0` (15351) and tick `min(0.1, interval/4)`.
  After >= 15 s without any write it emits `: keepalive\n\n`. Frames and
  heartbeats share one lock, so a comment never splits a frame. A failing
  heartbeat write closes the heartbeat silently. It runs only inside the
  `with heartbeat:` block (wb_proxy.py:22415-22485); the error frame in 3.7 is
  written after it has stopped.
- Batching: frames from one upstream read block are concatenated and written
  once, flushed before each potentially blocking upstream read
  (wb_proxy.py:22375-22412; reader `iter_sse_lines`, 15299-15349, 64 KiB
  `read1` blocks). Pure syscall optimisation; Rust may write per frame.
- Success terminator: `data: [DONE]\n\n` (wb_proxy.py:22485).

### 3.3 Per-upstream-line processing (exact algorithm)

For each upstream line (wb_proxy.py:22416-22471):

1. `data = strip_data_prefix(line)` (wb_proxy.py:11505-11515): strip
   whitespace; empty or starting with `:` -> skip; strip **repeated** `data:`
   prefixes (each followed by strip); skip again if empty or starting with `:`.
   Upstream `event:` / `id:` lines are therefore forwarded as raw text
   (see quirk below). Skip `[DONE]` (the gateway emits its own).
2. Try `json.loads(data)`. If the result is a dict (`maybe`):
   a. **Usage hoist**: if `maybe.usage` truthy, keep it as `last_usage` when
      `last_usage` is unset or its `total_tokens` >= the previous one
      (max by total_tokens, ties take the later), then **remove `usage` from
      this frame**.
   b. For each choice: append `delta.reasoning_content` (if truthy) and
      `delta.content` (if truthy) to `streamed_text` (used only for the
      fallback usage estimate). If `delta.content` is truthy: **remove it from
      the delta** and feed it to the leak filter (3.5); emit what the filter
      releases, *before* this frame.
   c. If `choice.finish_reason` truthy: remember it in `pending_finish`
      (last one wins) and set it to `null` in this frame.
   d. **Id rewrite**: if `maybe.id` truthy, set it to `stream_chunk_id`.
3. `cleaned = clean_chunk(...)` (wb_proxy.py:11516-11557) on the re-serialised
   dict (or the raw text when not a dict):
   - not JSON -> returned unchanged (forwarded verbatim as `data: <text>`);
   - usage aliases normalised (no-op here, usage already removed);
   - `finish_reason == ""` -> `null`;
   - `delta.function_call` removed when it is falsy or a dict without a truthy `name`;
   - `delta.tool_calls == []` removed;
   - `delta.extra_fields`, `delta.refusal`, `delta.reasoning_content` removed when present but falsy;
   - **if any choice's delta becomes `{}` and its `finish_reason` is falsy, the
     whole frame is dropped** (returns `""`);
   - re-serialised only if something changed, otherwise the input string is kept.
4. Non-empty `cleaned` -> `write_chunk(cleaned)` (sets `emitted`, records
   `first_ms` = ms since request arrival at the first frame).

Consequences:

- Upstream frames with an empty `choices` list (e.g. the upstream's own usage
  frame) survive with usage removed: the client sees
  `{"id":"chatcmpl-wb-…","object":"chat.completion.chunk",...,"choices":[]}`
  **[verified]**. Clients must tolerate a choices-less, usage-less chunk.
- Upstream fields are otherwise passed through unchanged: `object`, `created`
  (upstream value), `model` (upstream's model string), `system_fingerprint`,
  `delta.role`, `delta.reasoning_content`, `delta.tool_calls` (structured
  deltas are forwarded as-is, including their `index`, partial `arguments`
  and upstream ids), `logprobs`.
- A delta of `{"role":"assistant","content":""}` keeps `content:""`
  (empty content is falsy, so it is neither removed nor fed to the filter).
- **[quirk]** `clean_chunk` returns `""` for the whole frame as soon as *one*
  choice has an empty delta and no finish; with `n>1` other choices in that
  frame are lost. Upstream n>1 behaviour is unknown.
- **[quirk]** A non-JSON upstream line that is not an SSE comment
  (e.g. `event: message`) is forwarded as `data: event: message\n\n`, and it
  counts as `emitted`.

### 3.4 Chunk id stability

- One id per response: `stream_chunk_id = "chatcmpl-wb-" + uuid4().hex[:16]`
  (wb_proxy.py:22361). Every upstream frame with a truthy `id` is rewritten to it;
  every gateway-synthesised frame (text, tool_calls, finish, usage) uses it.
- A transparent retry (3.7) generates a **new** id; safe because nothing had
  been emitted yet.
- Upstream frames without an `id` are not given one.

### 3.5 Printed (leaked) tool-call filtering

Purpose: the model sometimes prints a tool call as text in `content` instead
of a structured `tool_calls` delta (wb_proxy.py:12456-12477).

Filter: `LeakedToolCallStreamFilter` (wb_proxy.py:12689-12758), one per
response, fed every non-empty `delta.content`. Output actions:
`("text", s)` -> `chat_text_chunk` = `{"id","object":"chat.completion.chunk","created":now,"model":<requested model>,"choices":[{"index":0,"delta":{"content":s},"finish_reason":null}]}`;
`("calls", [..])` -> `chat_tool_call_chunks`: **one frame per call**, delta
`{"tool_calls":[{"index":i,"id":"call_<32hex>","type":"function","function":{"name":...,"arguments":<JSON string>}}]}`
with `i` = position within that batch (wb_proxy.py:12761-12792).

`feed(text)` (wb_proxy.py:12709-12743):

1. `buf += text`. Find `idx` = first `<` from which `could_be_leaked_call`
   holds (wb_proxy.py:12647-12655, 12681-12686). `could_be_leaked_call(tail)`:
   normalise full-width `｜…DSML…｜` to `|DSML|`, lowercase; true if
   `LEAK_TAG_RE` matches at the start and the matched tag starts with one of
   `LEAK_TAG_PREFIXES` (wb_proxy.py:12505-12513); else false if `len > 96` or
   it contains `>`; else true if the tail is a prefix of a known tag prefix or
   vice versa.
2. No candidate -> release all buffered text.
3. Text before `idx` released; buffer keeps `buf[idx:]`.
4. Buffer > `MAX_HOLD = 262144` chars -> release everything as text.
5. `_split_at_call_close` (wb_proxy.py:12658-12678): find the first closing
   tag (`LEAK_CLOSE_RE`, ASCII or full-width DSML bars) whose name is in
   `CALL_CLOSE_TAGS = {calls, tool_calls, function_calls, function_call,
   invoke, run_code, run_shell}`, extended across immediately following
   (whitespace-separated) closing tags. None -> keep holding.
6. Parse that prefix with `parse_leaked_tool_calls` (wb_proxy.py:12536-12615),
   else `parse_dsml_tool_calls` (wb_proxy.py:12432-12454). No calls -> keep holding.
7. Emit `("calls", calls)`, then `("text", clean)` for leftover prose inside the
   prefix (note: **calls before text** in `feed`), then re-scan the remainder.

`flush()` at end of stream (wb_proxy.py:12745-12758): parse the whole
buffer; no calls -> `("text", buf)`; else `("text", clean)` (if non-empty)
**then** `("calls", ...)` (text before calls in `flush`).

`parse_leaked_tool_calls`: fold full-width DSML bars; find every
`<(|DSML|)?(parameter|param|arg|argument) ...name="X"...>value</...>`
(case-insensitive, DOTALL); the owning call is the nearest preceding opening
tag that is not a wrapper/param tag; its name is its `name="..."` attribute or
the tag name itself (`<run_code>` -> `run_code`); consecutive params with the
same owner merge. Arguments: `json.dumps({param: value.strip()}, ensure_ascii=False)`
(default separators; all values are strings). The markup region plus
trailing closing tags is removed; leftover DSML-bearing or wrapper tags are
stripped; result `.strip()`ed.

Verified behaviour **[verified]**:

| Upstream content deltas | Client sees |
|---|---|
| `"Sure. <run_"`, `"code><parameter name=\"code\">print(1)</parameter>"`, `"</run_code> done"` | text `Sure. `; tool_call `run_code` `{"code": "print(1)"}`; text ` done` |
| `x <invoke name="a"><parameter name="p">1</parameter></invoke> y` (one delta) | text `x `; tool_call `a` `{"p": "1"}`; text ` y` |
| `<think>abc</think> answer`, ` more` | nothing until end, then one text `<think>abc</think> answer more` |
| `use <parameter> tags`, ` ok` | text `use `, then at end text `<parameter> tags ok` |
| `a < b and c > d` | released immediately |
| ASCII DSML `<|DSML|calls><|DSML|invoke name="run_code"><|DSML|parameter name="code">…` with **no space** after `|DSML|` | **released as plain text, no tool call** |
| `Hi <|DSML| invoke name="ls">…</|DSML| invoke> bye` (space after bars) | **released as plain text, no tool call** |

**[bug, verified]** The stream filter never holds back a DSML-prefixed tag
once its `>` has arrived. `could_be_leaked_call` lowercases the tail first
(wb_proxy.py:12649), but `LEAK_TAG_RE` (wb_proxy.py:12485-12488) is compiled
without `re.IGNORECASE` and spells the token as uppercase `DSML`. So
`LEAK_TAG_RE.match("<|dsml|calls>...")` returns `None`, and the fallback
returns false because the tail contains `>`. Only a bare partial like `<|DSML|`
(no `>` yet) is held. Measured result: pure DSML calls (ASCII or full-width
bars) stream through as prose on the chat stream, while the **non-stream**
path (`repair_leaked_tool_calls` in `aggregate_stream`) converts the same text
into a tool call. Tests only cover the hybrid `<run_code>` form
(tests/_test_dsml_chat_fallback.py:107-150). Rust should decide whether to
fix the stream path (recommended) or keep parity.

Additional rules:

- The filter is fed even when the same turn also has structured
  `tool_calls` deltas (no "real call wins" check on the stream path, unlike
  `repair_leaked_tool_calls` at wb_proxy.py:12627-12629). Synthesised call
  indices start at 0 per batch and can collide with upstream structured
  call indices.
- Released text frames are written before the (content-stripped) upstream
  frame they came from; that upstream frame is then usually empty and dropped
  by `clean_chunk`, unless it carries `reasoning_content`, `role`, etc.
- Synthesised frames use the **requested** model name and `created = now`;
  passed-through frames keep the upstream's `model` and `created`.

### 3.6 Stream tail: finish frame, usage frame, `[DONE]`

After the upstream iterator ends (wb_proxy.py:22472-22485), in order:

1. `release(leak_filter.flush())`.
2. If any call was synthesised **or** `pending_finish` is set: one finish frame
   `{"id":…,"object":"chat.completion.chunk","created":now,"model":<requested>,"choices":[{"index":0,"delta":{},"finish_reason":R}]}`
   with `R = "tool_calls"` if any call was synthesised, else `pending_finish`
   (the upstream's last finish_reason verbatim: `stop`, `length`,
   `tool_calls`, `content_filter`, `error`, ...).
   No finish frame at all when the upstream never sent a finish_reason and
   nothing was synthesised.
3. If `last_usage` set: usage frame `chat_usage_chunk` (wb_proxy.py:12795-12812):
   `{"id":…,"object":"chat.completion.chunk","created":now,"model":<requested>,"choices":[],"usage":U}`
   with `U` = the upstream usage object after `normalize_usage_cache_aliases`
   (wb_proxy.py:619-643): when the best cached value (`_best_cached_tokens`,
   wb_proxy.py:806-833: first positive of `prompt_tokens_details.cached_tokens`,
   `prompt_cache_hit_tokens`, `cache_read_input_tokens`, `cached_tokens`,
   `input_tokens_details.cached_tokens`, `completion_tokens_details.cached_tokens`)
   is > 0, it is written to `cache_read_input_tokens`, `cached_tokens`,
   `prompt_cache_hit_tokens`, `prompt_tokens_details.cached_tokens` and (if that
   object exists) `input_tokens_details.cached_tokens`. Other upstream usage
   fields (e.g. `credit`) pass through.
   The usage frame is emitted regardless of the client's `stream_options`
   (it is controlled only by whether the upstream sent usage).
4. If nothing at all was emitted during the whole stream:
   `data: {"error": {"message": "empty upstream stream", "type": "server_error"}}\n\n`
   (note: compact-ish default separators, no `code`; wb_proxy.py:22480-22484).
5. `data: [DONE]\n\n` (always on this path, even after the empty-stream error).

Example tail **[verified]** (upstream sent a finish `stop` and usage, model printed a call):

```
data: {"id": "chatcmpl-wb-8b0c…", ..., "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_51e9…", "type": "function", "function": {"name": "run_code", "arguments": "{\"code\": \"x\"}"}}]}, "finish_reason": null}]}

data: {"id": "chatcmpl-wb-8b0c…", ..., "choices": []}

data: {"id": "chatcmpl-wb-8b0c…", ..., "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}

data: {"id": "chatcmpl-wb-8b0c…", ..., "choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8, "prompt_tokens_details": {"cached_tokens": 2}, "cache_read_input_tokens": 2, "cached_tokens": 2, "prompt_cache_hit_tokens": 2}}

data: [DONE]

```

finish_reason normalisation summary on the stream path: upstream `""` ->
`null` in passthrough frames; every upstream finish is withheld and replayed
once at the end; `tool_calls` forced when a printed call was converted.
Unlike the non-stream path there is **no** `tool_calls` -> `stop` downgrade
and no truncated-arguments drop on the stream path.

### 3.7 Mid-stream failures

`except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError)`
(wb_proxy.py:22486-22494): client disconnected. Nothing more is written; a
usage row with `outcome="client_aborted"` is recorded with whatever
`last_usage` was seen (may be null -> `usage_missing`).

Any other exception while reading upstream (timeout, `IncompleteRead`,
reset) (wb_proxy.py:22495-22544):

1. Egress backoff accounting (`wb_egress.note_transient`) if the setting is on.
2. **Transparent retry** if nothing was emitted yet and this is the first
   attempt: close the upstream, `open_upstream(..., pin_account=account, prebuilt_body=forwarded)`
   (same account, same body; wb_proxy.py:22335-22341), and recurse with
   `headers_sent=True, retried=True, reopen=None` (one retry only). A failed
   reopen falls through to step 3. Heartbeat comments already sent do not
   count as "emitted".
3. Otherwise: error row (`status=502`, `outcome="upstream_aborted"`, message
   `stream aborted: <exc>`, partial usage), then flush the filter, write any
   buffered frames, then the terminal error frame (`chat_stream_error_frame`,
   wb_proxy.py:12815-12825):

```
data: {"error": {"message": "upstream stream aborted: <exc>", "type": "upstream_error", "param": null, "code": "upstream_stream_error"}}

```

   **No finish frame, no usage frame and no `[DONE]`** follow; the
   connection closes (`Connection: close`).

Note the asymmetry: in-band `error.code` here is the string
`"upstream_stream_error"`, while JSON error responses use the integer HTTP status.

Truncated upstream detection: `iter_sse_lines` raises `IncompleteRead` when
the upstream closes early on a Content-Length response (wb_proxy.py:15332-15340),
which feeds this path.

### 3.8 reasoning_content passthrough

`delta.reasoning_content` is forwarded unchanged inside the upstream frame
(only removed when empty, 3.3). It is never fed to the leak filter, never
renamed (no `reasoning` alias added), and counted into the fallback token
estimate.

---

## 4. Non-stream output (`stream` falsy)

Source: `_chat_nonstream_response` (wb_proxy.py:22574-22598) calling
`aggregate_stream(upstream, model, None)` (wb_proxy.py:15412-15558). The
upstream is still read as SSE. There is no heartbeat and no transparent
retry on this path; any read exception -> 502 `upstream stream error: <exc>`.

### 4.1 Aggregation

Per line: `strip_data_prefix`; `[DONE]` sets `saw_done`; unparsable JSON is
skipped. Per parsed chunk:

- `first_chunk_at` = time of the first parsed chunk.
- `resp_id` = last truthy `chunk.id` (the **upstream id is kept**; unlike the
  stream path it is not rewritten).
- `model` = last truthy `chunk.model` (the **upstream's model string**).
- usage = max by `total_tokens` (ties take later), same rule as the stream.
- per choice (all choices fold into **one** message; `index` ignored):
  - `delta.content` (truthy) appended; `delta.reasoning_content` appended.
  - `delta.tool_calls[]`: keyed by `index` (missing -> current map size).
    New key -> `{"id": id or "call_<32hex>", "type": type or "function", "function": {"name","arguments"}}`.
    Existing key -> id replaced if a new one is given; `name` and `arguments`
    **concatenated**.
  - legacy `delta.function_call` with a truthy `name` -> slot 0 (create, or
    set name if empty and append arguments).
  - truthy `finish_reason` -> remembered (last wins; initial value `"stop"`).

### 4.2 Post-processing, in order (wb_proxy.py:15496-15532)

1. `message = {"role":"assistant","content": "".join(content)}` (always a
   string, `""` when there was none); add `reasoning_content` only when
   non-empty.
2. No structured calls -> `repair_leaked_tool_calls(message)`
   (wb_proxy.py:12618-12644): parse printed calls (leak parser, then DSML
   parser); on success `content` = cleaned text and `tool_calls` =
   `[{"id","type":"function","function":{"name","arguments"}}]`, and
   `finish = "tool_calls"`.
3. Drop structured calls with an empty `function.name`.
4. If `finish == "length"` or `[DONE]` was never seen: drop calls whose
   `arguments` is a non-empty string that is not valid JSON
   (`is_truncated_arguments`, wb_proxy.py:12091-12109).
5. Remaining structured calls -> `message.tool_calls` ordered by index; if
   `finish` is `stop` or null -> `"tool_calls"`.
   Else if a printed call was recovered -> `"tool_calls"` (overrides `length`).
   Else if `finish == "tool_calls"` (all calls filtered away) -> `"stop"`.
6. Usage fallback when usage is missing or `total_tokens == 0` and there is any text:
   `comp = estimate_tokens(content) + estimate_tokens(reasoning)`,
   `prompt = max(1, comp // 2)`;
   `{"prompt_tokens": prompt, "completion_tokens": comp, "total_tokens": prompt+comp,
   "completion_tokens_details": {"reasoning_tokens": estimate_tokens(reasoning)},
   "prompt_tokens_details": {"cached_tokens": 0}}`.
   `estimate_tokens(t)` (wb_proxy.py:15285-15292) = CJK chars (U+4E00-9FFF,
   U+3400-4DBF) + `max(1, int(other_chars / 3.6))`, 0 for empty.
   **[quirk]** the prompt estimate is derived from the completion, not the prompt.

### 4.3 Response object

```json
{
  "id": "<upstream id, else chatcmpl-wb-<16hex>>",
  "object": "chat.completion",
  "created": <now, int>,
  "model": "<upstream model string, else requested model>",
  "choices": [{"index": 0,
               "message": {"role": "assistant", "content": "<text>",
                           "reasoning_content": "<only if non-empty>",
                           "tool_calls": [ ...only if any... ]},
               "finish_reason": "<stop|length|tool_calls|...>"}],
  "usage": { ...upstream usage after normalize_usage_cache_aliases, or the estimate; omitted if neither... },
  "elapsed_ms": <int, aggregation time only>,
  "first_chunk_at": <float epoch seconds or null>
}
```

- **[quirk]** `elapsed_ms` and `first_chunk_at` are internal fields that leak
  into the client response (wb_proxy.py:15556-15557, returned verbatim at
  22598). Strict clients ignore them; Rust may drop them (decide).
- `message.content` is `""`, never `null`, even for tool-call-only answers.
- No `logprobs`, no `system_fingerprint`, no `refusal`.
- Sent with `_json(200, result)`: `Content-Type: application/json; charset=utf-8`,
  `Content-Length`, CORS header; keep-alive allowed (no `Connection: close`).

Example **[verified]**:

```json
{"id": "up-9", "object": "chat.completion", "created": 1792740017, "model": "deepseek-v4.1-flash",
 "choices": [{"index": 0, "message": {"role": "assistant", "content": "", "reasoning_content": "hmm",
   "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "ls", "arguments": "{\"p\":1}"}}]},
   "finish_reason": "tool_calls"}],
 "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14},
 "elapsed_ms": 0, "first_chunk_at": 1792740017.88}
```

### 4.4 Stream vs non-stream differences (port checklist)

| Aspect | stream | non-stream |
|---|---|---|
| response id | gateway `chatcmpl-wb-<16hex>` | upstream id |
| model field | upstream per passthrough frame; requested on synthesised frames | last upstream model |
| printed-call recovery | `LeakedToolCallStreamFilter` (DSML-opening bug) | `repair_leaked_tool_calls` (DSML works) |
| printed calls when structured calls exist | still converted | skipped |
| empty-name / truncated call drop | no | yes |
| `tool_calls` -> `stop` downgrade | no | yes |
| retry on read error before first byte | once, same account | no |
| empty upstream | error frame + `[DONE]` | 200 with `content:""`, `finish_reason:"stop"`, no usage |

---

## 5. Usage and accounting captured per request

All rows go to `USAGE_LOG = <WB_PROXY_USAGE_DIR or ./usage>/usage.jsonl`
(wb_proxy.py:526-530, `--usage-dir` override 22710-22711), one JSON object
per line, appended (`_persist_usage`, wb_proxy.py:929-943). Exactly one row
per request that reached the upstream-open stage or failed in the
classified ways of 2.3.

### 5.1 Success / abort row: `record_usage` (wb_proxy.py:836-920)

Called through `record_upstream_usage` (wb_proxy.py:923-926), which sets
`model` to the billed model (`upstream._wb_billed_model`, T26) or the requested one.

| field | value |
|---|---|
| `at` | epoch float |
| `iso` | local `%Y-%m-%dT%H:%M:%S` |
| `model` | billed model (post T1/T2 alias; `hy4-preview` if the request had none) |
| `requested_model` | only when different from `model` (credit fallback) |
| `stream` | bool |
| `outcome` | `completed` / `client_aborted` |
| `elapsed_ms` | wall ms from request arrival (`t_start`, set after body parse and `build_upstream_body`, wb_proxy.py:22267) |
| `ttft_ms` | ms to first emitted frame (stream) / first parsed upstream chunk (non-stream); may be null |
| `gen_ms` | `elapsed_ms - ttft_ms` or null |
| `usage_missing` | `true` only when no usage at all |
| `prompt_tokens`, `completion_tokens`, `reasoning_tokens` (from `completion_tokens_details`), `cached_tokens` (`_best_cached_tokens`), `total_tokens`, `credit` | from `_extract_usage` (wb_proxy.py:651-663); absent when usage missing |
| `msgs_sha`, `n_msgs`, `system_sha`, `prefix_sha` | `prompt_fingerprint(forwarded.messages)` (wb_proxy.py:10359-10375): sha256 of `json.dumps(obj, ensure_ascii=False, sort_keys=True)` truncated to 12 hex; `system_sha` = hash of message 0 if system else `""`; `prefix_sha` = hash of all but the last message, `""` when one message |
| `account` | account uid |
| `realm` | account realm, else current realm |
| `key` | API key settings id or `""` (never the secret) |
| `cost_policy` | `wb_pricing.current_policy_id(model)` |
| `reasoning_effort` | only when known: `upstream_effort_of(upstream_body, model)` (wb_proxy.py:13035-13065, returned at 14637) - catalog-pinned effort, else `none` when thinking disabled or effort `none`, else client effort, else catalog `defaultEffort` |
| `tokens_per_sec` | `round(completion_tokens / (gen_ms/1000), 2)` when `gen_ms > 0` |
| `cache_hit_pct` | `round(cached*100/prompt, 1)` when `prompt_tokens > 0` |

Side effect: `acc.consume_credit(credit)` when the usage has a `credit`
(wb_proxy.py:886-887). A `chat done:` log line is written (917).

Stream-path usage selection (wb_proxy.py:22545-22571): `last_usage` (max by
total_tokens). If missing or `total_tokens == 0` and any content/reasoning
text streamed: estimate `comp = estimate_tokens(all text joined)`,
`prompt = max(1, comp//2)`, `reasoning_tokens: 0`, `cached_tokens: 0`. This
estimate is **only recorded**, never sent to the client. If the upstream's
held finish was `error`, the row is written by `record_error` instead (status
502, message `upstream stream finished with error`, `outcome="failed"`,
model = billed model), though the client already received a normal tail with
`finish_reason:"error"` and `[DONE]`.

Non-stream path records `result.usage` (upstream or the 4.2 estimate) with
`ttft_ms` from `first_chunk_at - t_start`.

### 5.2 Error row: `record_error` (wb_proxy.py:945-1000)

Fields: `at`, `iso`, `model`, `error: true`, `outcome` (`failed` default,
`upstream_aborted` for mid-stream aborts), `status` (HTTP int), `message`
(first 200 chars), `elapsed_ms`, optional `stream`, `ttft_ms`, `gen_ms`,
usage fields from `_extract_usage(usage)` when partial usage exists,
fingerprint fields when `fp` given (only mid-stream errors pass it), `account`
and `realm` (only when an account is known), `key` (always, `""` if none).
No `cost_policy`, `reasoning_effort`, `tokens_per_sec`, `cache_hit_pct`.

Call sites on this path and the model they log: per-key 429 (`payload.model or "unknown"`,
wb_proxy.py:22198), open failures (`model` = request model or `hy4-preview`,
22285-22333), mid-stream abort (22528-22532), stream finished with error
(22562-22566), non-stream aggregation failure (22585-22586).

**[quirk]** On `client_aborted`, a usage row (not an error row) is written
with `outcome="client_aborted"`; usage may be partial or missing.

---

## 6. `GET /v1/models` (and `/models`)

Handler `_get_v1_models` (wb_proxy.py:18800-18853), routed at 18667-18668.
Requires the same auth as chat (`_authorized`).

### 6.1 Catalogue source

`fetch_models(realm)` (wb_proxy.py:10861-10902), realm =
`_request_realm()` (key-bound realm, then `X-Realm` header, then `?realm=`
query; the header/query value is not validated) or the current realm
(wb_proxy.py:18515-18533). Cached per realm for 300 s in `_models_cache`
(wb_proxy.py:514), single-flight via `_catalog_lock`.

Live source precedence (`curated_live_sources`, wb_proxy.py:11449-11481):

1. remote `GET <realm chat_upstream>/v3/config` with an account's headers,
   10 s timeout (wb_proxy.py:11066, ~11239-11262); also ingests the tier map
   (`wb_tiers`);
2. the desktop app's cached product config
   (`~/.workbuddy-ai/cache/acc-product-config-v3.json` for intl,
   `~/.workbuddy/cache/...` for cn), parsed the same way;
3. "legacy" loose parse of that same file;
4. intl only, if all above are empty: `GET https://www.workbuddy.ai/v2/enterprises/personal/models`
   (`fetch_endpoint_models`, wb_proxy.py:11484-11504).

`curate_remote_catalog` (wb_proxy.py:11196-~11235) drops virtual aliases
(`default-model`, `fast-model`, `balanced-model`, ... wb_proxy.py:10661),
non-chat models (`lite`, prefixes `codewise-`/`completion-`, suffixes
`-image-alpha`, `-image-alpha-edit`, `-taco-completion`; wb_proxy.py:10671-10683),
`-sg`/`-x` paid builds and paid siblings of free (`x0.00`) models.

`merge_catalog(live, realm, extras)` (wb_proxy.py:10738-10791): start from the
bundled snapshot `wb_catalog.STATIC_INTL_MODELS` / `STATIC_CN_MODELS`
(wb_catalog.py:18-1731, JSON text parsed at import), overlay live metadata
(reasoning blocks merged), order by `INTL_UI_ORDER` / `CN_UI_ORDER`
(wb_proxy.py:~10684+); with `extras` (remote/cache sources) append live-only
models that carry a non-empty `credits`. Result: ordered `[(id, meta)]`.
Provenance stored in `_catalog_meta[realm] = {source, fetched_at, file_mtime}`,
`source` in `remote|endpoint|cache|legacy|bundled`.

The same cached entries feed `model_catalog_meta` (T3, T17, T18) without ever
triggering a fetch.

### 6.2 Filters, in order

1. Pinned route `/u/<ref>/v1/models`: drop models the pinned account has
   disabled (`account.model_disabled`, wb_accounts.py:1591).
2. Settings `filter_models_by_key` (default **off**, wb_settings.py:1726-1735)
   and a key was used: keep only `key_allows_model` (fnmatch-case on the
   lowercased id against the key's `models` patterns; empty list = all;
   wb_settings.py:428-444).
3. Settings `models_only_available` (default **off**, wb_settings.py:1769-1777):
   keep models with at least one enabled credentialed account in the realm;
   PRO-only models need a paid account (`models_available_for`, wb_proxy.py:10840-10858).
4. Annotation (not a filter): PRO-gated models (`wb_tiers.requires_pro`) get
   `"entitlement": {"source":"catalog","required":"standard","at": <tier ingest time>}`.

**Model aliases (T1) are not advertised** in `/v1/models`; alias names only
work on the request path.

### 6.3 Response shape

```json
{"object": "list",
 "data": [ <model_entry>... ],
 "realm": "intl|cn",
 "catalog": {"source": "...", "fetched_at": <float>, "file_mtime": <float|null>}}
```

`catalog` only when provenance exists. No ETag on this route (plain `_json`).
Fetch exception -> 502 `{"error":{"message": "<exc>", "type":"server_error","code":502}}`.

`model_entry(mid, meta)` (wb_proxy.py:10903-11017) always emits `id`,
`object:"model"`, `created` (now), `owned_by:"workbuddy"`, and the capability
flags in every common spelling: `capabilities{vision,tool_calls,reasoning}`,
`supports_vision`, `supports_images`, `supports_tool_calls`,
`supports_reasoning`, `vision`, `multimodal`, `abilities{vision,functionCall,function_call,reasoning}`,
`input_modalities`, `output_modalities:["text"]`, `modalities{input,output}`,
`architecture{input_modalities,output_modalities,modality:"text(+image)->text"}`,
`context_length` and `max_input_tokens` (from `wb_modelsdev.lookup`, 1M when
unknown). Optional: `name`, `description` (`descriptionEn` then `descriptionZh`),
`max_output_tokens` / `max_completion_tokens`, `output_clamp` /
`max_output_tokens_clamped` (`wb_probes`), `context_windows`,
`reasoning_efforts`, `reasoning_fixed_effort`, `reasoning_default_effort`,
`reasoning_can_disable`, `always_reasoning`, `credits`, `vendor`,
`temperature`, `top_p`, `is_default`, `tags` (minus `badge:*`).
Special case: `glm-5.3-flash` is forced to `vision=false` with a `vision_note`.
Vision = `supportsImages && !disabledMultimodal`.

`wb_modelsdev.refresh_async` is kicked off (best effort) on each call
(wb_proxy.py:18808-18813).

---

## 7. Open questions for the Rust port

1. `/v1/completions` has no legacy-prompt translation (0.3). Keep or implement?
2. DSML opening tags are not recognised by the stream filter (3.5). Fix (recommended) or keep parity?
3. Sanitizer literal rewrites and `.strip()` also touch user/tool text when the gate fires (T7). Keep?
4. Non-dict `messages[0]` or string `messages` crash `build_upstream_body` with no response (T9). Return 400?
5. Non-object JSON bodies become `{}` and are forwarded (2.1). Return 400?
6. `elapsed_ms` / `first_chunk_at` leak into non-stream responses (4.3). Drop?
7. Stream path forwards a choices-less, usage-less upstream frame and non-JSON upstream lines (3.3). Filter?
8. `stream` uses Python truthiness (0.2). Match exactly or accept only booleans?
9. Synthesised tool-call indices restart at 0 per batch and can collide with upstream structured calls (3.5).
